//! MCP server that lets a client explore a Rift diff of two large files
//! through small, bounded answers.
//!
//! No tools yet: the server only identifies itself.
#![forbid(unsafe_code)]

use rmcp::ServerHandler;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};

/// The server state shared by every request.
#[derive(Debug, Clone, Default)]
pub struct Server;

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().build()).with_server_info(
            Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_names_itself() {
        let info = Server.get_info();
        assert_eq!(info.server_info.name, "rift-mcp");
    }
}
