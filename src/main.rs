use log::{error, info};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead},
    process::Stdio,
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command as AsyncCommand},
};

use std::num::ParseIntError;

#[derive(Error, Debug)]
enum LspError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("LSP protocol error: {0}")]
    Protocol(String),
    #[error("Parse error: {0}")]
    ParseInt(#[from] ParseIntError),
}

struct LspConnection {
    child: Child,
}

impl LspConnection {
    async fn new(server_cmd: &str) -> Result<Self, LspError> {
        info!("Starting LSP server: {}", server_cmd);
        let child = AsyncCommand::new(server_cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        Ok(Self { child })
    }

    async fn initialize(&mut self) -> Result<Value, LspError> {
        info!("Initializing LSP connection");
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "clientInfo": {
                    "name": "vim-rust-lsp-bridge",
                    "version": "0.1.0"
                },
                "rootUri": std::env::current_dir()?
                    .to_str()
                    .ok_or_else(|| LspError::Protocol("Invalid current directory".into()))?,
                "capabilities": {},
                "trace": "verbose",
            }
        });

        self.send_message(&request.to_string()).await?;
        self.read_response().await
    }

    async fn send_message(&mut self, content: &str) -> Result<(), LspError> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| LspError::Protocol("Failed to get stdin".into()))?;

        let message = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);
        stdin.write_all(message.as_bytes()).await?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<Value, LspError> {
        let stdout = self
            .child
            .stdout
            .as_mut()
            .ok_or_else(|| LspError::Protocol("Failed to get stdout".into()))?;

        let mut reader = tokio::io::BufReader::new(stdout);
        let mut content_length = None;

        // Read headers
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await?;

            if line.is_empty() {
                continue;
            }

            if line.starts_with("Content-Length:") {
                content_length = Some(
                    line[16..]
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| LspError::Protocol("Invalid Content-Length".into()))?,
                );
            } else if line == "\r\n" {
                break;
            }
        }

        // Read content
        let content_length =
            content_length.ok_or_else(|| LspError::Protocol("Missing Content-Length".into()))?;
        let mut content = vec![0; content_length];
        reader.read_exact(&mut content).await?;

        serde_json::from_slice(&content).map_err(Into::into)
    }

    async fn handle_vim_command(&mut self, command: &str) -> Result<(), LspError> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return Ok(());
        }

        match parts[0] {
            "DEFINITION" if parts.len() == 4 => {
                self.goto_definition(parts[1], parts[2], parts[3]).await
            }
            "HOVER" if parts.len() == 3 => self.hover(parts[1], parts[2], parts[3]).await,
            "COMPLETION" if parts.len() == 3 => self.completion(parts[1], parts[2], parts[3]).await,
            _ => {
                error!("Unknown command: {}", command);
                Ok(())
            }
        }
    }

    async fn goto_definition(&mut self, uri: &str, line: &str, col: &str) -> Result<(), LspError> {
        let line_num = line.parse::<u32>()?;
        let col_num = col.parse::<u32>()?;

        let request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": uri },
                "position": {
                    "line": line_num,
                    "character": col_num,
                }
            }
        });

        self.send_message(&request.to_string()).await?;
        let response = self.read_response().await?;
        println!("DEFINITION_RESPONSE: {}", response);
        Ok(())
    }

    async fn hover(&mut self, uri: &str, line: &str, col: &str) -> Result<(), LspError> {
        let line_num = line.parse::<u32>()?;
        let col_num = col.parse::<u32>()?;

        let request = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": {
                    "line": line_num,
                    "character": col_num,
                }
            }
        });

        self.send_message(&request.to_string()).await?;
        let response = self.read_response().await?;
        println!("HOVER_RESPONSE: {}", response);
        Ok(())
    }

    async fn completion(&mut self, uri: &str, line: &str, col: &str) -> Result<(), LspError> {
        let line_num = line.parse::<u32>()?;
        let col_num = col.parse::<u32>()?;

        let request = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": {
                    "line": line_num,
                    "character": col_num,
                }
            }
        });

        self.send_message(&request.to_string()).await?;
        let response = self.read_response().await?;
        println!("COMPLETION_RESPONSE: {}", response);
        Ok(())
    }
}

async fn run_lsp_bridge() -> Result<(), LspError> {
    let mut lsp = LspConnection::new("rust-analyzer").await?;
    lsp.initialize().await?;
    info!("LSP connection initialized");

    // Main event loop - read from stdin
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        lsp.handle_vim_command(&line).await?;
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    env_logger::init();

    if let Err(e) = run_lsp_bridge().await {
        error!("Error in LSP bridge: {}", e);
        std::process::exit(1);
    }
}
