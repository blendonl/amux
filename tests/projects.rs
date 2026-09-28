mod common;

use std::fs;
use std::path::{Path, PathBuf};

use amux::protocol::{AttachedSession, ClientMessage, NewSession};
use common::git::{self, path_str, Repos, PROJECT};
use common::{settled_pair, Listing, TestServer, DETACH};

fn blocking<T>(work: impl FnOnce() -> T) -> T {
    tokio::task::block_in_place(work)
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|err| panic!("resolving {}: {err}", path.display()))
}

fn add_project(server: &TestServer, checkout: &Path) -> String {
    server.run_ok(&["project", "add", path_str(checkout)])
}

fn project_session(repos: &Repos, branch: Option<&str>) -> NewSession {
    NewSession {
        project: Some(repos.project_ref()),
        branch: branch.map(str::to_owned),
        ..NewSession::new(None, common::SIZE)
    }
}

fn create_detached(server: &TestServer, request: NewSession) -> AttachedSession {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("building a runtime");
    runtime.block_on(async {
        let mut client = server.client().await;
        let attached = client.create(request).await;
        client.detach().await;
        attached
    })
}

fn sessions_on(server: &TestServer, name: &str) -> Vec<String> {
    Listing::parse(&server.run_ok(&["ls"]))
        .sessions(name)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn new_with_a_project_and_branch_runs_in_its_worktree_and_new_again_attaches_to_it() {
    let repos = Repos::new();
    let server = TestServer::start();
    let checkout = repos.clone_to(&repos.path("work/amux"));
    let added = add_project(&server, &checkout);
    assert_eq!(
        added.trim(),
        format!(
            "registered amux ({}) at {}",
            repos.project_ref().id,
            checkout.display()
        )
    );
    let worktree = repos.path("work/amux-worktrees/feature-x");

    let mut first = server.terminal(&["new", "-p", PROJECT, "-b", "feature-x"]);
    first.type_text("echo \"at=$PWD\"\r");
    first.wait_for_text(&format!("at={}", worktree.display()));
    first.type_text(DETACH);
    first.wait_for_text("[detached (from session amux/feature-x)]");
    assert!(first.wait_for_exit().success());
    assert_eq!(
        git::git(&worktree, &["branch", "--show-current"]),
        "feature-x"
    );

    let mut again = server.terminal(&["new", "-p", PROJECT, "-b", "feature-x"]);
    again.wait_for_text(&format!("at={}", worktree.display()));
    again.type_text(DETACH);
    again.wait_for_text("[detached (from session amux/feature-x)]");
    assert!(again.wait_for_exit().success());
    assert_eq!(sessions_on(&server, server.name()), ["amux/feature-x"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_news_for_one_worktree_end_up_in_one_session() {
    let repos = Repos::new();
    let worktrees = repos.path("configured-worktrees");
    let server = TestServer::builder()
        .config(&format!(
            "amux.opt.projects.amux = {{ worktrees_dir = {} }}",
            common::lua(path_str(&worktrees))
        ))
        .start();
    let checkout = repos.clone_to(&repos.path("work/amux"));
    blocking(|| add_project(&server, &checkout));
    let request = project_session(&repos, Some("together"));

    let mut clients = Vec::new();
    for _ in 0..4 {
        clients.push(server.client().await);
    }
    for client in &clients {
        client
            .send(ClientMessage::NewSession(request.clone()))
            .await;
    }
    let mut attached = Vec::new();
    for client in &mut clients {
        attached.push(client.expect_attached().await);
    }

    assert!(
        attached.iter().all(|session| session.id == attached[0].id),
        "{attached:?}"
    );
    assert_eq!(attached[0].session, "amux/together");
    let sessions = server.client().await.list_sessions().await;
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0].project, Some(repos.project_ref().id));
    assert_eq!(sessions[0].branch.as_deref(), Some("together"));
    assert_eq!(sessions[0].attached_clients, 4);
    assert_eq!(
        git::git(&worktrees.join("together"), &["branch", "--show-current"]),
        "together"
    );
}

#[test]
fn kill_remove_worktree_refuses_dirty_trees_and_the_main_checkout() {
    let repos = Repos::new();
    let server = TestServer::start();
    let checkout = repos.clone_to(&repos.path("work/amux"));
    add_project(&server, &checkout);
    let worktree = repos.path("work/amux-worktrees/feature-x");
    create_detached(&server, project_session(&repos, Some("feature-x")));
    let scratch = worktree.join("scratch.txt");
    fs::write(&scratch, "unsaved work").unwrap();

    let dirty = server.run(&["kill", "-t", "amux/feature-x", "--remove-worktree"]);

    let stderr = String::from_utf8_lossy(&dirty.stderr);
    assert!(!dirty.status.success());
    assert!(stderr.contains("uncommitted changes"), "{stderr}");
    assert!(scratch.exists());
    assert_eq!(sessions_on(&server, server.name()), ["amux/feature-x"]);

    fs::remove_file(&scratch).unwrap();
    server.run_ok(&["kill", "-t", "amux/feature-x", "--remove-worktree"]);
    assert!(!worktree.exists());
    assert!(sessions_on(&server, server.name()).is_empty());
    assert!(git::git(&checkout, &["branch", "--list", "feature-x"]).contains("feature-x"));

    let main = create_detached(&server, project_session(&repos, None));
    assert_eq!(main.session, "amux/main");
    let refused = server.run(&["kill", "-t", "amux/main", "--remove-worktree"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success());
    assert!(stderr.contains("main checkout"), "{stderr}");
    assert!(checkout.join(".git").exists());

    server.create_session("plain");
    let unbound = server.run(&["kill", "-t", "plain", "--remove-worktree"]);
    let stderr = String::from_utf8_lossy(&unbound.stderr);
    assert!(
        stderr.contains("session plain is not bound to a project worktree"),
        "{stderr}"
    );
    assert_eq!(sessions_on(&server, server.name()), ["amux/main", "plain"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn clone_on_a_peer_registers_the_project_and_fetches_branches_only_on_origin() {
    let repos = Repos::new();
    let [a, b] = settled_pair(
        TestServer::builder().name("a"),
        TestServer::builder().name("b"),
    );
    let checkout = repos.clone_to(&repos.path("work/amux"));
    blocking(|| add_project(&a, &checkout));
    let on_b = NewSession {
        on: Some("b".into()),
        ..project_session(&repos, None)
    };

    let mut client = a.client().await;
    client.send(ClientMessage::NewSession(on_b.clone())).await;
    let error = client.expect_error().await;
    assert!(
        error.starts_with("b has no checkout of project amux"),
        "{error}"
    );
    assert!(error.contains("--clone"), "{error}");
    assert!(error.contains("`amux project add <path>` on b"), "{error}");

    let mut client = a.client().await;
    let attached = client
        .create(NewSession {
            clone: true,
            ..on_b.clone()
        })
        .await;
    assert_eq!(
        (attached.server.as_str(), attached.session.as_str()),
        ("b", "amux/main")
    );
    let cloned = canonical(&b.home()).join("projects").join(PROJECT);
    client.type_text("echo \"at=$PWD\"\r").await;
    client
        .wait_for_text(&format!("at={}", cloned.display()))
        .await;
    client.detach().await;

    let pushed = repos.push_branch("pushed-later", "only on origin");
    let mut client = a.client().await;
    let attached = client
        .create(NewSession {
            branch: Some("pushed-later".into()),
            ..on_b.clone()
        })
        .await;
    assert_eq!(attached.session, "amux/pushed-later");
    let worktree = cloned
        .parent()
        .unwrap()
        .join("amux-worktrees")
        .join("pushed-later");
    client.type_text("echo \"at=$PWD\"\r").await;
    client
        .wait_for_text(&format!("at={}", worktree.display()))
        .await;
    client.detach().await;
    assert_eq!(git::git(&worktree, &["rev-parse", "HEAD"]), pushed);
    blocking(|| {
        a.wait_for_ls("amux/pushed-later on b", |ls| {
            ls.sessions("b").contains(&"amux/pushed-later")
        })
    });

    blocking(|| {
        let id = repos.project_ref().id;
        let projects = a.wait_for_output(&["projects"], "both checkouts", |projects| {
            projects.contains(path_str(&cloned))
        });
        let lines: Vec<Vec<&str>> = projects
            .lines()
            .map(|line| line.split_whitespace().collect())
            .collect();
        assert_eq!(
            lines,
            [
                vec![PROJECT, id.as_str()],
                vec!["a", path_str(&checkout)],
                vec!["b", path_str(&cloned)],
            ],
            "{projects}"
        );

        let by_project = a.wait_for_output(
            &["ls", "--by", "project"],
            "both sessions on b",
            |sessions| sessions.contains("amux/pushed-later@b"),
        );
        let lines: Vec<Vec<&str>> = by_project
            .lines()
            .map(|line| line.split_whitespace().collect())
            .collect();
        assert_eq!(
            lines,
            [
                vec![PROJECT, id.as_str()],
                vec!["amux/main@b", "1", "window"],
                vec!["amux/pushed-later@b", "1", "window"],
            ],
            "{by_project}"
        );

        a.run_ok(&["kill", "-t", "amux/pushed-later@b", "--remove-worktree"]);
        assert!(!worktree.exists());
    });
}

#[test]
fn default_server_routes_a_project_session_to_that_server() {
    let repos = Repos::new();
    let b = TestServer::builder().name("b").start();
    let checkout = repos.clone_to(&repos.path("b/amux"));
    add_project(&b, &checkout);
    let a = TestServer::builder()
        .name("a")
        .config("amux.opt.projects.amux = { default_server = \"b\" }")
        .peer(&b)
        .start();
    a.wait_for_output(
        &["projects"],
        "b's checkout from its snapshot",
        |projects| projects.contains(path_str(&checkout)),
    );

    let mut terminal = a.terminal(&["new", "-p", PROJECT]);
    terminal.type_text("echo \"at=$PWD\"\r");
    terminal.wait_for_text(&format!("at={}", checkout.display()));
    terminal.type_text(DETACH);
    terminal.wait_for_text("[detached (from session amux/main@b)]");
    assert!(terminal.wait_for_exit().success());
    let ls = a.wait_for_ls("amux/main on b", |ls| ls.sessions("b") == ["amux/main"]);
    assert!(ls.sessions("a").is_empty(), "{ls:?}");

    let mut here = a.terminal(&["new", "-p", PROJECT, "--on", "a"]);
    here.wait_for_text("a has no checkout of project amux");
    assert!(!here.wait_for_exit().success());
}

#[test]
fn new_inside_a_repo_binds_the_session_and_registers_the_checkout() {
    let repos = Repos::new();
    let server = TestServer::start();
    let checkout = repos.clone_to(&repos.path("work/amux"));
    let nested = checkout.join("src");
    fs::create_dir_all(&nested).unwrap();

    server.create_session("amux/main");

    let mut terminal = server.terminal_in(&nested, &["new"]);
    terminal.type_text("echo \"at=$PWD\"\r");
    terminal.wait_for_text(&format!("at={}", checkout.display()));
    terminal.type_text(DETACH);
    terminal.wait_for_text("[detached (from session amux/main-2)]");
    assert!(terminal.wait_for_exit().success());
    let projects = server.run_ok(&["projects"]);
    assert!(
        projects.lines().any(|line| line
            .split_whitespace()
            .eq([server.name(), path_str(&checkout)])),
        "{projects}"
    );

    let moved = repos.clone_to(&repos.path("elsewhere/amux"));
    let added = server.run_ok_in(&moved, &["project", "add"]);
    assert!(
        added.ends_with(&format!("at {}\n", moved.display())),
        "{added}"
    );
    let projects = server.run_ok(&["projects"]);
    assert!(projects.contains(path_str(&moved)), "{projects}");
    assert!(!projects.contains(path_str(&checkout)), "{projects}");

    let mut outside = server.terminal(&["new", "-b", "feature"]);
    outside.wait_for_text("-b and --clone need a project");
    assert!(!outside.wait_for_exit().success());

    let mut server = server;
    server.restart();
    let projects = server.run_ok(&["projects"]);
    assert!(projects.contains(path_str(&moved)), "{projects}");
}

#[test]
fn a_machine_without_git_starts_sessions_quietly_and_still_refuses_project_flags() {
    let server = TestServer::builder().prepare();
    let no_git = server.root().join("no-git");
    fs::create_dir(&no_git).unwrap();
    let without_git = [("PATH", path_str(&no_git))];

    let mut started = server.terminal_with_env(&[], &without_git);
    started.type_text("echo $((6*7))\r");
    started.wait_for_text("42");
    started.type_text(DETACH);
    let screen = started.wait_for_text("[detached (from session 0)]");
    assert!(started.wait_for_exit().success());
    assert!(!screen.contains("amux:"), "{screen}");

    let mut branch = server.terminal_with_env(&["new", "-b", "feature"], &without_git);
    branch.wait_for_text("-b and --clone need a project");
    assert!(!branch.wait_for_exit().success());

    let mut cloned = server.terminal_with_env(&["new", "--clone"], &without_git);
    cloned.wait_for_text("-b and --clone need a project");
    assert!(!cloned.wait_for_exit().success());

    let mut unknown = server.terminal_with_env(&["new", "-p", "missing"], &without_git);
    unknown.wait_for_text("unknown project: missing");
    assert!(!unknown.wait_for_exit().success());
}
