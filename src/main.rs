use std::{net::SocketAddr, path::PathBuf};

use tatara::{mcp, server};

const USAGE: &str = "Tatara — an AI-native 3D editor

Usage:
  tatara          Start the editor at http://127.0.0.1:3000
  tatara --mcp    Run the stdio MCP bridge to a running editor

Environment:
  TATARA_PORT         Editor port (default 3000)
  TATARA_WEB_DIR      Built web assets (default web/dist)
  TATARA_URL          Editor URL used by --mcp (default http://127.0.0.1:3000)
  TATARA_AI_BASE_URL  OpenAI-compatible endpoint for in-editor chat (optional)
  TATARA_AI_MODEL     Model name for in-editor chat
  TATARA_AI_API_KEY   API key for the chat provider (optional)";

fn web_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TATARA_WEB_DIR") {
        return dir.into();
    }
    let local = PathBuf::from("web/dist");
    if local.join("index.html").exists() {
        return local;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/dist")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("--mcp") => {
            let url =
                std::env::var("TATARA_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
            return mcp::run(url.trim_end_matches('/').into()).await;
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some(other) => {
            eprintln!("unknown argument: {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
    let port: u16 = match std::env::var("TATARA_PORT") {
        Ok(p) => p
            .parse()
            .map_err(|_| anyhow::anyhow!("TATARA_PORT must be a port number"))?,
        Err(_) => 3000,
    };
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    server::serve(addr, web_dir(), server::AiConfig::from_env()).await
}
