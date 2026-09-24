use anyhow::Result;
use clap::Parser;

use amux::cli::{Cli, Command, DebugAction, ServersAction};
use amux::client::{self, Endpoint};
use amux::cluster::ssh;
use amux::config::ServerConfig;
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
        Some(Command::List) => client::list_cluster(&endpoint).await,
        Some(Command::Servers(args)) => match args.action {
            None => client::list_servers(&endpoint).await,
            Some(ServersAction::Add(args)) => {
                let server = ServerConfig {
                    address: args.address,
                    amux_path: args.amux_path,
                    socket: args.socket,
                };
                client::add_server(&endpoint, args.name, server).await
            }
            Some(ServersAction::Remove { name }) => client::remove_server(&endpoint, name).await,
        },
        Some(Command::KillServer) => client::kill_server(&endpoint).await,
        Some(Command::Server) => server::run(&endpoint.socket, endpoint.config.as_deref()).await,
        Some(Command::Bridge(args)) => ssh::bridge(&endpoint, args.no_start).await,
        Some(Command::Debug(DebugAction::Links)) => client::debug_links(&endpoint).await,
        Some(Command::Debug(DebugAction::DropLink { peer })) => {
            client::drop_link(&endpoint, peer).await
        }
    }
}
