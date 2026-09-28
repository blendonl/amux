mod common;

use common::TestServer;

#[test]
fn discover_says_when_both_sources_are_off() {
    let server = TestServer::start();

    let discover = server.run_ok(&["discover"]);

    assert_eq!(
        discover,
        "tailscale  off (disabled in the config)\nlan        off (disabled in the config)\n"
    );
}

#[test]
fn discover_needs_a_running_server() {
    let server = TestServer::builder().prepare();

    let output = server.run(&["discover"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no server running"), "{stderr}");
}
