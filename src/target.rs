use std::cmp::Reverse;
use std::fmt;
use std::str::FromStr;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

const SERVER_SEPARATOR: char = '@';
const WINDOW_SEPARATOR: char = ':';
const PANE_SEPARATOR: char = '.';
const RESERVED_IN_SESSION_NAMES: [char; 2] = [SERVER_SEPARATOR, WINDOW_SEPARATOR];
const SANITIZED_REPLACEMENT: &str = "-";

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Target {
    pub session: Option<String>,
    pub server: Option<String>,
    pub window: Option<u32>,
    pub pane: Option<u32>,
}

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        parse_target(text).map_err(|reason| anyhow!("invalid target {text:?}: {reason}"))
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(session) = &self.session {
            f.write_str(session)?;
        }
        if let Some(server) = &self.server {
            write!(f, "{SERVER_SEPARATOR}{server}")?;
        }
        if self.window.is_some() || self.pane.is_some() {
            write!(f, "{WINDOW_SEPARATOR}")?;
        }
        if let Some(window) = self.window {
            write!(f, "{window}")?;
        }
        if let Some(pane) = self.pane {
            write!(f, "{PANE_SEPARATOR}{pane}")?;
        }
        Ok(())
    }
}

fn parse_target(text: &str) -> Result<Target> {
    let (address, position) = match text.split_once(WINDOW_SEPARATOR) {
        Some((address, position)) => (address, Some(position)),
        None => (text, None),
    };
    let (session, server) = parse_address(address)?;
    let (window, pane) = match position {
        Some(position) => parse_position(position)?,
        None => (None, None),
    };
    Ok(Target {
        session,
        server,
        window,
        pane,
    })
}

fn parse_address(address: &str) -> Result<(Option<String>, Option<String>)> {
    let Some((session, server)) = address.split_once(SERVER_SEPARATOR) else {
        return Ok((non_empty(address), None));
    };
    if server.contains(SERVER_SEPARATOR) {
        bail!("more than one '{SERVER_SEPARATOR}'");
    }
    if server.is_empty() {
        bail!("empty server name after '{SERVER_SEPARATOR}'");
    }
    Ok((non_empty(session), Some(server.to_owned())))
}

fn parse_position(position: &str) -> Result<(Option<u32>, Option<u32>)> {
    if position.contains(WINDOW_SEPARATOR) {
        bail!("more than one '{WINDOW_SEPARATOR}'");
    }
    let (window, pane) = match position.split_once(PANE_SEPARATOR) {
        Some((window, pane)) => (window, Some(pane)),
        None => (position, None),
    };
    let window = parse_index("window", window, WINDOW_SEPARATOR)?;
    let pane = pane
        .map(|pane| parse_index("pane", pane, PANE_SEPARATOR))
        .transpose()?;
    Ok((Some(window), pane))
}

fn parse_index(kind: &str, text: &str, separator: char) -> Result<u32> {
    if text.is_empty() {
        bail!("missing {kind} number after '{separator}'");
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("{kind} {text:?} is not a number");
    }
    text.parse()
        .map_err(|_| anyhow!("{kind} {text} is too large"))
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

pub fn sanitize_session_name(name: &str) -> String {
    name.replace(RESERVED_IN_SESSION_NAMES, SANITIZED_REPLACEMENT)
}

pub fn validate_session_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("the session name must not be empty");
    }
    if let Some(reserved) = name
        .chars()
        .find(|character| RESERVED_IN_SESSION_NAMES.contains(character))
    {
        bail!(
            "the session name {name:?} must not contain '{reserved}', \
             because targets use it to separate the server, window and pane"
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub server: String,
    pub session: String,
    pub last_activity: u64,
    pub is_local: bool,
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{SERVER_SEPARATOR}{}", self.session, self.server)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    UnknownServer {
        server: String,
    },
    NoSessionsOnServer {
        server: String,
    },
    SessionNotFound {
        session: String,
        server: Option<String>,
    },
    Ambiguous {
        session: String,
        matches: Vec<String>,
    },
    NoLocalSessions,
    NoSessions,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownServer { server } => write!(f, "unknown server: {server}"),
            Self::NoSessionsOnServer { server } => write!(f, "no sessions on server {server}"),
            Self::SessionNotFound {
                session,
                server: None,
            } => write!(f, "can't find session: {session}"),
            Self::SessionNotFound {
                session,
                server: Some(server),
            } => write!(f, "can't find session: {session}{SERVER_SEPARATOR}{server}"),
            Self::Ambiguous { session, matches } => write!(
                f,
                "session {session} is on more than one server, pick one of: {}",
                matches.join(", ")
            ),
            Self::NoLocalSessions => f.write_str("no sessions on this server"),
            Self::NoSessions => f.write_str("no sessions"),
        }
    }
}

impl std::error::Error for ResolveError {}

pub fn resolve<'a>(target: &Target, candidates: &'a [Candidate]) -> Result<&'a Candidate> {
    resolve_with_servers(target, &[] as &[&str], candidates)
}

pub fn resolve_with_servers<'a, S: AsRef<str>>(
    target: &Target,
    servers: &[S],
    candidates: &'a [Candidate],
) -> Result<&'a Candidate> {
    Ok(find(target, servers, candidates)?)
}

fn find<'a, S: AsRef<str>>(
    target: &Target,
    servers: &[S],
    candidates: &'a [Candidate],
) -> Result<&'a Candidate, ResolveError> {
    match (&target.session, &target.server) {
        (Some(session), Some(server)) => {
            ensure_known(server, servers, candidates)?;
            candidates
                .iter()
                .find(|candidate| candidate.server == *server && candidate.session == *session)
                .ok_or_else(|| ResolveError::SessionNotFound {
                    session: session.clone(),
                    server: Some(server.clone()),
                })
        }
        (Some(session), None) => find_unique(session, candidates),
        (None, Some(server)) => {
            ensure_known(server, servers, candidates)?;
            most_recent(
                candidates
                    .iter()
                    .filter(|candidate| candidate.server == *server),
            )
            .ok_or_else(|| ResolveError::NoSessionsOnServer {
                server: server.clone(),
            })
        }
        (None, None) => most_recent(candidates.iter().filter(|candidate| candidate.is_local))
            .ok_or(if candidates.is_empty() {
                ResolveError::NoSessions
            } else {
                ResolveError::NoLocalSessions
            }),
    }
}

fn ensure_known<S: AsRef<str>>(
    server: &str,
    servers: &[S],
    candidates: &[Candidate],
) -> Result<(), ResolveError> {
    let listed = servers.iter().any(|known| known.as_ref() == server);
    let hosting = candidates
        .iter()
        .any(|candidate| candidate.server == server);
    if listed || hosting {
        Ok(())
    } else {
        Err(ResolveError::UnknownServer {
            server: server.to_owned(),
        })
    }
}

fn find_unique<'a>(
    session: &str,
    candidates: &'a [Candidate],
) -> Result<&'a Candidate, ResolveError> {
    let matches: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.session == session)
        .collect();
    match matches.as_slice() {
        [] => Err(ResolveError::SessionNotFound {
            session: session.to_owned(),
            server: None,
        }),
        [only] => Ok(only),
        several => {
            let mut matches: Vec<String> = several.iter().map(ToString::to_string).collect();
            matches.sort();
            Err(ResolveError::Ambiguous {
                session: session.to_owned(),
                matches,
            })
        }
    }
}

fn most_recent<'a>(candidates: impl Iterator<Item = &'a Candidate>) -> Option<&'a Candidate> {
    candidates.max_by_key(|candidate| {
        (
            candidate.last_activity,
            Reverse(&candidate.server),
            Reverse(&candidate.session),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type Row = (
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        Option<u32>,
        Option<u32>,
    );

    const CANONICAL_FORMS: &[Row] = &[
        ("", None, None, None, None),
        ("amux/main", Some("amux/main"), None, None, None),
        (
            "amux/main@desktop",
            Some("amux/main"),
            Some("desktop"),
            None,
            None,
        ),
        ("@laptop", None, Some("laptop"), None, None),
        (
            "amux/main@desktop:2.1",
            Some("amux/main"),
            Some("desktop"),
            Some(2),
            Some(1),
        ),
        (":2", None, None, Some(2), None),
        ("@desktop:1", None, Some("desktop"), Some(1), None),
        ("amux/main:2", Some("amux/main"), None, Some(2), None),
        ("amux/main:2.1", Some("amux/main"), None, Some(2), Some(1)),
        (":2.1", None, None, Some(2), Some(1)),
        ("@desktop:0.0", None, Some("desktop"), Some(0), Some(0)),
        ("0", Some("0"), None, None, None),
        (
            "amux/v1.2@desk.lan:3.4",
            Some("amux/v1.2"),
            Some("desk.lan"),
            Some(3),
            Some(4),
        ),
        (
            ":4294967295.4294967295",
            None,
            None,
            Some(u32::MAX),
            Some(u32::MAX),
        ),
    ];

    fn target(
        session: Option<&str>,
        server: Option<&str>,
        window: Option<u32>,
        pane: Option<u32>,
    ) -> Target {
        Target {
            session: session.map(Into::into),
            server: server.map(Into::into),
            window,
            pane,
        }
    }

    fn canonical_targets() -> impl Iterator<Item = (&'static str, Target)> {
        CANONICAL_FORMS
            .iter()
            .map(|&(text, session, server, window, pane)| {
                (text, target(session, server, window, pane))
            })
    }

    #[test]
    fn every_syntax_form_parses() {
        for (text, expected) in canonical_targets() {
            assert_eq!(text.parse::<Target>().unwrap(), expected, "{text:?}");
        }
    }

    #[test]
    fn canonical_forms_format_back_to_themselves() {
        for (text, parsed) in canonical_targets() {
            assert_eq!(parsed.to_string(), text);
        }
    }

    #[test]
    fn parsing_a_formatted_target_gives_it_back() {
        for (_, original) in canonical_targets() {
            let reparsed: Target = original.to_string().parse().unwrap();
            assert_eq!(reparsed, original);
        }
    }

    #[test]
    fn leading_zeros_parse_and_format_without_them() {
        let cases = [
            ("a:007", "a:7"),
            ("a:01.02", "a:1.2"),
            ("@x:00.000", "@x:0.0"),
        ];
        for (text, canonical) in cases {
            let parsed: Target = text.parse().unwrap();
            assert_eq!(parsed.to_string(), canonical, "{text:?}");
            assert_eq!(canonical.parse::<Target>().unwrap(), parsed, "{text:?}");
        }
    }

    #[test]
    fn a_pane_without_a_window_formats_but_is_not_valid_syntax() {
        let pane_only = target(Some("a"), None, None, Some(1));
        assert_eq!(pane_only.to_string(), "a:.1");
        assert!(pane_only.to_string().parse::<Target>().is_err());
    }

    #[test]
    fn malformed_targets_are_rejected_with_the_reason() {
        let cases = [
            ("@", "empty server name after '@'"),
            ("amux/main@", "empty server name after '@'"),
            ("amux/main@:1", "empty server name after '@'"),
            ("a@b@c", "more than one '@'"),
            ("@@desktop", "more than one '@'"),
            ("a@b@", "more than one '@'"),
            ("a@b@c:1", "more than one '@'"),
            ("a:1:2", "more than one ':'"),
            ("a::1", "more than one ':'"),
            ("a:x", "window \"x\" is not a number"),
            ("a:-1", "window \"-1\" is not a number"),
            ("a:+1", "window \"+1\" is not a number"),
            ("a: 1", "window \" 1\" is not a number"),
            ("a:1.y", "pane \"y\" is not a number"),
            ("a:1.2.3", "pane \"2.3\" is not a number"),
            ("a:1.+2", "pane \"+2\" is not a number"),
            ("a:", "missing window number after ':'"),
            (":", "missing window number after ':'"),
            ("@desktop:", "missing window number after ':'"),
            (":.1", "missing window number after ':'"),
            ("a:.", "missing window number after ':'"),
            ("a:1.", "missing pane number after '.'"),
            ("@desktop:2.", "missing pane number after '.'"),
            ("a:4294967296", "window 4294967296 is too large"),
            ("a:1.4294967296", "pane 4294967296 is too large"),
        ];
        for (text, reason) in cases {
            let err = text.parse::<Target>().unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("invalid target {text:?}: {reason}"),
                "{text:?}"
            );
        }
    }

    #[test]
    fn targets_survive_the_wire() {
        for (text, original) in canonical_targets() {
            let bytes = postcard::to_stdvec(&original).unwrap();
            let decoded: Target = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(decoded, original, "{text:?}");
        }
    }

    #[test]
    fn sanitizing_replaces_the_target_separators() {
        let cases = [
            ("amux/main", "amux/main"),
            ("amux/feature-x", "amux/feature-x"),
            ("user@host", "user-host"),
            ("a:b", "a-b"),
            ("@:", "--"),
            ("feature/x@y:z", "feature/x-y-z"),
            ("v1.2", "v1.2"),
            ("", ""),
        ];
        for (name, sanitized) in cases {
            assert_eq!(sanitize_session_name(name), sanitized, "{name:?}");
        }
    }

    #[test]
    fn sanitized_names_are_valid_and_address_themselves() {
        for name in ["user@host", "a:b:c", "@x", "x:", "amux/fix@2:3.4"] {
            let sanitized = sanitize_session_name(name);
            validate_session_name(&sanitized).unwrap();
            let parsed: Target = sanitized.parse().unwrap();
            assert_eq!(parsed, target(Some(&sanitized), None, None, None));
        }
    }

    #[test]
    fn user_given_session_names_are_validated() {
        for name in ["amux/main", "0", "v1.2", "feature-x", "notes"] {
            assert!(validate_session_name(name).is_ok(), "{name:?}");
        }

        let cases = [
            ("", "the session name must not be empty"),
            ("a@b", "the session name \"a@b\" must not contain '@'"),
            ("a:b", "the session name \"a:b\" must not contain ':'"),
            ("@", "the session name \"@\" must not contain '@'"),
            (":x@y", "the session name \":x@y\" must not contain ':'"),
            ("x@y:z", "the session name \"x@y:z\" must not contain '@'"),
        ];
        for (name, message) in cases {
            let err = validate_session_name(name).unwrap_err().to_string();
            assert!(err.starts_with(message), "{name:?}: {err}");
        }
    }

    fn candidate(server: &str, session: &str, last_activity: u64, is_local: bool) -> Candidate {
        Candidate {
            server: server.into(),
            session: session.into(),
            last_activity,
            is_local,
        }
    }

    fn cluster() -> Vec<Candidate> {
        vec![
            candidate("laptop", "amux/main", 90, false),
            candidate("laptop", "scratch", 30, false),
            candidate("home-server", "infra/main", 10, false),
            candidate("home-server", "amux/main", 5, false),
            candidate("home-server", "infra/dev", 20, false),
            candidate("desktop", "amux/main", 50, true),
            candidate("desktop", "notes/main", 70, true),
        ]
    }

    fn resolve_text<'a>(text: &str, candidates: &'a [Candidate]) -> Result<&'a Candidate> {
        resolve(&text.parse().unwrap(), candidates)
    }

    fn failure(result: Result<&Candidate>) -> (ResolveError, String) {
        let err = result.unwrap_err();
        let resolve_error = err
            .downcast_ref::<ResolveError>()
            .expect("resolution fails with a ResolveError")
            .clone();
        (resolve_error, err.to_string())
    }

    #[test]
    fn candidates_display_as_session_at_server() {
        assert_eq!(
            candidate("desktop", "amux/main", 0, true).to_string(),
            "amux/main@desktop"
        );
    }

    #[test]
    fn every_resolution_rule_picks_the_documented_session() {
        let cases = [
            ("notes/main", "notes/main@desktop"),
            ("scratch", "scratch@laptop"),
            ("infra/dev", "infra/dev@home-server"),
            ("amux/main@desktop", "amux/main@desktop"),
            ("amux/main@laptop", "amux/main@laptop"),
            ("amux/main@home-server", "amux/main@home-server"),
            ("@laptop", "amux/main@laptop"),
            ("@home-server", "infra/dev@home-server"),
            ("@desktop", "notes/main@desktop"),
            ("", "notes/main@desktop"),
        ];
        let candidates = cluster();
        for (text, expected) in cases {
            let resolved = resolve_text(text, &candidates).unwrap();
            assert_eq!(resolved.to_string(), expected, "{text:?}");
        }
    }

    #[test]
    fn window_and_pane_do_not_change_the_resolution() {
        let candidates = cluster();
        let cases = [
            ("notes/main:2.1", "notes/main"),
            ("amux/main@laptop:3", "amux/main@laptop"),
            ("@home-server:0.4", "@home-server"),
            (":1", ""),
            (":1.2", ""),
        ];
        for (with_position, without) in cases {
            let target: Target = with_position.parse().unwrap();
            let resolved = resolve(&target, &candidates).unwrap();
            assert_eq!(resolved, resolve_text(without, &candidates).unwrap());
            assert_eq!(target.to_string(), with_position);
        }
    }

    #[test]
    fn every_resolution_error_is_distinct_and_readable() {
        let cases = [
            (
                "amux/main",
                ResolveError::Ambiguous {
                    session: "amux/main".into(),
                    matches: vec![
                        "amux/main@desktop".into(),
                        "amux/main@home-server".into(),
                        "amux/main@laptop".into(),
                    ],
                },
                "session amux/main is on more than one server, pick one of: \
                 amux/main@desktop, amux/main@home-server, amux/main@laptop",
            ),
            (
                "amux/main:2.1",
                ResolveError::Ambiguous {
                    session: "amux/main".into(),
                    matches: vec![
                        "amux/main@desktop".into(),
                        "amux/main@home-server".into(),
                        "amux/main@laptop".into(),
                    ],
                },
                "session amux/main is on more than one server, pick one of: \
                 amux/main@desktop, amux/main@home-server, amux/main@laptop",
            ),
            (
                "nope",
                ResolveError::SessionNotFound {
                    session: "nope".into(),
                    server: None,
                },
                "can't find session: nope",
            ),
            (
                "nope@desktop",
                ResolveError::SessionNotFound {
                    session: "nope".into(),
                    server: Some("desktop".into()),
                },
                "can't find session: nope@desktop",
            ),
            (
                "notes/main@laptop",
                ResolveError::SessionNotFound {
                    session: "notes/main".into(),
                    server: Some("laptop".into()),
                },
                "can't find session: notes/main@laptop",
            ),
            (
                "amux/main@nas",
                ResolveError::UnknownServer {
                    server: "nas".into(),
                },
                "unknown server: nas",
            ),
            (
                "@nas",
                ResolveError::UnknownServer {
                    server: "nas".into(),
                },
                "unknown server: nas",
            ),
            (
                "@nas:1",
                ResolveError::UnknownServer {
                    server: "nas".into(),
                },
                "unknown server: nas",
            ),
        ];
        let candidates = cluster();
        for (text, expected, message) in cases {
            let (error, text_error) = failure(resolve_text(text, &candidates));
            assert_eq!(error, expected, "{text:?}");
            assert_eq!(text_error, message, "{text:?}");
        }
    }

    #[test]
    fn the_default_target_needs_a_local_session() {
        let remote_only: Vec<Candidate> = cluster()
            .into_iter()
            .filter(|candidate| !candidate.is_local)
            .collect();
        let cases = [
            (Vec::new(), ResolveError::NoSessions, "no sessions"),
            (
                remote_only,
                ResolveError::NoLocalSessions,
                "no sessions on this server",
            ),
        ];
        for (candidates, expected, message) in cases {
            for text in ["", ":1", ":1.2"] {
                let (error, text_error) = failure(resolve_text(text, &candidates));
                assert_eq!(error, expected, "{text:?}");
                assert_eq!(text_error, message, "{text:?}");
            }
        }
    }

    #[test]
    fn an_empty_cluster_fails_every_rule() {
        let cases = [
            (
                "amux/main",
                ResolveError::SessionNotFound {
                    session: "amux/main".into(),
                    server: None,
                },
            ),
            (
                "amux/main@desktop",
                ResolveError::UnknownServer {
                    server: "desktop".into(),
                },
            ),
            (
                "@desktop",
                ResolveError::UnknownServer {
                    server: "desktop".into(),
                },
            ),
            ("", ResolveError::NoSessions),
        ];
        for (text, expected) in cases {
            assert_eq!(failure(resolve_text(text, &[])).0, expected, "{text:?}");
        }
    }

    #[test]
    fn listed_servers_without_sessions_are_known() {
        let candidates = cluster();
        let servers: Vec<String> = ["desktop", "laptop", "home-server", "nas"]
            .map(String::from)
            .to_vec();
        let resolve_listed =
            |text: &str| resolve_with_servers(&text.parse().unwrap(), &servers, &candidates);

        let cases = [
            (
                "@nas",
                ResolveError::NoSessionsOnServer {
                    server: "nas".into(),
                },
                "no sessions on server nas",
            ),
            (
                "@nas:2.1",
                ResolveError::NoSessionsOnServer {
                    server: "nas".into(),
                },
                "no sessions on server nas",
            ),
            (
                "amux/main@nas",
                ResolveError::SessionNotFound {
                    session: "amux/main".into(),
                    server: Some("nas".into()),
                },
                "can't find session: amux/main@nas",
            ),
            (
                "@cloud",
                ResolveError::UnknownServer {
                    server: "cloud".into(),
                },
                "unknown server: cloud",
            ),
        ];
        for (text, expected, message) in cases {
            let (error, text_error) = failure(resolve_listed(text));
            assert_eq!(error, expected, "{text:?}");
            assert_eq!(text_error, message, "{text:?}");
        }

        assert_eq!(
            resolve_listed("@laptop").unwrap().to_string(),
            "amux/main@laptop"
        );
        assert_eq!(
            resolve_listed("scratch").unwrap().to_string(),
            "scratch@laptop"
        );
    }

    #[test]
    fn servers_hosting_sessions_are_known_without_being_listed() {
        let candidates = cluster();
        let resolved =
            resolve_with_servers(&"@laptop".parse().unwrap(), &["nas"], &candidates).unwrap();
        assert_eq!(resolved.to_string(), "amux/main@laptop");
    }

    #[test]
    fn equally_recent_sessions_resolve_the_same_whatever_the_order() {
        let tied = vec![
            candidate("desktop", "b", 40, true),
            candidate("desktop", "a", 40, true),
            candidate("desktop", "c", 40, true),
            candidate("desktop", "old", 10, true),
        ];
        let mut reversed = tied.clone();
        reversed.reverse();

        for text in ["", "@desktop"] {
            assert_eq!(resolve_text(text, &tied).unwrap().session, "a", "{text:?}");
            assert_eq!(
                resolve_text(text, &reversed).unwrap().session,
                "a",
                "{text:?}"
            );
        }
    }

    #[test]
    fn the_most_recent_session_wins_regardless_of_its_position() {
        let candidates = vec![
            candidate("desktop", "old", 1, true),
            candidate("desktop", "new", 100, true),
            candidate("desktop", "middle", 50, true),
        ];
        assert_eq!(resolve_text("", &candidates).unwrap().session, "new");
        assert_eq!(
            resolve_text("@desktop", &candidates).unwrap().session,
            "new"
        );
    }
}
