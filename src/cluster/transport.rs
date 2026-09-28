use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use anyhow::{bail, Context, Result};

use super::ssh;
use crate::identity::ServerId;
use crate::protocol::{LinkTransport, PublicKey};
use crate::settings::SshSettings;

const SSH_SCHEME: &str = "ssh://";
const EXEC_SCHEME: &str = "exec:";
const TCP_SCHEME: &str = "tcp://";
const LAN_SCHEME: &str = "lan://";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Ssh {
        user: Option<String>,
        host: String,
        port: Option<u16>,
    },
    Exec(Vec<String>),
    Tcp {
        host: String,
        port: u16,
    },
    Lan {
        id: ServerId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connect {
    Command(Vec<String>),
    Tcp(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportAuth {
    Ssh,
    Noise { key: PublicKey, vouched_by: Voucher },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voucher {
    Nobody,
    TrustStore,
    Tailnet,
    Pairing,
}

impl TransportAuth {
    pub fn transport(&self) -> LinkTransport {
        match self {
            Self::Ssh => LinkTransport::Ssh,
            Self::Noise { .. } => LinkTransport::Noise,
        }
    }

    pub fn key(&self) -> Option<PublicKey> {
        match self {
            Self::Ssh => None,
            Self::Noise { key, .. } => Some(*key),
        }
    }
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
        if let Some(rest) = text.strip_prefix(TCP_SCHEME) {
            return parse_tcp(rest).with_context(|| format!("parsing the address {text:?}"));
        }
        if let Some(id) = text.strip_prefix(LAN_SCHEME) {
            let id = id
                .parse()
                .with_context(|| format!("parsing the address {text:?}"))?;
            return Ok(Self::Lan { id });
        }
        bail!(
            "{text:?} is none of ssh://[user@]host[:port], exec:<command>, tcp://host:port \
             and lan://<server id>"
        )
    }
}

impl Address {
    pub fn is_local_only(&self) -> bool {
        let Self::Tcp { host, .. } = self else {
            return false;
        };
        match host.parse::<IpAddr>() {
            Ok(ip) => {
                let ip = ip.to_canonical();
                ip.is_loopback() || ip.is_unspecified()
            }
            Err(_) => host.eq_ignore_ascii_case("localhost"),
        }
    }

    pub fn connect(
        &self,
        ssh: &SshSettings,
        amux_path: Option<&str>,
        socket: &str,
        no_start: bool,
        endpoints: &[SocketAddr],
    ) -> Option<Connect> {
        match self {
            Self::Ssh { user, host, port } => Some(Connect::Command(ssh::command(
                ssh,
                user.as_deref(),
                host,
                *port,
                amux_path,
                socket,
                no_start,
            ))),
            Self::Exec(argv) => Some(Connect::Command(ssh::exec(argv, no_start))),
            Self::Tcp { host, port } => Some(Connect::Tcp(vec![endpoint(host, *port)])),
            Self::Lan { .. } if endpoints.is_empty() => None,
            Self::Lan { .. } => Some(Connect::Tcp(
                endpoints.iter().map(ToString::to_string).collect(),
            )),
        }
    }
}

fn endpoint(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn parse_ssh(text: &str) -> Result<Address> {
    let (user, host_and_port) = match text.split_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, text),
    };
    let (host, port) = split_host_port(host_and_port)?;

    if let Some(user) = user {
        if user.is_empty() || user.starts_with('-') || !is_plain(user) {
            bail!("invalid user {user:?}");
        }
    }
    check_host(host)?;
    Ok(Address::Ssh {
        user: user.map(str::to_owned),
        host: host.to_owned(),
        port: port.map(parse_port).transpose()?,
    })
}

fn parse_tcp(text: &str) -> Result<Address> {
    let (host, port) = split_host_port(text)?;
    check_host(host)?;
    let port = port.context("missing the port")?;
    Ok(Address::Tcp {
        host: host.to_owned(),
        port: parse_port(port)?,
    })
}

fn split_host_port(text: &str) -> Result<(&str, Option<&str>)> {
    match text.strip_prefix('[') {
        Some(bracketed) => {
            let (host, rest) = bracketed.split_once(']').context("missing `]`")?;
            let port = match rest {
                "" => None,
                rest => Some(rest.strip_prefix(':').context("expected `:` after `]`")?),
            };
            Ok((host, port))
        }
        None => Ok(match text.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (text, None),
        }),
    }
}

fn check_host(host: &str) -> Result<()> {
    if host.is_empty() || host.starts_with('-') || !is_plain(host) {
        bail!("invalid host {host:?}");
    }
    Ok(())
}

fn parse_port(port: &str) -> Result<u16> {
    match port.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => bail!("invalid port {port:?}"),
    }
}

fn is_plain(text: &str) -> bool {
    !text
        .chars()
        .any(|char| char.is_whitespace() || char.is_control() || matches!(char, '@' | '/'))
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

    fn tcp(host: &str, port: u16) -> Address {
        Address::Tcp {
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
    fn tcp_addresses_need_a_host_and_a_port() {
        assert_eq!(
            "tcp://100.64.0.2:7447".parse::<Address>().unwrap(),
            tcp("100.64.0.2", 7447)
        );
        assert_eq!(
            "tcp://desk.tail0.ts.net:7447".parse::<Address>().unwrap(),
            tcp("desk.tail0.ts.net", 7447)
        );
        assert_eq!(
            "tcp://[fd7a:115c:a1e0::1]:7447".parse::<Address>().unwrap(),
            tcp("fd7a:115c:a1e0::1", 7447)
        );
    }

    #[test]
    fn lan_addresses_name_a_server_id() {
        let id = ServerId::random().unwrap();
        assert_eq!(
            format!("lan://{id}").parse::<Address>().unwrap(),
            Address::Lan { id }
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
            "tcp://",
            "tcp://desk",
            "tcp://desk:",
            "tcp://desk:0",
            "tcp://me@desk:7447",
            "tcp://[::1]",
            "tcp://[::1:7447",
            "tcp://-desk:7447",
            "tcp://desk:7447/path",
            "lan://",
            "lan://laptop",
            "lan://0123",
        ] {
            assert!(address.parse::<Address>().is_err(), "{address} parsed");
        }
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
            address.connect(
                &SshSettings::default(),
                Some("ignored"),
                "ignored",
                true,
                &[]
            ),
            Some(Connect::Command(argv(&[
                "env",
                "HOME=/tmp/a b",
                "amux",
                "-S",
                "/tmp/b.sock",
                "bridge",
                "--no-start"
            ])))
        );
    }

    #[test]
    fn tcp_addresses_connect_to_their_endpoint() {
        assert_eq!(
            tcp("100.64.0.2", 7447).connect(&SshSettings::default(), None, "default", true, &[]),
            Some(Connect::Tcp(argv(&["100.64.0.2:7447"])))
        );
        assert_eq!(
            tcp("fd7a::1", 7447).connect(&SshSettings::default(), None, "default", false, &[]),
            Some(Connect::Tcp(argv(&["[fd7a::1]:7447"])))
        );
    }

    #[test]
    fn lan_addresses_connect_to_the_endpoints_last_seen() {
        let lan = Address::Lan {
            id: ServerId::random().unwrap(),
        };
        assert_eq!(
            lan.connect(&SshSettings::default(), None, "default", true, &[]),
            None
        );
        let endpoints: [SocketAddr; 2] = [
            "192.168.0.10:40123".parse().unwrap(),
            "[fe80::2]:40123".parse().unwrap(),
        ];
        assert_eq!(
            lan.connect(&SshSettings::default(), None, "default", true, &endpoints),
            Some(Connect::Tcp(argv(&[
                "192.168.0.10:40123",
                "[fe80::2]:40123"
            ])))
        );
    }

    #[test]
    fn only_loopback_and_unspecified_tcp_hosts_are_local_only() {
        for address in [
            "tcp://127.0.0.1:7447",
            "tcp://127.1.2.3:7447",
            "tcp://0.0.0.0:7447",
            "tcp://[::1]:7447",
            "tcp://[::]:7447",
            "tcp://[::ffff:127.0.0.1]:7447",
            "tcp://localhost:7447",
        ] {
            assert!(
                address.parse::<Address>().unwrap().is_local_only(),
                "{address}"
            );
        }
        for address in [
            "tcp://192.168.0.10:7447",
            "tcp://100.64.0.2:7447",
            "tcp://[fd7a:115c:a1e0::1]:7447",
            "tcp://desk.tail0.ts.net:7447",
            "ssh://localhost",
            "exec:amux bridge",
        ] {
            assert!(
                !address.parse::<Address>().unwrap().is_local_only(),
                "{address}"
            );
        }
    }

    #[test]
    fn only_noise_carries_a_key() {
        let key = PublicKey([9; 32]);
        let noise = TransportAuth::Noise {
            key,
            vouched_by: Voucher::TrustStore,
        };
        assert_eq!(TransportAuth::Ssh.transport(), LinkTransport::Ssh);
        assert_eq!(TransportAuth::Ssh.key(), None);
        assert_eq!(noise.transport(), LinkTransport::Noise);
        assert_eq!(noise.key(), Some(key));
    }
}
