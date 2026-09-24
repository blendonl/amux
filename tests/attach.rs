mod common;

use std::time::Duration;

use amux::protocol::{ClientMessage, ServerMessage};
use common::{TestServer, SIZE, TIMEOUT};

#[tokio::test]
async fn reattaching_after_a_detach_redraws_the_whole_screen() {
    let server = TestServer::start();
    let mut client = server.client().await;
    let session = client.new_session(None).await;
    client.type_text("echo $((6*7))\r").await;
    client.wait_for_text("42").await;
    client.detach().await;

    let mut client = server.client().await;
    assert_eq!(client.attach(Some(&session)).await, session);
    let first_frame = client.next_output().await;

    let mut fresh = vt100::Parser::new(SIZE.rows, SIZE.cols, 0);
    fresh.process(&first_frame);
    let contents = fresh.screen().contents();
    assert!(
        contents.contains("42"),
        "first frame after reattach:\n{contents}"
    );
}

#[tokio::test]
async fn attaching_without_a_target_picks_the_most_recent_session() {
    let server = TestServer::start();
    server.client().await.new_session(Some("older")).await;
    server.client().await.new_session(Some("newer")).await;

    assert_eq!(server.client().await.attach(None).await, "newer");
}

#[tokio::test]
async fn input_is_handled_while_the_client_is_not_reading_output() {
    let server = TestServer::start();
    let mut client = server.client().await;
    client.new_session(None).await;
    client.type_text("yes\r").await;
    client.wait_for_text("y\ny\ny").await;

    let marker = server.home().join("marker");
    client.type_text("\x03").await;
    client
        .type_text(&format!(": > {}\r", marker.display()))
        .await;

    let deadline = std::time::Instant::now() + TIMEOUT;
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the shell never ran the command typed while output was unread"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn panes_do_not_inherit_ssh_variables() {
    let server = TestServer::builder()
        .env("SSH_CONNECTION", "10.0.0.1 50000 10.0.0.2 22")
        .env("SSH_AUTH_SOCK", "/tmp/agent.sock")
        .start();
    let mut client = server.client().await;
    client.new_session(None).await;
    client
        .type_text("echo \"ssh=[$SSH_CONNECTION$SSH_AUTH_SOCK] home=$HOME\"\r")
        .await;

    let expected = format!("ssh=[] home={}", server.home().display());
    client.wait_for_text(&expected).await;
}

#[tokio::test]
async fn a_session_in_a_missing_directory_fails_with_the_path() {
    let server = TestServer::start();
    let missing = server.home().join("missing");
    let mut client = server.client().await;
    client
        .send(ClientMessage::NewSession {
            name: None,
            cwd: missing.clone(),
            size: SIZE,
        })
        .await;

    match client.recv().await {
        Some(ServerMessage::Error(message)) => {
            assert!(
                message.contains(&missing.display().to_string()),
                "{message}"
            );
        }
        other => panic!("expected an error, got {other:?}"),
    }
    assert!(server.client().await.list_sessions().await.is_empty());
}

#[tokio::test]
async fn a_session_ends_when_its_shell_exits() {
    let server = TestServer::start();
    let mut client = server.client().await;
    client.new_session(Some("short")).await;
    client.type_text("exit\r").await;

    loop {
        match client.recv().await {
            Some(ServerMessage::Exited) => break,
            Some(ServerMessage::Output(_)) => {}
            other => panic!("expected the session to exit, got {other:?}"),
        }
    }
    let deadline = std::time::Instant::now() + TIMEOUT;
    while !server.client().await.list_sessions().await.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the session was never removed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
