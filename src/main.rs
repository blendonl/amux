use anyhow::Result;
use clap::Parser;

use amux::cli::{Cli, Command};
use amux::client::{self, Endpoint};
use amux::server;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let endpoint = Endpoint {
        socket: cli.socket()?,
        config: cli.config,
    };

    match cli.command {
        None => client::attach_or_create(&endpoint).await,
        Some(Command::New(args)) => client::new_session(&endpoint, args.name).await,
        Some(Command::Attach(args)) => client::attach_session(&endpoint, args.target).await,
        Some(Command::List) => client::list_sessions(&endpoint).await,
        Some(Command::KillServer) => client::kill_server(&endpoint).await,
        Some(Command::Server) => server::run(&endpoint.socket, endpoint.config.as_deref()).await,
    }
}
