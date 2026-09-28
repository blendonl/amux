use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use super::session::Session;
use super::{server_name, Server};
use crate::cluster;
use crate::paths;
use crate::settings::{ServerConfig, Settings};

const RESTART_REQUIRED: &str = "restart required for";

impl Server {
    pub(super) async fn reload_config(&self) -> Result<Option<String>> {
        let _reloading = self.reloading.lock().await;
        match self.reload().await {
            Ok(notice) => {
                info!(
                    notice = notice.as_deref().unwrap_or_default(),
                    "config reloaded"
                );
                Ok(notice)
            }
            Err(err) => {
                warn!("the config did not reload, keeping the running one: {err:#}");
                Err(err)
            }
        }
    }

    async fn reload(&self) -> Result<Option<String>> {
        let loaded = self.hooks.reload().await?;
        let loaded = loaded.expand_home(&paths::home_dir()?);
        let running = self.settings();
        let Reconciled {
            mut settings,
            mut notices,
        } = reconcile(loaded, &running, &self.identity.name);
        match cluster::with_env(settings.cluster.clone()) {
            Ok(tuned) => self.cluster.set_settings(tuned),
            Err(err) => {
                notices.push(format!("{err:#}"));
                settings.cluster.clone_from(&running.cluster);
            }
        }
        notices.extend(self.reconfigure_servers(&settings.servers));
        self.settings.send_replace(Arc::new(settings));
        let sessions: Vec<Arc<Session>> = self.state().sessions.values().cloned().collect();
        for session in sessions {
            session.redraw();
        }
        Ok((!notices.is_empty()).then(|| notices.join("; ")))
    }

    fn reconfigure_servers(&self, wanted: &BTreeMap<String, ServerConfig>) -> Vec<String> {
        let configured = self.cluster.configured_servers();
        let mut problems = Vec::new();
        for (name, server) in &configured {
            if wanted.get(name) != Some(server) {
                if let Err(err) = self.cluster.remove_server(name) {
                    problems.push(format!("amux.opt.servers.{name}: {err:#}"));
                }
            }
        }
        for (name, server) in wanted {
            if *name == self.identity.name || configured.get(name) == Some(server) {
                continue;
            }
            if let Err(err) = self.cluster.add_server(name.clone(), server.clone()) {
                problems.push(format!("amux.opt.servers.{name}: {err:#}"));
            }
        }
        problems
    }
}

#[derive(Debug, PartialEq)]
struct Reconciled {
    settings: Settings,
    notices: Vec<String>,
}

fn reconcile(mut settings: Settings, running: &Settings, name: &str) -> Reconciled {
    let mut kept = Vec::new();
    if settings.name != running.name && server_name(&settings).ok().as_deref() != Some(name) {
        kept.push("amux.opt.name");
        settings.name.clone_from(&running.name);
    }
    if settings.discovery != running.discovery {
        kept.push("amux.opt.discovery");
        settings.discovery.clone_from(&running.discovery);
    }
    if settings.lan != running.lan {
        kept.push("amux.opt.lan");
        settings.lan.clone_from(&running.lan);
    }
    let notices = if kept.is_empty() {
        Vec::new()
    } else {
        vec![format!("{RESTART_REQUIRED} {}", kept.join(", "))]
    };
    Reconciled { settings, notices }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Settings {
        Settings {
            name: Some(name.into()),
            ..Settings::default()
        }
    }

    #[test]
    fn live_settings_are_taken_as_loaded() {
        let mut loaded = named("desk");
        loaded.pane.term = "tmux-256color".into();
        loaded.cluster.backoff_max_ms = 5000;
        loaded.projects_dir = "/srv/projects".into();
        assert_eq!(
            reconcile(loaded.clone(), &named("desk"), "desk"),
            Reconciled {
                settings: loaded,
                notices: Vec::new(),
            }
        );
    }

    #[test]
    fn a_new_name_is_refused_and_the_rest_still_applies() {
        let mut loaded = named("laptop");
        loaded.window.base_index = 1;
        let Reconciled { settings, notices } = reconcile(loaded, &named("desk"), "desk");
        assert_eq!(settings.name.as_deref(), Some("desk"));
        assert_eq!(settings.window.base_index, 1);
        assert_eq!(notices, ["restart required for amux.opt.name"]);
    }

    #[test]
    fn naming_the_server_what_it_is_already_called_needs_no_restart() {
        let hostname = identity_hostname();
        let Reconciled { settings, notices } =
            reconcile(named(&hostname), &Settings::default(), &hostname);
        assert_eq!(settings.name, Some(hostname));
        assert_eq!(notices, Vec::<String>::new());
    }

    #[test]
    fn listener_and_discovery_changes_wait_for_a_restart() {
        let mut loaded = named("desk");
        loaded.lan.port = 7448;
        loaded.discovery.tailscale = false;
        loaded.discovery.tailscale_port = std::num::NonZeroU16::new(7500);
        loaded.cluster.ping_interval_ms = 1000;
        let Reconciled { settings, notices } = reconcile(loaded, &named("desk"), "desk");
        assert_eq!(settings.lan, named("desk").lan);
        assert_eq!(settings.discovery, named("desk").discovery);
        assert_eq!(settings.cluster.ping_interval_ms, 1000);
        assert_eq!(
            notices,
            ["restart required for amux.opt.discovery, amux.opt.lan"]
        );
    }

    fn identity_hostname() -> String {
        crate::identity::hostname().unwrap()
    }
}
