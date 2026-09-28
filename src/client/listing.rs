use std::collections::BTreeMap;
use std::fmt::Write;
use std::time::{Duration, SystemTime};

use crate::project::ProjectId;
use crate::protocol::{
    DiscoveryReport, DiscoveryStatus, DiscoveryView, ServerStatus, ServerView, SessionInfo,
};

const MIN_NAME_COLUMN: usize = 25;
const WINDOWS_COLUMN: usize = 12;
const COLUMN_GAP: usize = 2;
const TABLE_GAP: &str = "   ";
const SESSION_INDENT: &str = "  ";
const NO_PROJECT: &str = "(no project)";
const STALE: &str = "stale";

pub fn sessions(servers: &[ServerView], now: SystemTime) -> String {
    let name_column = servers
        .iter()
        .flat_map(|server| {
            let header = match server.status {
                ServerStatus::Local => None,
                _ => Some(width(&server.name)),
            };
            let sessions = server
                .sessions
                .iter()
                .map(|session| SESSION_INDENT.len() + width(&session.name));
            header.into_iter().chain(sessions)
        })
        .map(|len| len + COLUMN_GAP)
        .fold(MIN_NAME_COLUMN, usize::max);

    let mut out = String::new();
    for server in servers {
        let header = match &server.status {
            ServerStatus::Local => format!("{} (this server)", server.name),
            status => pad(&server.name, name_column) + &cluster_status(status, now),
        };
        push_line(&mut out, &header);

        let stale = is_stale(&server.status);
        for session in &server.sessions {
            let line = format!(
                "{SESSION_INDENT}{}{}{}",
                pad(&session.name, name_column - SESSION_INDENT.len()),
                pad(&windows(session), WINDOWS_COLUMN),
                session_flag(session, stale)
            );
            push_line(&mut out, &line);
        }
    }
    out
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct ProjectGroup<'a> {
    unbound: bool,
    label: String,
    id: Option<&'a ProjectId>,
}

struct ListedSession<'a> {
    target: String,
    session: &'a SessionInfo,
    stale: bool,
}

pub fn sessions_by_project(servers: &[ServerView]) -> String {
    let mut groups: BTreeMap<ProjectGroup, Vec<ListedSession>> = BTreeMap::new();
    for server in servers {
        for session in &server.sessions {
            let project = session.project.as_ref();
            let label = match project {
                Some(id) => project_name(servers, id),
                None => NO_PROJECT.to_owned(),
            };
            let group = ProjectGroup {
                unbound: project.is_none(),
                label,
                id: project,
            };
            groups.entry(group).or_default().push(ListedSession {
                target: format!("{}@{}", session.name, server.name),
                session,
                stale: is_stale(&server.status),
            });
        }
    }

    let name_column = groups
        .iter()
        .flat_map(|(group, sessions)| {
            let targets = sessions
                .iter()
                .map(|listed| SESSION_INDENT.len() + width(&listed.target));
            std::iter::once(width(&group.label)).chain(targets)
        })
        .map(|len| len + COLUMN_GAP)
        .fold(MIN_NAME_COLUMN, usize::max);

    let mut out = String::new();
    for (group, mut sessions) in groups {
        let header = match group.id {
            Some(id) => pad(&group.label, name_column) + id.as_str(),
            None => group.label,
        };
        push_line(&mut out, &header);
        sessions.sort_by(|a, b| a.target.cmp(&b.target));
        for listed in sessions {
            let line = format!(
                "{SESSION_INDENT}{}{}{}",
                pad(&listed.target, name_column - SESSION_INDENT.len()),
                pad(&windows(listed.session), WINDOWS_COLUMN),
                session_flag(listed.session, listed.stale)
            );
            push_line(&mut out, &line);
        }
    }
    out
}

pub fn projects(servers: &[ServerView]) -> String {
    let mut projects: BTreeMap<(String, &ProjectId), Vec<[String; 3]>> = BTreeMap::new();
    for server in servers {
        for checkout in &server.projects {
            let flag = if is_stale(&server.status) { STALE } else { "" };
            projects
                .entry((project_name(servers, &checkout.id), &checkout.id))
                .or_default()
                .push([
                    server.name.clone(),
                    checkout.path.display().to_string(),
                    flag.to_owned(),
                ]);
        }
    }

    let name_column = projects
        .iter()
        .flat_map(|((name, _), checkouts)| {
            let servers = checkouts
                .iter()
                .map(|[server, _, _]| SESSION_INDENT.len() + width(server));
            std::iter::once(width(name)).chain(servers)
        })
        .max()
        .unwrap_or_default()
        + COLUMN_GAP;
    let path_column = projects
        .values()
        .flatten()
        .map(|[_, path, _]| width(path) + TABLE_GAP.len())
        .max()
        .unwrap_or_default();

    let mut out = String::new();
    for ((name, id), checkouts) in projects {
        push_line(&mut out, &(pad(&name, name_column) + id.as_str()));
        for [server, path, flag] in checkouts {
            let line = format!(
                "{SESSION_INDENT}{}{}{flag}",
                pad(&server, name_column - SESSION_INDENT.len()),
                pad(&path, path_column)
            );
            push_line(&mut out, &line);
        }
    }
    out
}

fn project_name(servers: &[ServerView], id: &ProjectId) -> String {
    let local_first = servers
        .iter()
        .filter(|server| server.status == ServerStatus::Local)
        .chain(
            servers
                .iter()
                .filter(|server| server.status != ServerStatus::Local),
        );
    local_first
        .flat_map(|server| &server.projects)
        .find(|checkout| checkout.id == *id)
        .map_or_else(
            || {
                id.as_str()
                    .rsplit('/')
                    .find(|part| !part.is_empty())
                    .unwrap_or(id.as_str())
                    .to_owned()
            },
            |checkout| checkout.name.clone(),
        )
}

fn is_stale(status: &ServerStatus) -> bool {
    !matches!(status, ServerStatus::Local | ServerStatus::Online { .. })
}

pub fn servers(servers: &[ServerView], now: SystemTime) -> String {
    let rows: Vec<[String; 4]> = servers
        .iter()
        .map(|server| {
            [
                server.name.clone(),
                server_status(&server.status, now),
                server
                    .version
                    .as_ref()
                    .map_or_else(|| "unknown version".to_owned(), ToString::to_string),
                server.address.clone().unwrap_or_default(),
            ]
        })
        .collect();

    let mut widths = [0; 4];
    for row in &rows {
        for (column, cell) in row.iter().enumerate() {
            widths[column] = widths[column].max(width(cell));
        }
    }
    let mut out = String::new();
    for row in &rows {
        let line: String = row
            .iter()
            .zip(widths)
            .map(|(cell, column)| pad(cell, column) + TABLE_GAP)
            .collect();
        push_line(&mut out, &line);
    }
    out
}

pub fn discovery(report: &DiscoveryReport) -> String {
    let name_column = report
        .sources
        .iter()
        .map(|source| width(&source.via.to_string()))
        .chain(
            report
                .peers
                .iter()
                .map(|peer| SESSION_INDENT.len() + width(&peer.name)),
        )
        .max()
        .unwrap_or_default()
        + COLUMN_GAP;
    let address_column = report
        .peers
        .iter()
        .map(|peer| width(&peer.address) + TABLE_GAP.len())
        .max()
        .unwrap_or_default();

    let mut out = String::new();
    for source in &report.sources {
        let header = pad(&source.via.to_string(), name_column) + &source.state.to_string();
        push_line(&mut out, &header);
        for peer in report.peers.iter().filter(|peer| peer.via == source.via) {
            let line = format!(
                "{SESSION_INDENT}{}{}{}",
                pad(&peer.name, name_column - SESSION_INDENT.len()),
                pad(&peer.address, address_column),
                discovery_status(peer)
            );
            push_line(&mut out, &line);
        }
    }
    out
}

fn discovery_status(peer: &DiscoveryView) -> String {
    match (&peer.status, &peer.last_error) {
        (DiscoveryStatus::Linked, _) | (_, None) => peer.status.to_string(),
        (status, Some(error)) => format!("{status}: {error}"),
    }
}

pub fn duration(secs: u64) -> String {
    let plural = |count: u64, unit: &str| match count {
        1 => format!("1 {unit}"),
        count => format!("{count} {unit}s"),
    };
    match secs {
        0..60 => plural(secs, "second"),
        _ => plural(secs.div_ceil(60), "minute"),
    }
}

fn cluster_status(status: &ServerStatus, now: SystemTime) -> String {
    match status {
        ServerStatus::Online {
            latency: Some(latency),
        } => format_latency(*latency),
        ServerStatus::Online { latency: None } => "online".to_owned(),
        other => server_status(other, now),
    }
}

fn server_status(status: &ServerStatus, now: SystemTime) -> String {
    match status {
        ServerStatus::Local => "this server".to_owned(),
        ServerStatus::Online {
            latency: Some(latency),
        } => format!("online, {}", format_latency(*latency)),
        ServerStatus::Online { latency: None } => "online".to_owned(),
        ServerStatus::Offline { last_seen, stopped } => {
            let state = if *stopped {
                "offline (stopped)"
            } else {
                "offline"
            };
            match last_seen {
                Some(seen) => format!("{state}, last seen {}", ago(*seen, now)),
                None => format!("{state}, never seen"),
            }
        }
        ServerStatus::Incompatible { version } => format!("incompatible, runs {version}"),
    }
}

fn session_flag(session: &SessionInfo, stale: bool) -> &'static str {
    if stale {
        STALE
    } else if session.attached_clients > 0 {
        "attached"
    } else {
        ""
    }
}

fn windows(session: &SessionInfo) -> String {
    match session.windows.len() {
        1 => "1 window".to_owned(),
        count => format!("{count} windows"),
    }
}

fn format_latency(latency: Duration) -> String {
    match latency.as_millis() {
        0 => "<1 ms".to_owned(),
        millis => format!("{millis} ms"),
    }
}

fn ago(then: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(then).unwrap_or_default().as_secs();
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

fn pad(text: &str, column: usize) -> String {
    let mut padded = text.to_owned();
    let len = width(text);
    padded.extend(std::iter::repeat_n(' ', column.saturating_sub(len)));
    padded
}

fn width(text: &str) -> usize {
    text.chars().count()
}

fn push_line(out: &mut String, line: &str) {
    let _ = writeln!(out, "{}", line.trim_end());
}

#[cfg(test)]
mod tests {
    use crate::protocol::{
        ProjectCheckout, SessionId, SourceState, SourceView, Version, Via, WindowSummary,
    };

    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn session(name: &str, windows: usize, attached_clients: usize) -> SessionInfo {
        SessionInfo {
            id: SessionId(1),
            name: name.into(),
            windows: (0..windows)
                .map(|index| WindowSummary {
                    index,
                    name: "sh".into(),
                    panes: 1,
                })
                .collect(),
            attached_clients,
            last_activity: SystemTime::UNIX_EPOCH,
            project: None,
            branch: None,
        }
    }

    fn server(name: &str, status: ServerStatus, sessions: Vec<SessionInfo>) -> ServerView {
        ServerView {
            id: None,
            name: name.into(),
            address: None,
            version: Some(Version {
                release: "0.1.0".into(),
                major: 2,
                minor: 0,
            }),
            status,
            sessions,
            projects: Vec::new(),
        }
    }

    fn example(now: SystemTime) -> Vec<ServerView> {
        vec![
            server(
                "desktop",
                ServerStatus::Local,
                vec![session("amux/main", 3, 1), session("amux/feature-x", 1, 0)],
            ),
            server(
                "laptop",
                ServerStatus::Online {
                    latency: Some(Duration::from_millis(12)),
                },
                vec![session("notes/main", 1, 0)],
            ),
            server(
                "home-server",
                ServerStatus::Offline {
                    last_seen: Some(now - 3 * HOUR),
                    stopped: false,
                },
                vec![session("infra/main", 2, 1)],
            ),
        ]
    }

    #[test]
    fn ls_matches_the_design() {
        let now = SystemTime::now();
        assert_eq!(
            sessions(&example(now), now),
            "\
desktop (this server)
  amux/main              3 windows   attached
  amux/feature-x         1 window
laptop                   12 ms
  notes/main             1 window
home-server              offline, last seen 3h ago
  infra/main             2 windows   stale
"
        );
    }

    #[test]
    fn long_names_widen_the_column() {
        let now = SystemTime::now();
        let servers = vec![server(
            "a-server-with-a-very-long-name",
            ServerStatus::Offline {
                last_seen: None,
                stopped: true,
            },
            vec![session("s", 1, 0)],
        )];
        assert_eq!(
            sessions(&servers, now),
            "\
a-server-with-a-very-long-name  offline (stopped), never seen
  s                             1 window    stale
"
        );
    }

    const AMUX: &str = "github.com/blendonl/amux";
    const NOTES: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    fn checkout(id: &str, name: &str, path: &str) -> ProjectCheckout {
        ProjectCheckout {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            origin: None,
        }
    }

    fn bound(name: &str, project: &str, branch: &str, attached: usize) -> SessionInfo {
        SessionInfo {
            project: Some(project.into()),
            branch: Some(branch.into()),
            ..session(name, 1, attached)
        }
    }

    fn with_projects(now: SystemTime) -> Vec<ServerView> {
        let mut servers = example(now);
        servers[0].sessions = vec![
            bound("amux/main", AMUX, "main", 1),
            session("scratch", 2, 0),
        ];
        servers[0].projects = vec![
            checkout(AMUX, "amux", "/home/me/projects/amux"),
            checkout(NOTES, "notes", "/home/me/notes"),
        ];
        servers[1].sessions = vec![bound("amux/feature-x", AMUX, "feature-x", 0)];
        servers[1].projects = vec![checkout(AMUX, "amux", "/Users/me/src/amux")];
        servers[2].sessions = vec![bound("notes/main", NOTES, "main", 0)];
        servers[2].projects = vec![checkout(NOTES, "notes", "/srv/notes")];
        servers
    }

    #[test]
    fn ls_by_project_groups_sessions_across_servers() {
        let now = SystemTime::now();
        assert_eq!(
            sessions_by_project(&with_projects(now)),
            format!(
                "\
amux                      {AMUX}
  amux/feature-x@laptop   1 window
  amux/main@desktop       1 window    attached
notes                     {NOTES}
  notes/main@home-server  1 window    stale
(no project)
  scratch@desktop         2 windows
"
            )
        );
    }

    #[test]
    fn projects_lists_every_checkout_by_server() {
        let now = SystemTime::now();
        assert_eq!(
            projects(&with_projects(now)),
            format!(
                "\
amux           {AMUX}
  desktop      /home/me/projects/amux
  laptop       /Users/me/src/amux
notes          {NOTES}
  desktop      /home/me/notes
  home-server  /srv/notes               stale
"
            )
        );
        assert_eq!(projects(&example(now)), "");
    }

    #[test]
    fn servers_lists_status_version_and_address() {
        let now = SystemTime::now();
        let mut servers = example(now);
        servers[1].address = Some("ssh://laptop".into());
        servers[1].status = ServerStatus::Online {
            latency: Some(Duration::from_micros(300)),
        };
        servers[2].version = None;
        servers[2].status = ServerStatus::Incompatible {
            version: Version {
                release: "0.3.0".into(),
                major: 3,
                minor: 1,
            },
        };

        assert_eq!(
            super::servers(&servers, now),
            "\
desktop       this server                                    amux 0.1.0 (protocol 2.0)
laptop        online, <1 ms                                  amux 0.1.0 (protocol 2.0)   ssh://laptop
home-server   incompatible, runs amux 0.3.0 (protocol 3.1)   unknown version
"
        );
    }

    fn candidate(
        via: Via,
        name: &str,
        address: &str,
        status: DiscoveryStatus,
        last_error: Option<&str>,
    ) -> DiscoveryView {
        DiscoveryView {
            via,
            name: name.into(),
            address: address.into(),
            server: None,
            status,
            last_error: last_error.map(str::to_owned),
        }
    }

    #[test]
    fn discover_groups_candidates_by_source_with_their_last_error() {
        let report = DiscoveryReport {
            sources: vec![
                SourceView {
                    via: Via::Tailscale,
                    state: SourceState::Running,
                },
                SourceView {
                    via: Via::Lan,
                    state: SourceState::Running,
                },
            ],
            peers: vec![
                candidate(
                    Via::Tailscale,
                    "desk",
                    "tcp://100.64.0.2:7447",
                    DiscoveryStatus::Linked,
                    None,
                ),
                candidate(
                    Via::Tailscale,
                    "phone",
                    "tcp://100.64.0.9:7447",
                    DiscoveryStatus::Failing,
                    Some("connection refused"),
                ),
                candidate(
                    Via::Tailscale,
                    "nas",
                    "tcp://100.64.0.4:7447",
                    DiscoveryStatus::Absent,
                    None,
                ),
                candidate(
                    Via::Lan,
                    "laptop",
                    "lan://000000000000000000000000000000a7",
                    DiscoveryStatus::NotPaired,
                    None,
                ),
            ],
        };

        assert_eq!(
            discovery(&report),
            "\
tailscale  running
  desk     tcp://100.64.0.2:7447                    linked
  phone    tcp://100.64.0.9:7447                    failing: connection refused
  nas      tcp://100.64.0.4:7447                    absent
lan        running
  laptop   lan://000000000000000000000000000000a7   not paired
"
        );
    }

    #[test]
    fn discover_says_which_sources_are_off_or_not_running() {
        let report = DiscoveryReport {
            sources: vec![
                SourceView {
                    via: Via::Tailscale,
                    state: SourceState::Off,
                },
                SourceView {
                    via: Via::Lan,
                    state: SourceState::NotRunning,
                },
            ],
            peers: Vec::new(),
        };

        assert_eq!(
            discovery(&report),
            "tailscale  off (disabled in the config)\nlan        not running\n"
        );
    }

    #[test]
    fn durations_round_up_to_whole_minutes() {
        assert_eq!(duration(1), "1 second");
        assert_eq!(duration(45), "45 seconds");
        assert_eq!(duration(60), "1 minute");
        assert_eq!(duration(300), "5 minutes");
        assert_eq!(duration(299), "5 minutes");
    }

    #[test]
    fn ages_use_the_largest_whole_unit() {
        let now = SystemTime::now();
        assert_eq!(ago(now - Duration::from_secs(5), now), "5s ago");
        assert_eq!(ago(now - Duration::from_secs(150), now), "2m ago");
        assert_eq!(ago(now - 5 * HOUR, now), "5h ago");
        assert_eq!(ago(now - 50 * HOUR, now), "2d ago");
        assert_eq!(ago(now + HOUR, now), "0s ago");
    }
}
