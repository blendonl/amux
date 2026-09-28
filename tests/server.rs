mod common;

use std::fs;

use amux::config::{Incarnation, ServerId};
use amux::protocol::{self, Hello, PeerMessage, Role, Version, PROTOCOL_MAJOR};
use common::TestServer;
use tokio::net::UnixStream;

#[tokio::test]
async fn the_welcome_carries_the_configured_name_and_version() {
    let server = TestServer::builder().name("desk").start();
    let client = server.client().await;

    assert_eq!(client.welcome().server_name, "desk");
    assert_eq!(client.welcome().version, Version::current());
}

#[tokio::test]
async fn a_client_with_another_major_version_is_refused_with_both_versions() {
    let server = TestServer::builder().name("desk").start();
    let future = Version {
        release: "9.9.9".into(),
        major: PROTOCOL_MAJOR + 1,
        minor: 0,
    };
    let mut stream = UnixStream::connect(server.socket()).await.unwrap();

    let message = protocol::greet(&mut stream, Role::Client, &future)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains(&Version::current().to_string()),
        "{message}"
    );
    assert!(message.contains(&future.to_string()), "{message}");
    assert!(message.contains("`desk`"), "{message}");
    server.wait_for_log("refused a client");
}

#[tokio::test]
async fn a_peer_greeting_is_answered_with_a_hello() {
    let server = TestServer::builder().name("desk").start();
    let mut stream = UnixStream::connect(server.socket()).await.unwrap();
    protocol::greet(&mut stream, Role::Peer, &Version::current())
        .await
        .unwrap();

    let visitor = Hello {
        id: ServerId::random().unwrap(),
        incarnation: Incarnation::random().unwrap(),
        name: "visitor".into(),
        version: Version::current(),
        peers: Vec::new(),
        public_key: None,
    };
    protocol::write_message(&mut stream, &PeerMessage::Hello(visitor))
        .await
        .unwrap();

    match protocol::read_message::<_, PeerMessage>(&mut stream).await {
        Ok(Some(PeerMessage::Hello(hello))) => {
            assert_eq!(hello.name, "desk");
            assert_eq!(hello.id.to_string(), server.server_id());
            assert_eq!(hello.version, Version::current());
            assert_eq!(hello.public_key, None);
        }
        other => panic!("expected a hello, got {other:?}"),
    }
}

#[test]
fn the_server_id_survives_a_restart_and_the_incarnation_does_not() {
    let mut server = TestServer::builder().name("desk").start();
    let id_path = server.state_dir().join("server-id");
    let id = fs::read_to_string(&id_path).unwrap().trim().to_owned();
    assert_eq!(id.len(), 32);
    server.wait_for_log(&format!("name=desk id={id}"));

    server.restart();

    assert_eq!(fs::read_to_string(&id_path).unwrap().trim(), id);
    let log = server.log();
    let incarnations: Vec<&str> = log
        .lines()
        .filter(|line| line.contains(&format!("id={id}")))
        .filter_map(|line| line.split("incarnation=").nth(1))
        .collect();
    assert_eq!(incarnations.len(), 2, "{log}");
    assert_ne!(incarnations[0], incarnations[1]);
}

#[test]
fn an_invalid_config_stops_the_server_from_starting() {
    let server = TestServer::builder().config("prefix = \"C-a\"").prepare();

    let output = server.run(&["server"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("unknown field `prefix`"), "{stderr}");
    assert!(
        stderr.contains(&server.config_path().display().to_string()),
        "{stderr}"
    );
}

#[test]
fn the_config_flag_and_environment_variable_override_the_default_path() {
    let server = TestServer::builder().prepare();
    let custom = server.root().join("custom.toml");
    fs::write(&custom, "bogus = true\n").unwrap();
    let custom = custom.to_str().unwrap();

    let flag = server.run(&["--config", custom, "server"]);
    let flag_stderr = String::from_utf8_lossy(&flag.stderr);
    assert!(flag_stderr.contains(custom), "{flag_stderr}");

    let env = server
        .command()
        .env("AMUX_CONFIG", custom)
        .arg("server")
        .output()
        .unwrap();
    let env_stderr = String::from_utf8_lossy(&env.stderr);
    assert!(env_stderr.contains(custom), "{env_stderr}");
}
