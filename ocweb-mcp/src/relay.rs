use crate::auth::{Credentials, authenticate};
use anyhow::{Context, Result, bail, ensure};
use axum::extract::ws::{Message, WebSocket};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct BrowserError {
    pub code: String,
    pub message: String,
}
impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for BrowserError {}

pub fn error_json(error: &anyhow::Error) -> Value {
    let code = error
        .downcast_ref::<BrowserError>()
        .map(|e| e.code.as_str())
        .unwrap_or("BROWSER_ERROR");
    let message = format!("{error:#}");
    // Older extension frames collapse document changes and access changes into
    // the same text. That text is neither revocation evidence nor retry authority.
    let message = if code == "BROWSER_ERROR" && message.contains("access was revoked") {
        "浏览器扩展返回了无法分类的访问状态错误，无法判断是页面变化还是权限变化，未自动重试。请在 chrome://extensions 重新加载 OCWeb 扩展后重试；若仍出现，请检查扩展版本。".to_owned()
    } else {
        message
    };
    json!({"error": message, "code": code})
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tab {
    pub tab_id: u64,
    pub url: String,
    pub title: String,
    pub active: bool,
}

type Reply = oneshot::Sender<Result<Value>>;
struct Command {
    seq: u64,
    body: Value,
    reply: Reply,
}
struct Connection {
    generation: u64,
    sender: mpsc::Sender<Command>,
    tabs: Vec<Tab>,
}

#[derive(Clone)]
pub struct Relay {
    connection: Arc<Mutex<Option<Connection>>>,
    next: Arc<AtomicU64>,
    pub connected: watch::Sender<bool>,
    pub events: broadcast::Sender<(u64, Value)>,
    pub credentials: Arc<Credentials>,
    pub shutdown: CancellationToken,
    port: u16,
}

impl Relay {
    pub fn new(credentials: Credentials, port: u16) -> Self {
        Self {
            connection: Default::default(),
            next: Arc::new(AtomicU64::new(1)),
            connected: watch::channel(false).0,
            events: broadcast::channel(1024).0,
            credentials: Arc::new(credentials),
            shutdown: CancellationToken::new(),
            port,
        }
    }
    pub async fn tabs(&self) -> Result<Vec<Tab>> {
        self.connection
            .lock()
            .await
            .as_ref()
            .map(|c| c.tabs.clone())
            .context(
                "Browser not connected. Open /docs and follow the extension pairing instructions.",
            )
    }
    pub async fn generation(&self) -> Result<u64> {
        self.connection
            .lock()
            .await
            .as_ref()
            .map(|c| c.generation)
            .context("Browser disconnected")
    }
    // A multi-command action pins its connection. Never replay writes into a replacement browser.
    pub async fn command(&self, generation: u64, mut body: Value) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        let seq = self.next.fetch_add(1, Ordering::Relaxed);
        body["seq"] = json!(seq);
        let sender = {
            let guard = self.connection.lock().await;
            let c = guard.as_ref().context("Browser disconnected")?;
            ensure!(
                c.generation == generation,
                "Browser connection changed; take a fresh snapshot before retrying"
            );
            c.sender.clone()
        };
        sender
            .try_send(Command {
                seq,
                body,
                reply: tx,
            })
            .context("Browser command queue unavailable or full")?;
        tokio::time::timeout(Duration::from_secs(30), rx).await.context("Browser command timed out; outcome may be unknown. Inspect the page before retrying.")?.context("Browser disconnected during command; outcome may be unknown")?
    }
    pub async fn cdp(
        &self,
        generation: u64,
        tab_id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.command(
            generation,
            json!({"type":"cdp","tabId":tab_id,"method":method,"params":params}),
        )
        .await
    }
    pub async fn serve_extension(&self, mut socket: WebSocket) -> Result<()> {
        authenticate(&mut socket, &self.credentials).await?;
        let hello = tokio::time::timeout(Duration::from_secs(10), socket.recv())
            .await?
            .context("extension disconnected before hello")??;
        let Message::Text(text) = hello else {
            bail!("expected extension hello")
        };
        let hello: Value = serde_json::from_str(&text)?;
        ensure!(hello["type"] == "hello", "expected extension hello");
        let tabs: Vec<Tab> = serde_json::from_value(hello["tabs"].clone())?;
        let generation = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, mut commands) = mpsc::channel::<Command>(64);
        {
            let mut guard = self.connection.lock().await;
            // Do not steal a live extension's tabs or race debugger teardown.
            ensure!(
                guard.is_none(),
                "another browser extension is already connected"
            );
            *guard = Some(Connection {
                generation,
                sender,
                tabs,
            });
        }
        self.connected.send_replace(true);
        eprintln!(
            "\n浏览器已连接。MCP: http://127.0.0.1:{}/mcp\nAPI 文档: http://127.0.0.1:{}/docs\nAI 配置: 添加 Streamable HTTP MCP，URL 使用上面的 /mcp，Authorization 请求头设为 Bearer <credentials.json 中的 api_token>。\n",
            self.port, self.port
        );
        let mut pending = HashMap::<u64, Reply>::new();
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        let mut last_seen = tokio::time::Instant::now();
        let outcome: Result<()> = async {
            loop {
                tokio::select! {
                    _ = self.shutdown.cancelled() => break,
                    _ = heartbeat.tick() => {
                        ensure!(last_seen.elapsed() < Duration::from_secs(60), "extension heartbeat expired");
                        pending.retain(|_, reply| !reply.is_closed());
                        socket.send(Message::Text(json!({"type":"ping"}).to_string().into())).await?;
                    }
                    Some(command) = commands.recv() => {
                        if command.reply.is_closed() { continue; }
                        pending.insert(command.seq, command.reply);
                        socket.send(Message::Text(command.body.to_string().into())).await?;
                    }
                    frame = socket.recv() => {
                        let Some(frame) = frame else { break };
                        match frame? {
                            Message::Text(text) => {
                                last_seen = tokio::time::Instant::now();
                                let frame: Value = serde_json::from_str(&text)?;
                                match frame["type"].as_str() {
                                    Some("tabs") => {
                                        let tabs: Vec<Tab> = serde_json::from_value(frame["tabs"].clone())?;
                                        if let Some(c) = self.connection.lock().await.as_mut().filter(|c| c.generation == generation) { c.tabs = tabs; }
                                    }
                                    Some("result" | "error") => {
                                        if let Some(reply) = frame["seq"].as_u64().and_then(|seq| pending.remove(&seq)) {
                                            let result = if frame["type"] == "error" { Err(BrowserError { code: frame["code"].as_str().unwrap_or("BROWSER_ERROR").to_owned(), message: frame["message"].as_str().unwrap_or("extension error").to_owned() }.into()) } else { Ok(frame.get("result").cloned().unwrap_or(json!({}))) };
                                            let _ = reply.send(result);
                                        }
                                    }
                                    Some("cdpEvent" | "detached") => { let _ = self.events.send((generation, frame)); },
                                    Some("pong") => {},
                                    _ => bail!("unexpected extension frame"),
                                }
                            }
                            Message::Close(_) => break,
                            Message::Ping(v) => socket.send(Message::Pong(v)).await?,
                            Message::Pong(_) => last_seen = tokio::time::Instant::now(),
                            _ => bail!("unsupported extension frame"),
                        }
                    }
                }
            }
            Ok(())
        }.await;
        let _ = socket.send(Message::Close(None)).await;
        for (_, reply) in pending {
            let _ = reply.send(Err(anyhow::anyhow!(
                "Browser disconnected; command outcome may be unknown"
            )));
        }
        {
            let mut guard = self.connection.lock().await;
            if guard.as_ref().is_some_and(|c| c.generation == generation) {
                *guard = None;
            }
        }
        self.connected.send_replace(false);
        eprintln!(
            "浏览器连接已断开；服务继续等待扩展重连。帮助: http://127.0.0.1:{}/docs",
            self.port
        );
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_access_error_does_not_claim_revocation() {
        // Observed extension frame lacked a code; the receive adapter defaults it.
        let error = anyhow::Error::new(BrowserError {
            code: "BROWSER_ERROR".into(),
            message: "tab 202624569 access was revoked".into(),
        });
        let response = error_json(&error);
        assert_eq!(response["code"], "BROWSER_ERROR");
        let message = response["error"].as_str().unwrap();
        assert!(
            !message.contains("revoked"),
            "unknown authority must not be reported as revoked: {message}"
        );
        assert!(message.contains("chrome://extensions"));
    }

    #[test]
    fn classified_revocation_and_document_change_remain_distinct() {
        for (code, message) in [
            ("ACCESS_REVOKED", "tab 7 access was revoked: paused"),
            (
                "DOCUMENT_CHANGED",
                "safe_retry exhausted after 5 attempts; page keeps changing",
            ),
            ("BROWSER_ERROR", "JavaScript exception: fixture failure"),
        ] {
            let error = anyhow::Error::new(BrowserError {
                code: code.into(),
                message: message.into(),
            });
            assert_eq!(error_json(&error), json!({"code":code,"error":message}));
        }
    }
}
