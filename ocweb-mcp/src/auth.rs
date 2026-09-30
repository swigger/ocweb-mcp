use anyhow::{Context, Result, bail, ensure};
use axum::extract::ws::{Message, WebSocket};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

pub const PROTOCOL: &str = "openclaw-extension-relay.v2";

pub fn random_id(size: usize) -> String {
    let mut bytes = vec![0; size];
    rand::rng().fill_bytes(&mut bytes);
    B64.encode(bytes)
}

#[derive(Clone)]
pub struct Credentials {
    pub relay_key: Vec<u8>,
    pub api_token: String,
    pub instance_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredentials {
    relay_key: String,
    api_token: String,
}

impl Credentials {
    pub fn load(dir: &Path) -> Result<Self> {
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(dir)?;
        let path = dir.join("credentials.json");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let stored = match options.open(&path) {
            Ok(mut file) => {
                let mut key = [0u8; 32];
                rand::rng().fill_bytes(&mut key);
                let stored = StoredCredentials {
                    relay_key: hex::encode(key),
                    api_token: random_id(32),
                };
                file.write_all(&serde_json::to_vec(&stored)?)?;
                file.sync_all()?;
                stored
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                    "credentials must not be a symlink"
                );
                let mut file = fs::File::open(&path)?;
                ensure!(
                    file.metadata()?.is_file(),
                    "credentials must be a regular file"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    ensure!(
                        file.metadata()?.permissions().mode() & 0o077 == 0,
                        "credentials must be private: chmod 600 {}",
                        path.display()
                    );
                }
                let mut raw = String::new();
                (&mut file).take(4097).read_to_string(&mut raw)?;
                ensure!(raw.len() <= 4096, "credentials file too large");
                serde_json::from_str(&raw)
                    .context("invalid credentials file (another instance may be initializing it)")?
            }
            Err(e) => return Err(e.into()),
        };
        let relay_key = hex::decode(&stored.relay_key)?;
        ensure!(
            relay_key.len() == 32 && B64.decode(&stored.api_token)?.len() == 32,
            "invalid credential length"
        );
        Ok(Self {
            relay_key,
            api_token: stored.api_token,
            instance_id: random_id(16),
        })
    }

    pub fn key_id(&self) -> String {
        B64.encode(Sha256::digest(&self.relay_key))[..22].to_string()
    }
    pub fn pairing(&self, port: u16) -> String {
        format!(
            "ws://127.0.0.1:{port}/extension#{}",
            hex::encode(&self.relay_key)
        )
    }
    pub fn api_authorized(&self, header: &str) -> bool {
        header
            .strip_prefix("Bearer ")
            .is_some_and(|s| bool::from(s.as_bytes().ct_eq(self.api_token.as_bytes())))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Hello {
    r#type: String,
    v: u8,
    key_id: String,
    client_nonce: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Response {
    r#type: String,
    v: u8,
    session_id: String,
    client_proof: String,
}

fn proof(key: &[u8], kind: &str, fields: &Value, client_proof: Option<&str>) -> Vec<u8> {
    let mut values = vec![json!("openclaw.browser-relay.auth"), json!(2), json!(kind)];
    for field in [
        "keyId",
        "instanceId",
        "sessionId",
        "clientNonce",
        "serverNonce",
        "issuedAtMs",
        "expiresAtMs",
        "role",
        "transport",
        "method",
        "resource",
        "flow",
    ] {
        values.push(fields[field].clone());
    }
    if let Some(p) = client_proof {
        values.push(json!(p));
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key size");
    mac.update(&serde_json::to_vec(&values).expect("JSON values serialize"));
    mac.finalize().into_bytes().to_vec()
}

async fn read_text(socket: &mut WebSocket) -> Result<String> {
    match socket.recv().await.context("extension disconnected")?? {
        Message::Text(text) if text.len() <= 16_384 => Ok(text.to_string()),
        _ => bail!("expected a bounded authentication text frame"),
    }
}

pub async fn authenticate(socket: &mut WebSocket, credentials: &Credentials) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let hello: Hello = serde_json::from_str(&read_text(socket).await?)?;
        ensure!(hello.r#type == "auth.hello" && hello.v == 2 && hello.key_id == credentials.key_id(), "invalid authentication hello");
        let nonce = B64.decode(&hello.client_nonce)?;
        ensure!(nonce.len() == 32 && B64.encode(&nonce) == hello.client_nonce, "invalid client nonce");
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
        let session_id = random_id(16);
        let mut fields = json!({"type":"auth.challenge","v":2,"keyId":credentials.key_id(),"instanceId":credentials.instance_id,"sessionId":session_id,"clientNonce":hello.client_nonce,"serverNonce":random_id(32),"issuedAtMs":now,"expiresAtMs":now + 10_000,"role":"extension","transport":"websocket","method":"GET","resource":"/extension","flow":"extension"});
        fields["serverProof"] = json!(B64.encode(proof(&credentials.relay_key, "server", &fields, None)));
        socket.send(Message::Text(fields.to_string().into())).await?;
        let response: Response = serde_json::from_str(&read_text(socket).await?)?;
        ensure!(response.r#type == "auth.response" && response.v == 2 && response.session_id == session_id, "invalid auth response");
        let supplied = B64.decode(&response.client_proof)?;
        let expected = proof(&credentials.relay_key, "client", &fields, None);
        ensure!(bool::from(supplied.ct_eq(&expected)), "invalid client proof");
        let accept = B64.encode(proof(&credentials.relay_key, "accept", &fields, Some(&response.client_proof)));
        socket.send(Message::Text(json!({"type":"auth.ok","v":2,"sessionId":session_id,"acceptProof":accept}).to_string().into())).await?;
        Ok(())
    }).await.context("authentication timed out")?
}
