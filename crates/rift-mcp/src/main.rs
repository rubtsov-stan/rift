//! `rift-mcp`: the MCP server on standard input and output.
#![forbid(unsafe_code)]

use anyhow::Context;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let service = rift_mcp::Server
        .serve(stdio())
        .await
        .context("start the server")?;
    service.waiting().await.context("serve")?;
    Ok(())
}
