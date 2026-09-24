use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::paths;

#[derive(Debug, Parser)]
#[command(name = "amux", version, about = "A terminal multiplexer")]
pub struct Cli {
    #[arg(
        short = 'L',
        long,
        global = true,
        default_value = "default",
        help = "Name of the server socket inside the runtime directory"
    )]
    pub socket_name: String,

    #[arg(
        short = 'S',
        long,
        global = true,
        conflicts_with = "socket_name",
        help = "Full path of the server socket"
    )]
    pub socket_path: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        env = "AMUX_CONFIG",
        value_name = "PATH",
        help = "Config file, defaults to $XDG_CONFIG_HOME/amux/config.toml"
    )]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn socket(&self) -> Result<PathBuf> {
        match &self.socket_path {
            Some(path) => Ok(path.clone()),
            None => paths::default_socket(&self.socket_name),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(
        visible_alias = "new-session",
        about = "Create a new session and attach to it"
    )]
    New(NewArgs),

    #[command(
        visible_aliases = ["a", "attach-session"],
        about = "Attach to an existing session"
    )]
    Attach(AttachArgs),

    #[command(
        visible_aliases = ["ls", "list-sessions"],
        about = "List the sessions on every server in the cluster"
    )]
    List,

    #[command(about = "List the servers in the cluster, or add and remove them")]
    Servers(ServersArgs),

    #[command(about = "Stop the server and every session it owns")]
    KillServer,

    #[command(hide = true)]
    Server,

    #[command(
        hide = true,
        about = "Pipe stdin and stdout to the server, starting it if needed"
    )]
    Bridge(BridgeArgs),

    #[command(hide = true, subcommand)]
    Debug(DebugAction),
}

#[derive(Debug, Default, Args)]
pub struct NewArgs {
    #[arg(short = 's', long = "session-name", help = "Name for the new session")]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct AttachArgs {
    #[arg(
        short = 't',
        long = "target",
        help = "Session to attach to, defaults to the most recent one"
    )]
    pub target: Option<String>,
}

#[derive(Debug, Args)]
pub struct ServersArgs {
    #[command(subcommand)]
    pub action: Option<ServersAction>,
}

#[derive(Debug, Subcommand)]
pub enum ServersAction {
    #[command(about = "Add a server to the config and link to it")]
    Add(AddServerArgs),

    #[command(visible_alias = "rm", about = "Remove a server from the config")]
    Remove {
        #[arg(help = "Name of the server in the config")]
        name: String,
    },
}

#[derive(Debug, Args)]
pub struct AddServerArgs {
    #[arg(help = "Name for the server")]
    pub name: String,

    #[arg(help = "ssh://[user@]host[:port] or exec:<command>")]
    pub address: String,

    #[arg(
        long,
        help = "Path of amux on that machine, defaults to amux on its PATH"
    )]
    pub amux_path: Option<String>,

    #[arg(
        long,
        help = "Socket name of the server on that machine, defaults to this server's"
    )]
    pub socket: Option<String>,
}

#[derive(Debug, Args)]
pub struct BridgeArgs {
    #[arg(
        long,
        help = "Fail instead of starting the server when it is not running"
    )]
    pub no_start: bool,
}

#[derive(Debug, Subcommand)]
pub enum DebugAction {
    #[command(about = "List the live peer links")]
    Links,

    #[command(about = "Drop the link to a peer, which then reconnects")]
    DropLink {
        #[arg(help = "Name or id of the peer")]
        peer: String,
    },
}
