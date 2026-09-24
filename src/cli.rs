use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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

    #[command(subcommand)]
    pub command: Option<Command>,
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

    #[command(visible_aliases = ["ls", "list-sessions"], about = "List sessions")]
    List,

    #[command(about = "Stop the server and every session it owns")]
    KillServer,

    #[command(hide = true)]
    Server,
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
