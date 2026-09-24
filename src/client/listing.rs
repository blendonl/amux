use std::fmt::Write;
use std::time::{Duration, SystemTime};

use crate::protocol::{ServerStatus, ServerView, SessionInfo};

const MIN_NAME_COLUMN: usize = 25;
const WINDOWS_COLUMN: usize = 12;
const COLUMN_GAP: usize = 2;
const TABLE_GAP: &str = "   ";
const SESSION_INDENT: &str = "  ";

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

        let stale = !matches!(
            server.status,
            ServerStatus::Local | ServerStatus::Online { .. }
        );
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
        "stale"
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
    use crate::protocol::{SessionId, Version, WindowSummary};

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
