mod cli;
mod client;
mod paths;
mod protocol;
mod server;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command, NewArgs};

#[tokio::main]
async fn main() -> Result<()> {
    let Cli {
        socket_name,
        socket_path,
        command,
    } = Cli::parse();
    let socket = match socket_path {
        Some(path) => path,
        None => paths::default_socket(&socket_name)?,
    };

    match command.unwrap_or_else(|| Command::New(NewArgs::default())) {
        Command::New(args) => client::new_session(&socket, args.name).await,
        Command::Attach(args) => client::attach_session(&socket, args.target).await,
        Command::List => client::list_sessions(&socket).await,
        Command::KillServer => client::kill_server(&socket).await,
        Command::Server => server::run(&socket).await,
    }
}
