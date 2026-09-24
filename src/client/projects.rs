use std::collections::BTreeMap;

use anyhow::{bail, Result};

use crate::project::{Detected, ProjectId};
use crate::protocol::{ProjectRef, ServerView};

pub fn find(servers: &[ServerView], wanted: &str) -> Result<Option<ProjectRef>> {
    let mut matches: BTreeMap<&ProjectId, (ProjectRef, Vec<&str>)> = BTreeMap::new();
    for server in servers {
        for checkout in &server.projects {
            if checkout.name != wanted && checkout.id.as_str() != wanted {
                continue;
            }
            let (project, hosts) = matches.entry(&checkout.id).or_insert_with(|| {
                (
                    ProjectRef {
                        id: checkout.id.clone(),
                        name: checkout.name.clone(),
                        origin: None,
                    },
                    Vec::new(),
                )
            });
            if project.origin.is_none() {
                project.origin.clone_from(&checkout.origin);
            }
            hosts.push(&server.name);
        }
    }
    if matches.len() > 1 {
        let choices: Vec<String> = matches
            .into_iter()
            .map(|(id, (_, hosts))| format!("{id} (on {})", hosts.join(", ")))
            .collect();
        bail!(
            "project {wanted} is ambiguous, pass one of these ids to -p instead: {}",
            choices.join(", ")
        );
    }
    Ok(matches.into_values().next().map(|(project, _)| project))
}

pub fn from_detected(detected: &Detected) -> ProjectRef {
    ProjectRef {
        id: detected.id.clone(),
        name: detected.name.clone(),
        origin: detected.url.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::protocol::{ProjectCheckout, ServerStatus};

    fn checkout(id: &str, name: &str, origin: Option<&str>) -> ProjectCheckout {
        ProjectCheckout {
            id: id.into(),
            name: name.into(),
            path: PathBuf::from("/src").join(name),
            origin: origin.map(Into::into),
        }
    }

    fn server(name: &str, projects: Vec<ProjectCheckout>) -> ServerView {
        ServerView {
            id: None,
            name: name.into(),
            address: None,
            version: None,
            status: ServerStatus::Local,
            sessions: Vec::new(),
            projects,
        }
    }

    const AMUX: &str = "github.com/blendonl/amux";
    const FORK: &str = "github.com/someone/amux";

    #[test]
    fn a_name_checked_out_on_several_servers_is_one_project() {
        let servers = [
            server("desk", vec![checkout(AMUX, "amux", None)]),
            server(
                "laptop",
                vec![checkout(
                    AMUX,
                    "amux",
                    Some("git@github.com:blendonl/amux.git"),
                )],
            ),
        ];

        let found = find(&servers, "amux").unwrap().unwrap();

        assert_eq!(
            found,
            ProjectRef {
                id: AMUX.into(),
                name: "amux".into(),
                origin: Some("git@github.com:blendonl/amux.git".into()),
            }
        );
        assert_eq!(find(&servers, "notes").unwrap(), None);
    }

    #[test]
    fn a_name_shared_by_two_projects_fails_and_ids_pick_one() {
        let servers = [
            server("desk", vec![checkout(AMUX, "amux", None)]),
            server("laptop", vec![checkout(FORK, "amux", None)]),
        ];

        let err = find(&servers, "amux").unwrap_err();

        assert_eq!(
            err.to_string(),
            format!(
                "project amux is ambiguous, pass one of these ids to -p instead: \
                 {AMUX} (on desk), {FORK} (on laptop)"
            )
        );
        assert_eq!(find(&servers, FORK).unwrap().unwrap().id.as_str(), FORK);
    }
}
