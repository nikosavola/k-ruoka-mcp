use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::net::{IpAddr, SocketAddr};

use k_ruoka_mcp::{login, mcp};

#[derive(Parser)]
#[command(name = "k-ruoka-mcp", about = "MCP server for a K-Ruoka shopping cart")]
struct Cli {
    /// Defaults to `serve`.
    ///
    /// Being an MCP server is the whole job, and clients spawn it by bare command --
    /// `{"command": "uvx", "args": ["k-ruoka-mcp"]}` passes no subcommand at all. A
    /// usage error there is unhelpful and hard to see, since the client only ever
    /// reads stdout. `login` stays explicit because it is the interactive one.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server (stdio by default).
    Serve(ServeArgs),
    /// Open a visible browser and wait while you sign in to K-Plussa by hand.
    Login {
        /// Chrome remote-debugging port, for reaching the browser over an SSH
        /// tunnel when the machine has no display.
        #[arg(long, default_value_t = 9222)]
        port: u16,
        /// Store to probe for a signed-in account while waiting.
        #[arg(long, default_value = login::DEFAULT_PROBE_STORE)]
        store_id: String,
    },
}

#[derive(Clone, Copy, Default, ValueEnum)]
enum Transport {
    #[default]
    Stdio,
    StreamableHttp,
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, value_enum, default_value = "stdio")]
    transport: Transport,
    /// HTTP listen address. Use 0.0.0.0 inside a container behind an auth proxy.
    #[arg(long, env = "K_RUOKA_HTTP_HOST", default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(long, env = "K_RUOKA_HTTP_PORT", default_value_t = 8080)]
    port: u16,
    /// Allowed HTTP Host authorities. Defaults to loopback hosts only.
    #[arg(long, env = "K_RUOKA_ALLOWED_HOSTS", value_delimiter = ',')]
    allowed_host: Vec<String>,
    /// Exact browser origins allowed on /mcp. All present Origins are rejected by default.
    #[arg(long, env = "K_RUOKA_ALLOWED_ORIGINS", value_delimiter = ',')]
    allowed_origin: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        None => mcp::serve().await,
        Some(Command::Serve(args)) => match args.transport {
            Transport::Stdio => mcp::serve().await,
            Transport::StreamableHttp => {
                let address = SocketAddr::new(args.host, args.port);
                let config = mcp::http::HttpConfig {
                    allowed_hosts: args.allowed_host,
                    allowed_origins: args.allowed_origin,
                };
                mcp::serve_http(address, config).await
            }
        },
        Some(Command::Login { port, store_id }) => login::run(port, &store_id).await,
    }
}
