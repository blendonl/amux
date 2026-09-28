use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{bail, Context, Result};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tracing::{debug, info, warn};

use super::session::{Binding, Session};
use super::{Server, SessionName, SessionSpec};
use crate::project::{self, Project, ProjectId, Registry};
use crate::protocol::{NewSession, ProjectCheckout, ProjectRef, StateEvent};
use crate::target;

pub const REGISTRY_FILE: &str = "projects.toml";

type WorktreeKey = (ProjectId, String);

pub struct Projects {
    registry: AsyncMutex<Registry>,
    creating: Mutex<HashMap<WorktreeKey, Arc<AsyncMutex<()>>>>,
}

impl Projects {
    pub fn new(registry: Registry) -> Self {
        Self {
            registry: AsyncMutex::new(registry),
            creating: Mutex::default(),
        }
    }

    async fn lock_worktree(&self, project: &ProjectId, branch: &str) -> WorktreeLock<'_> {
        let key = (project.clone(), branch.to_owned());
        let lock = Arc::clone(self.creating().entry(key.clone()).or_default());
        let guard = lock.lock_owned().await;
        WorktreeLock {
            projects: self,
            key,
            _guard: guard,
        }
    }

    fn creating(&self) -> MutexGuard<'_, HashMap<WorktreeKey, Arc<AsyncMutex<()>>>> {
        self.creating.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct WorktreeLock<'a> {
    projects: &'a Projects,
    key: WorktreeKey,
    _guard: OwnedMutexGuard<()>,
}

impl Drop for WorktreeLock<'_> {
    fn drop(&mut self) {
        let mut creating = self.projects.creating();
        if creating
            .get(&self.key)
            .is_some_and(|lock| Arc::strong_count(lock) <= 2)
        {
            creating.remove(&self.key);
        }
    }
}

pub fn checkouts(registry: &Registry) -> Vec<ProjectCheckout> {
    registry.projects().iter().map(checkout).collect()
}

fn checkout(project: &Project) -> ProjectCheckout {
    ProjectCheckout {
        id: project.id.clone(),
        name: project.name.clone(),
        path: project.path.clone(),
        origin: project.url.clone(),
    }
}

impl Server {
    pub(super) async fn register_project(&self, path: PathBuf) -> Result<ProjectCheckout> {
        let project = self
            .update_registry(move |registry| registry.add(&path))
            .await?;
        info!(project = %project.id, path = %project.path.display(), "project registered");
        Ok(checkout(&project))
    }

    pub(super) async fn create_project_session(
        self: &Arc<Self>,
        request: &NewSession,
        project: &ProjectRef,
    ) -> Result<Arc<Session>> {
        let checkout = self
            .checkout_of(project, request.cwd.as_deref(), request.clone)
            .await?;
        let branch = match &request.branch {
            Some(branch) => branch.clone(),
            None => {
                let path = checkout.path.clone();
                blocking(move || project::default_branch(&path)).await?
            }
        };

        let _lock = self.projects.lock_worktree(&checkout.id, &branch).await;
        if let Some(session) = self.session_for_worktree(&checkout.id, &branch) {
            info!(session = %session.name(), %branch, "reusing the worktree's session");
            return Ok(session);
        }
        if request.branch.is_some() {
            let (path, wanted) = (checkout.path.clone(), branch.clone());
            let timeout = self.settings.worktrees.fetch_timeout();
            if let Err(err) = blocking(move || project::fetch_branch(&path, &wanted, timeout)).await
            {
                debug!(%branch, "fetching the branch from origin failed: {err:#}");
            }
        }
        let worktrees_dir = self.worktrees_dir(project, &checkout);
        let (path, wanted) = (checkout.path.clone(), branch.clone());
        let worktree =
            blocking(move || project::ensure_worktree(&path, &wanted, &worktrees_dir)).await?;

        let name = match &request.name {
            Some(name) => SessionName::Given(name.clone()),
            None => SessionName::Derived(target::sanitize_session_name(&format!(
                "{}/{branch}",
                project.name
            ))),
        };
        self.spawn_session(SessionSpec {
            name,
            cwd: &worktree,
            size: request.size,
            env: &request.env,
            binding: Some(Binding {
                project: checkout.id,
                branch,
                checkout: checkout.path,
                worktree: worktree.clone(),
            }),
        })
    }

    pub(super) async fn kill_and_remove_worktree(&self, session: &Arc<Session>) -> Result<()> {
        let binding = session.binding().cloned().with_context(|| {
            format!(
                "session {} is not bound to a project worktree",
                session.name()
            )
        })?;
        let _lock = self
            .projects
            .lock_worktree(&binding.project, &binding.branch)
            .await;
        let (checkout, worktree) = (binding.checkout.clone(), binding.worktree.clone());
        blocking(move || project::check_removable(&checkout, &worktree)).await?;
        self.kill_session(session);
        let Binding {
            checkout, worktree, ..
        } = binding;
        blocking(move || project::remove_worktree(&checkout, &worktree)).await
    }

    pub(super) fn default_server(&self, project: Option<&ProjectRef>) -> Option<&str> {
        self.config
            .projects
            .get(&project?.name)?
            .default_server
            .as_deref()
    }

    async fn checkout_of(
        &self,
        project: &ProjectRef,
        cwd: Option<&Path>,
        clone: bool,
    ) -> Result<ProjectCheckout> {
        if let Some(known) = self.known_checkout(&project.id) {
            return Ok(known);
        }
        if let Some(cwd) = cwd {
            if let Some(found) = self.register_from_cwd(project, cwd).await? {
                return Ok(found);
            }
        }
        if clone {
            return self.clone_project(project).await;
        }
        let host = &self.identity.name;
        bail!(
            "{host} has no checkout of project {} ({}); add --clone to clone it on {host}, \
             or run `amux project add <path>` on {host}",
            project.name,
            project.id
        )
    }

    fn known_checkout(&self, id: &ProjectId) -> Option<ProjectCheckout> {
        self.state()
            .projects
            .iter()
            .find(|known| known.id == *id)
            .cloned()
    }

    async fn register_from_cwd(
        &self,
        project: &ProjectRef,
        cwd: &Path,
    ) -> Result<Option<ProjectCheckout>> {
        let (id, cwd) = (project.id.clone(), cwd.to_owned());
        let registered = self
            .update_registry(move |registry| match project::detect(&cwd)? {
                Some(detected) if detected.id == id => registry.add(&detected.checkout).map(Some),
                _ => Ok(None),
            })
            .await?;
        Ok(registered.map(|project| {
            info!(project = %project.id, path = %project.path.display(), "project registered on create");
            checkout(&project)
        }))
    }

    async fn clone_project(&self, project: &ProjectRef) -> Result<ProjectCheckout> {
        let url = project.origin.clone().with_context(|| {
            format!(
                "project {} has no origin remote to clone it from",
                project.name
            )
        })?;
        if !is_single_component(&project.name) {
            bail!(
                "cannot clone project {:?}: its name is not a directory name",
                project.name
            );
        }
        let dest = self.config.projects_dir.join(&project.name);
        let id = project.id.clone();
        info!(%url, dest = %dest.display(), "cloning project");
        let cloned = self
            .update_registry(move |registry| {
                if let Some(known) = registry.get(&id) {
                    return Ok(known.clone());
                }
                let path = project::clone(&url, &dest)?;
                let added = registry.add(&path)?;
                if added.id != id {
                    warn!(expected = %id, found = %added.id, "the clone is a different project");
                }
                Ok(added)
            })
            .await?;
        Ok(checkout(&cloned))
    }

    fn worktrees_dir(&self, project: &ProjectRef, checkout: &ProjectCheckout) -> PathBuf {
        [&project.name, &checkout.name]
            .into_iter()
            .find_map(|name| self.config.projects.get(name)?.worktrees_dir.clone())
            .unwrap_or_else(|| {
                project::default_worktrees_dir(
                    &checkout.path,
                    &checkout.name,
                    &self.settings.worktrees.suffix,
                )
            })
    }

    fn session_for_worktree(&self, project: &ProjectId, branch: &str) -> Option<Arc<Session>> {
        self.state()
            .sessions
            .values()
            .find(|session| {
                session
                    .binding()
                    .is_some_and(|binding| binding.is_for(project, branch))
            })
            .cloned()
    }

    async fn update_registry<T, F>(&self, change: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Registry) -> Result<T> + Send + 'static,
    {
        let mut registry = self.projects.registry.lock().await;
        let mut updated = registry.clone();
        let before = registry.clone();
        let (updated, result) = blocking(move || {
            let result = change(&mut updated)?;
            if updated != before {
                updated.save()?;
            }
            Ok((updated, result))
        })
        .await?;
        *registry = updated;
        let projects = checkouts(&registry);
        let mut state = self.state();
        if state.projects != projects {
            state.projects = projects.clone();
            self.publish(&mut state, StateEvent::ProjectsChanged(projects));
        }
        Ok(result)
    }
}

fn is_single_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

pub async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .context("a blocking task panicked")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_destinations_must_be_one_directory_name() {
        assert!(is_single_component("amux"));
        assert!(is_single_component("my.project"));
        for name in ["", ".", "..", "a/b", "/abs", "../up"] {
            assert!(!is_single_component(name), "accepted {name:?}");
        }
    }

    #[tokio::test]
    async fn worktree_locks_serialize_one_key_and_clean_up_after_the_last_holder() {
        let projects = Arc::new(Projects::new(
            Registry::load(Path::new("/nonexistent/projects.toml")).unwrap(),
        ));
        let id = ProjectId::from("github.com/example/amux");

        let first = projects.lock_worktree(&id, "main").await;
        let other_branch = projects.lock_worktree(&id, "feature").await;
        let waiting = {
            let projects = Arc::clone(&projects);
            let id = id.clone();
            tokio::spawn(async move {
                let _second = projects.lock_worktree(&id, "main").await;
            })
        };
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        assert_eq!(projects.creating().len(), 2);

        drop(first);
        waiting.await.unwrap();
        drop(other_branch);
        assert!(projects.creating().is_empty());
    }
}
