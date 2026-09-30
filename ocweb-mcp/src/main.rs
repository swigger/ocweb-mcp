mod auth;
mod browser;
mod mcp;
mod relay;
mod server;

use crate::{auth::Credentials, browser::Browser, relay::Relay};
use anyhow::{Context, Result};
use clap::Parser;
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    version,
    about = "Standalone Chrome browser automation: REST API + Streamable HTTP MCP"
)]
struct Args {
    /// Loopback port shared by the extension relay, REST, MCP, and docs.
    #[arg(long, default_value_t = 18799)]
    port: u16,
    /// Directory for private credentials. Defaults to ~/.ocweb-mcp.
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Seconds to wait for a paired extension before printing setup instructions.
    #[arg(long, default_value_t = 5)]
    connect_timeout: u64,
    /// Unpacked Chrome extension directory, for setup guidance.
    #[arg(long)]
    extension_dir: Option<PathBuf>,
    /// Print the pairing string and API token for explicit local setup.
    #[arg(long)]
    show_credentials: bool,
    /// Print only the extension pairing string and exit (works while the server is running).
    #[arg(long, conflicts_with = "show_credentials")]
    print_pairing: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let state_dir = args.state_dir.unwrap_or_else(|| {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ocweb-mcp")
    });
    let credentials = Credentials::load(&state_dir)?;
    if args.print_pairing {
        anyhow::ensure!(
            args.port != 0,
            "--print-pairing needs the running server's actual --port"
        );
        println!("{}", credentials.pairing(args.port));
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, args.port)).await.context("Cannot bind loopback port. Stop the existing instance or select --port <free-port>; update extension pairing when changing ports.")?;
    let port = listener.local_addr()?.port();
    let extension = args
        .extension_dir
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../chrome-extension"));
    let extension = extension.canonicalize().unwrap_or(extension);
    if args.show_credentials {
        eprintln!(
            "配对字符串（仅粘贴到扩展设置）: {}\nAPI/MCP Bearer token: {}",
            credentials.pairing(port),
            credentials.api_token
        );
    }
    let relay = Relay::new(credentials, port);
    let browser = Browser::new(relay.clone());
    let router = server::router(browser, port);
    let credential_path = state_dir.join("credentials.json");
    eprintln!(
        "OCWeb 正在监听 http://127.0.0.1:{port}，等待浏览器扩展握手…\nAPI 文档: http://127.0.0.1:{port}/docs\n凭据文件: {}",
        credential_path.display()
    );
    let mut connected = relay.connected.subscribe();
    let shutdown = relay.shutdown.clone();
    let guide = tokio::spawn(async move {
        let wait = async {
            loop {
                if *connected.borrow_and_update() {
                    return;
                }
                if connected.changed().await.is_err() {
                    return;
                }
            }
        };
        if tokio::time::timeout(Duration::from_secs(args.connect_timeout), wait)
            .await
            .is_err()
        {
            eprintln!(
                "\n尚未连接浏览器，服务会继续等待。请按以下步骤操作：\n1. 打开 Chrome 的 chrome://extensions，开启开发者模式。\n2. 点击“加载已解压的扩展程序”，选择 {}\n3. 打开扩展 Settings，在 Connect to OCWeb 中粘贴配对字符串。\n   格式：ws://127.0.0.1:{port}/extension#<credentials.json 中的 relay_key>\n   运行 ocweb-mcp --print-pairing，或在工程目录执行 cargo run -- --print-pairing。无需停止服务。\n4. 选择 Selected tabs 并把测试标签页加入 OpenClaw 标签组，或选择 All tabs。\n连接成功后终端会打印 MCP 接入地址。文档: http://127.0.0.1:{port}/docs\n",
                extension.display()
            );
        }
    });
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown.cancel();
        })
        .await;
    guide.abort();
    result?;
    Ok(())
}
