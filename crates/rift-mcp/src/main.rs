//! `rift-mcp`: the MCP server on standard input and output.
#![forbid(unsafe_code)]

use std::time::Duration;

use anyhow::Context;
use rift_mcp::Server;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

/// How often idle diffs are looked for.
const REAP_EVERY: Duration = Duration::from_secs(30);

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let server = Server::default();
    let reaper = server.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(REAP_EVERY).await;
            reaper.sessions().evict_idle();
        }
    });
    let service = server.serve(stdio()).await.context("start the server")?;
    service.waiting().await.context("serve")?;
    Ok(())
}
