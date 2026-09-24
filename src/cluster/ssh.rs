use std::process::{self, Stdio};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::info;

use crate::client::{self, Endpoint};

const SSH_SCHEME: &str = "ssh://";
const EXEC_SCHEME: &str = "exec:";
const DEFAULT_AMUX_PATH: &str = "amux";
const BRIDGE_COMMAND: &str = "bridge";
const NO_START_FLAG: &str = "--no-start";
const SSH_OPTIONS: [&str; 5] = ["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Ssh {
        user: Option<String>,
        host: String,
        port: Option<u16>,
    },
    Exec(Vec<String>),
}

impl FromStr for Address {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        if let Some(rest) = text.strip_prefix(SSH_SCHEME) {
            return parse_ssh(rest).with_context(|| format!("parsing the address {text:?}"));
        }
        if let Some(command) = text.strip_prefix(EXEC_SCHEME) {
            let argv = shell_words::split(command)
                .with_context(|| format!("splitting the address {text:?}"))?;
            if argv.is_empty() {
                bail!("the address {text:?} names no command");
            }
            return Ok(Self::Exec(argv));
        }
        bail!("{text:?} is neither an ssh://[user@]host[:port] nor an exec:<command> address")
    }
}

impl Address {
    pub fn bridge_command(
        &self,
        amux_path: Option<&str>,
        socket: &str,
        no_start: bool,
    ) -> Vec<String> {
        let mut argv = match self {
            Self::Ssh { user, host, port } => {
                let mut argv: Vec<String> = ["ssh"]
                    .into_iter()
                    .chain(SSH_OPTIONS)
                    .map(str::to_owned)
                    .collect();
                if let Some(port) = port {
                    argv.extend(["-p".to_owned(), port.to_string()]);
                }
                argv.push(match user {
                    Some(user) => format!("{user}@{host}"),
                    None => host.clone(),
                });
                argv.extend([
                    amux_path.unwrap_or(DEFAULT_AMUX_PATH).to_owned(),
                    "-L".to_owned(),
                    shell_words::quote(socket).into_owned(),
                    BRIDGE_COMMAND.to_owned(),
                ]);
                argv
            }
            Self::Exec(argv) => argv.clone(),
        };
        if no_start {
            argv.push(NO_START_FLAG.to_owned());
        }
        argv
    }
}

fn parse_ssh(text: &str) -> Result<Address> {
    let (user, host_and_port) = match text.split_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, text),
    };
    let (host, port) = match host_and_port.strip_prefix('[') {
        Some(bracketed) => {
            let (host, rest) = bracketed.split_once(']').context("missing `]`")?;
            let port = match rest {
                "" => None,
                rest => Some(rest.strip_prefix(':').context("expected `:` after `]`")?),
            };
            (host, port)
        }
        None => match host_and_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_and_port, None),
        },
    };

    if let Some(user) = user {
        if user.is_empty() || user.starts_with('-') || !is_plain(user) {
            bail!("invalid user {user:?}");
        }
    }
    if host.is_empty() || host.starts_with('-') || !is_plain(host) {
        bail!("invalid host {host:?}");
    }
    let port = port
        .map(|port| match port.parse::<u16>() {
            Ok(port) if port > 0 => Ok(port),
            _ => bail!("invalid port {port:?}"),
        })
        .transpose()?;

    Ok(Address::Ssh {
        user: user.map(str::to_owned),
        host: host.to_owned(),
        port,
    })
}

fn is_plain(text: &str) -> bool {
    !text
        .chars()
        .any(|char| char.is_whitespace() || char.is_control() || matches!(char, '@' | '/'))
}

pub struct Transport {
    pub child: Child,
    pub reader: ChildStdout,
    pub writer: ChildStdin,
}

pub fn spawn(argv: &[String], address: &str) -> Result<Transport> {
    let (program, args) = argv.split_first().context("the bridge command is empty")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("running {program}"))?;

    let reader = child.stdout.take().context("the bridge has no stdout")?;
    let writer = child.stdin.take().context("the bridge has no stdin")?;
    if let Some(stderr) = child.stderr.take() {
        let address = address.to_owned();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                info!(%address, "bridge: {line}");
            }
        });
    }
    Ok(Transport {
        child,
        reader,
        writer,
    })
}

pub async fn bridge(endpoint: &Endpoint, no_start: bool) -> Result<()> {
    let stream = if no_start {
        client::connect_stream(&endpoint.socket).await?
    } else {
        client::connect_or_start_server(endpoint).await?
    };
    let (mut from_server, mut to_server) = stream.into_split();

    let upstream = async move {
        let copied = io::copy(&mut io::stdin(), &mut to_server).await;
        let _ = to_server.shutdown().await;
        copied
    };
    let downstream = async move {
        let mut stdout = io::stdout();
        let copied = io::copy(&mut from_server, &mut stdout).await;
        let _ = stdout.flush().await;
        copied
    };

    let ended = tokio::select! {
        ended = upstream => ended,
        ended = downstream => ended,
    };
    match ended {
        Ok(_) => process::exit(0),
        Err(err) => {
            eprintln!("amux bridge: {err}");
            process::exit(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(user: Option<&str>, host: &str, port: Option<u16>) -> Address {
        Address::Ssh {
            user: user.map(str::to_owned),
            host: host.to_owned(),
            port,
        }
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn ssh_addresses_parse_user_host_and_port() {
        assert_eq!(
            "ssh://laptop".parse::<Address>().unwrap(),
            ssh(None, "laptop", None)
        );
        assert_eq!(
            "ssh://notpc@home-server".parse::<Address>().unwrap(),
            ssh(Some("notpc"), "home-server", None)
        );
        assert_eq!(
            "ssh://me@desk.tail0.ts.net:2222"
                .parse::<Address>()
                .unwrap(),
            ssh(Some("me"), "desk.tail0.ts.net", Some(2222))
        );
        assert_eq!(
            "ssh://[fe80::1]:22".parse::<Address>().unwrap(),
            ssh(None, "fe80::1", Some(22))
        );
    }

    #[test]
    fn malformed_addresses_are_rejected() {
        for address in [
            "laptop",
            "ssh://",
            "ssh://@laptop",
            "ssh://-oProxyCommand=x",
            "ssh://-oProxyCommand=x@host",
            "ssh://laptop:0",
            "ssh://laptop:ssh",
            "ssh://laptop/path",
            "ssh://a b",
            "ssh://a@b@c",
            "ssh://[::1",
            "exec:",
            "exec:   ",
            "exec:'unterminated",
        ] {
            assert!(address.parse::<Address>().is_err(), "{address} parsed");
        }
    }

    #[test]
    fn the_ssh_bridge_command_defaults_to_amux_on_the_path() {
        let address: Address = "ssh://laptop".parse().unwrap();
        assert_eq!(
            address.bridge_command(None, "default", false),
            argv(&[
                "ssh",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=15",
                "laptop",
                "amux",
                "-L",
                "default",
                "bridge",
            ])
        );
    }

    #[test]
    fn the_ssh_bridge_command_carries_user_port_amux_path_and_socket() {
        let address: Address = "ssh://notpc@home-server:2222".parse().unwrap();
        assert_eq!(
            address.bridge_command(Some("~/.cargo/bin/amux"), "dev box", true),
            argv(&[
                "ssh",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=15",
                "-p",
                "2222",
                "notpc@home-server",
                "~/.cargo/bin/amux",
                "-L",
                "'dev box'",
                "bridge",
                "--no-start",
            ])
        );
    }

    #[test]
    fn exec_addresses_split_like_a_shell_without_running_one() {
        let address: Address = "exec:env 'HOME=/tmp/a b' amux -S /tmp/b.sock bridge"
            .parse()
            .unwrap();
        assert_eq!(
            address,
            Address::Exec(argv(&[
                "env",
                "HOME=/tmp/a b",
                "amux",
                "-S",
                "/tmp/b.sock",
                "bridge"
            ]))
        );
        assert_eq!(
            address.bridge_command(Some("ignored"), "ignored", true),
            argv(&[
                "env",
                "HOME=/tmp/a b",
                "amux",
                "-S",
                "/tmp/b.sock",
                "bridge",
                "--no-start"
            ])
        );
    }
}
