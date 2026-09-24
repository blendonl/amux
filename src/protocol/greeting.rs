use std::fmt;
use std::io;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use super::{read_frame, write_message};

pub const MAGIC: [u8; 4] = *b"AMUX";
pub const PROTOCOL_MAJOR: u16 = 4;
pub const PROTOCOL_MINOR: u16 = 0;
pub const RELEASE: &str = env!("CARGO_PKG_VERSION");

const STOP_THE_SERVER: &str =
    "run `amux kill-server` to stop it (this ends its sessions), then try again";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Client,
    Peer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Greeting {
    pub magic: [u8; 4],
    pub major: u16,
    pub minor: u16,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub server_name: String,
    pub version: Version,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub release: String,
    pub major: u16,
    pub minor: u16,
}

impl Version {
    pub fn current() -> Self {
        Self {
            release: RELEASE.to_owned(),
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
        }
    }

    pub fn is_compatible_with(&self, major: u16) -> bool {
        self.major == major
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "amux {} (protocol {}.{})",
            self.release, self.major, self.minor
        )
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Client => "client",
            Self::Peer => "peer",
        })
    }
}

#[derive(Debug)]
pub struct IncompatibleServer {
    pub client: Version,
    pub server: Option<Welcome>,
}

impl fmt::Display for IncompatibleServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let client = &self.client;
        match &self.server {
            None => write!(
                f,
                "the running amux server is incompatible with this client, {client}: \
                 it did not answer the greeting; {STOP_THE_SERVER}"
            ),
            Some(Welcome {
                server_name,
                version,
            }) if version.major > client.major => write!(
                f,
                "the amux server `{server_name}` runs {version}, but this client is {client}; \
                 use the newer amux binary, or run `amux kill-server` to stop the server \
                 (this ends its sessions)"
            ),
            Some(Welcome {
                server_name,
                version,
            }) => write!(
                f,
                "the amux server `{server_name}` runs {version}, but this client is {client}; \
                 {STOP_THE_SERVER}"
            ),
        }
    }
}

impl std::error::Error for IncompatibleServer {}

pub async fn greet<S>(stream: &mut S, role: Role, local: &Version) -> Result<Welcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let greeting = Greeting {
        magic: MAGIC,
        major: local.major,
        minor: local.minor,
        role,
    };
    write_message(stream, &greeting)
        .await
        .context("sending the greeting")?;

    let reply = match read_frame(stream).await {
        Ok(Some(payload)) => postcard::from_bytes::<Welcome>(&payload).ok(),
        Ok(None) => None,
        Err(err) if is_hang_up(&err) => None,
        Err(err) => return Err(err.context("reading the server's welcome")),
    };
    match reply {
        Some(welcome) if local.is_compatible_with(welcome.version.major) => Ok(welcome),
        server => Err(IncompatibleServer {
            client: local.clone(),
            server,
        }
        .into()),
    }
}

pub async fn accept<S>(stream: &mut S, local: &Version, server_name: &str) -> Result<Option<Role>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(payload) = read_frame(stream).await? else {
        return Ok(None);
    };
    let greeting: Greeting = postcard::from_bytes(&payload).context("decoding the greeting")?;
    if greeting.magic != MAGIC {
        bail!("the connection did not open with an amux greeting");
    }

    let welcome = Welcome {
        server_name: server_name.to_owned(),
        version: local.clone(),
    };
    write_message(stream, &welcome)
        .await
        .context("sending the welcome")?;

    if !local.is_compatible_with(greeting.major) {
        bail!(
            "refused a {} speaking protocol {}.{}, this server speaks protocol {}.{}",
            greeting.role,
            greeting.major,
            greeting.minor,
            local.major,
            local.minor
        );
    }
    Ok(Some(greeting.role))
}

fn is_hang_up(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>().is_some_and(|err| {
        matches!(
            err.kind(),
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
        )
    })
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;

    fn version(major: u16, minor: u16) -> Version {
        Version {
            release: format!("{major}.{minor}.7"),
            major,
            minor,
        }
    }

    async fn handshake(
        client: Version,
        server: Version,
    ) -> (Result<Welcome>, Result<Option<Role>>) {
        let (mut client_end, mut server_end) = duplex(1024);
        let server_side =
            tokio::spawn(async move { accept(&mut server_end, &server, "desk").await });
        let welcome = greet(&mut client_end, Role::Client, &client).await;
        (welcome, server_side.await.unwrap())
    }

    fn incompatibility(result: Result<Welcome>) -> String {
        let err = result.unwrap_err();
        assert!(err.is::<IncompatibleServer>(), "unexpected error: {err:#}");
        err.to_string()
    }

    #[tokio::test]
    async fn matching_majors_connect_whatever_the_minor() {
        let (welcome, role) = handshake(version(1, 0), version(1, 3)).await;
        assert_eq!(
            welcome.unwrap(),
            Welcome {
                server_name: "desk".into(),
                version: version(1, 3),
            }
        );
        assert_eq!(role.unwrap(), Some(Role::Client));
    }

    #[tokio::test]
    async fn a_client_newer_than_the_server_is_refused_with_both_versions() {
        let (welcome, role) = handshake(version(2, 0), version(1, 4)).await;

        let message = incompatibility(welcome);
        assert!(message.contains("amux 1.4.7 (protocol 1.4)"), "{message}");
        assert!(message.contains("amux 2.0.7 (protocol 2.0)"), "{message}");
        assert!(message.contains("`desk`"), "{message}");
        assert!(message.contains("run `amux kill-server`"), "{message}");

        let refusal = role.unwrap_err().to_string();
        assert!(refusal.contains("protocol 2.0"), "{refusal}");
        assert!(refusal.contains("protocol 1.4"), "{refusal}");
    }

    #[tokio::test]
    async fn a_client_older_than_the_server_is_refused_with_both_versions() {
        let (welcome, role) = handshake(version(1, 4), version(2, 0)).await;

        let message = incompatibility(welcome);
        assert!(message.contains("amux 2.0.7 (protocol 2.0)"), "{message}");
        assert!(message.contains("amux 1.4.7 (protocol 1.4)"), "{message}");
        assert!(message.contains("use the newer amux binary"), "{message}");

        let refusal = role.unwrap_err().to_string();
        assert!(refusal.contains("protocol 1.4"), "{refusal}");
        assert!(refusal.contains("protocol 2.0"), "{refusal}");
    }

    #[tokio::test]
    async fn a_server_that_hangs_up_before_welcome_is_incompatible() {
        let (mut client_end, mut server_end) = duplex(1024);
        let server_side = tokio::spawn(async move {
            read_frame(&mut server_end).await.unwrap();
        });
        let welcome = greet(&mut client_end, Role::Client, &Version::current()).await;
        server_side.await.unwrap();

        let message = incompatibility(welcome);
        assert!(message.contains("run `amux kill-server`"), "{message}");
    }

    #[tokio::test]
    async fn a_connection_without_the_magic_is_refused_without_a_welcome() {
        let (mut client_end, mut server_end) = duplex(1024);
        let bogus = Greeting {
            magic: *b"tmux",
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            role: Role::Client,
        };
        write_message(&mut client_end, &bogus).await.unwrap();

        assert!(accept(&mut server_end, &Version::current(), "desk")
            .await
            .is_err());
        drop(server_end);
        assert_eq!(read_frame(&mut client_end).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_connection_closed_before_the_greeting_is_not_an_error() {
        let (client_end, mut server_end) = duplex(1024);
        drop(client_end);
        let role = accept(&mut server_end, &Version::current(), "desk").await;
        assert_eq!(role.unwrap(), None);
    }

    #[test]
    fn the_handshake_layout_never_changes() {
        let greeting = Greeting {
            magic: MAGIC,
            major: 1,
            minor: 2,
            role: Role::Peer,
        };
        assert_eq!(
            postcard::to_stdvec(&greeting).unwrap(),
            [b'A', b'M', b'U', b'X', 1, 2, 1]
        );

        let welcome = Welcome {
            server_name: "a".into(),
            version: Version {
                release: "0.1".into(),
                major: 3,
                minor: 4,
            },
        };
        assert_eq!(
            postcard::to_stdvec(&welcome).unwrap(),
            [1, b'a', 3, b'0', b'.', b'1', 3, 4]
        );
    }
}
