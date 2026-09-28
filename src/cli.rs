use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

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
        help = "Lua config file to use instead of $XDG_CONFIG_HOME/amux/init.lua or /etc/amux/init.lua; servers.lua is read from its directory"
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

    #[command(visible_alias = "kill-session", about = "Kill a session")]
    Kill(KillArgs),

    #[command(visible_alias = "rename-session", about = "Rename a session")]
    Rename(RenameArgs),

    #[command(
        visible_aliases = ["ls", "list-sessions"],
        about = "List the sessions on every server in the cluster"
    )]
    List(ListArgs),

    #[command(about = "List the projects in the cluster and where they are checked out")]
    Projects,

    #[command(subcommand, about = "Register projects with this server")]
    Project(ProjectAction),

    #[command(about = "List the servers in the cluster, or add, remove and forget them")]
    Servers(ServersArgs),

    #[command(
        about = "List the servers that Tailscale and the LAN found, and how linking them goes"
    )]
    Discover,

    #[command(
        about = "Pair with a server on the LAN: run it without a code on one machine, then with the code it prints on the other"
    )]
    Pair(PairArgs),

    #[command(
        subcommand,
        about = "Check the Lua config, print its defaults or its path, or reload it"
    )]
    Config(ConfigAction),

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

    #[arg(
        long,
        value_name = "SERVER",
        help = "Server to run the session on, defaults to the project's default_server or this one"
    )]
    pub on: Option<String>,

    #[arg(
        short = 'p',
        long,
        value_name = "PROJECT",
        help = "Project to run the session in, by name or id, defaults to the repo here"
    )]
    pub project: Option<String>,

    #[arg(
        short = 'b',
        long,
        value_name = "BRANCH",
        help = "Branch whose worktree the session runs in, created if needed"
    )]
    pub branch: Option<String>,

    #[arg(
        long,
        help = "Clone the project into projects_dir when the server has no checkout of it"
    )]
    pub clone: bool,
}

#[derive(Debug, Default, Args)]
pub struct ListArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = Grouping::Server,
        help = "Group the sessions by server or by project"
    )]
    pub by: Grouping,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Grouping {
    #[default]
    Server,
    Project,
}

#[derive(Debug, Subcommand)]
pub enum ProjectAction {
    #[command(about = "Register the git repository at a path, defaults to the current directory")]
    Add {
        #[arg(help = "A directory inside the repository")]
        path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum ConfigAction {
    #[command(
        about = "Load the config as the client and the server do, and print the files it read"
    )]
    Check,

    #[command(about = "Print every default as Lua assignments to amux.opt, a valid init.lua")]
    Defaults,

    #[command(about = "Print the path of your init.lua, whether or not it exists yet")]
    Path,

    #[command(about = "Make the running server load its config again, without a restart")]
    Reload,
}

#[derive(Debug, Args)]
pub struct AttachArgs {
    #[arg(
        short = 't',
        long = "target",
        value_name = "SESSION[@SERVER]",
        help = "Session to attach to, defaults to the most recent one on this server"
    )]
    pub target: Option<String>,
}

#[derive(Debug, Args)]
pub struct KillArgs {
    #[arg(
        short = 't',
        long = "target",
        value_name = "SESSION[@SERVER]",
        help = "Session to kill, defaults to the most recent one on this server"
    )]
    pub target: Option<String>,

    #[arg(
        long,
        help = "Also delete the session's worktree, refusing uncommitted changes and the main checkout"
    )]
    pub remove_worktree: bool,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    #[arg(
        short = 't',
        long = "target",
        value_name = "SESSION[@SERVER]",
        help = "Session to rename, defaults to the most recent one on this server"
    )]
    pub target: Option<String>,

    #[arg(help = "New name for the session")]
    pub name: String,
}

#[derive(Debug, Args)]
pub struct ServersArgs {
    #[command(subcommand)]
    pub action: Option<ServersAction>,
}

#[derive(Debug, Subcommand)]
pub enum ServersAction {
    #[command(about = "Add a server to servers.lua and link to it")]
    Add(AddServerArgs),

    #[command(visible_alias = "rm", about = "Remove a server from servers.lua")]
    Remove {
        #[arg(help = "Name of the server in servers.lua")]
        name: String,
    },

    #[command(
        about = "Forget a server: drop its link and addresses, remove it from servers.lua and refuse it from now on"
    )]
    Forget {
        #[arg(help = "Name or id of the server")]
        server: String,
    },
}

#[derive(Debug, Args)]
pub struct AddServerArgs {
    #[arg(help = "Name for the server")]
    pub name: String,

    #[arg(help = "ssh://[user@]host[:port], tcp://host:port, lan://<server id> or exec:<command>")]
    pub address: String,

    #[arg(
        long,
        help = "Path of amux on that machine, defaults to the first of amux on its PATH, ~/.cargo/bin, ~/.local/bin, /usr/local/bin and /opt/homebrew/bin"
    )]
    pub amux_path: Option<String>,

    #[arg(
        long,
        help = "Socket name of the server on that machine, defaults to this server's"
    )]
    pub socket: Option<String>,
}

#[derive(Debug, Args)]
pub struct PairArgs {
    #[arg(help = "The code that `amux pair` printed on the other machine")]
    pub code: Option<String>,

    #[arg(
        long,
        requires = "code",
        value_name = "HOST[:PORT]",
        help = "Address of the other machine, for when multicast does not reach it"
    )]
    pub host: Option<String>,

    #[arg(
        long,
        help = "Give this server a new key first, so that a server that was forgotten can pair again"
    )]
    pub new_key: bool,

    #[arg(
        short,
        long,
        help = "Print each step of the pairing as it happens, to see where it stops"
    )]
    pub verbose: bool,
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
