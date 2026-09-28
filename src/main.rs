use anyhow::Result;
use clap::Parser;

use amux::cli::{Cli, Command, DebugAction, ProjectAction, ServersAction};
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
        Some(Command::New(args)) => client::new_session(&endpoint, args).await,
        Some(Command::Attach(args)) => client::attach_session(&endpoint, args.target).await,
        Some(Command::Kill(args)) => {
            client::kill_session(&endpoint, args.target, args.remove_worktree).await
        }
        Some(Command::Rename(args)) => {
            client::rename_session(&endpoint, args.target, args.name).await
        }
        Some(Command::List(args)) => client::list_cluster(&endpoint, args.by).await,
        Some(Command::Projects) => client::list_projects(&endpoint).await,
        Some(Command::Project(ProjectAction::Add { path })) => {
            client::add_project(&endpoint, path).await
        }
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
            Some(ServersAction::Forget { server }) => {
                client::forget_server(&endpoint, server).await
            }
        },
        Some(Command::Discover) => client::discover(&endpoint).await,
        Some(Command::Pair(args)) => {
            client::pair(&endpoint, args.code, args.host, args.new_key).await
        }
        Some(Command::KillServer) => client::kill_server(&endpoint).await,
        Some(Command::Server) => server::run(&endpoint.socket, endpoint.config.as_deref()).await,
        Some(Command::Bridge(args)) => ssh::bridge(&endpoint, args.no_start).await,
        Some(Command::Debug(DebugAction::Links)) => client::debug_links(&endpoint).await,
        Some(Command::Debug(DebugAction::DropLink { peer })) => {
            client::drop_link(&endpoint, peer).await
        }
    }
}
