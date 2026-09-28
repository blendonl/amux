use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::git::{self, GitError};
use super::id::{self, ProjectId};
use super::{canonical, worktree};

const ORIGIN: &str = "origin";
const GIT_DIR_NAME: &str = ".git";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub id: ProjectId,
    pub name: String,
    pub url: Option<String>,
    pub branch: Option<String>,
    pub worktree_root: PathBuf,
    pub checkout: PathBuf,
}

pub fn detect(cwd: &Path) -> Result<Option<Detected>> {
    if !inside_work_tree(cwd)? {
        return Ok(None);
    }
    let paths = git::run(cwd, ["rev-parse", "--show-toplevel", "--git-common-dir"])?;
    let mut lines = paths.lines();
    let (Some(toplevel), Some(common_dir)) = (lines.next(), lines.next()) else {
        bail!(
            "unexpected `git rev-parse` output in {}: {paths:?}",
            cwd.display()
        );
    };
    let worktree_root = canonical(Path::new(toplevel))?;
    let common_dir = canonical(&cwd.join(common_dir))?;
    let checkout = main_checkout(&worktree_root, &common_dir)?;

    let url = origin_url(&checkout)?;
    let id = match &url {
        Some(url) => ProjectId::from_remote_url(url),
        None => match ProjectId::from_root_commit(&checkout)? {
            Some(id) => id,
            None => return Ok(None),
        },
    };
    let name = checkout
        .file_name()
        .map(OsStr::to_string_lossy)
        .with_context(|| format!("{} has no directory name", checkout.display()))?
        .into_owned();

    Ok(Some(Detected {
        id,
        name,
        url,
        branch: current_branch(&worktree_root)?,
        worktree_root,
        checkout,
    }))
}

pub(super) fn current_branch(worktree: &Path) -> Result<Option<String>> {
    let branch = git::run(worktree, ["branch", "--show-current"])?;
    Ok(Some(branch).filter(|branch| !branch.is_empty()))
}

fn inside_work_tree(cwd: &Path) -> Result<bool> {
    match git::run(cwd, ["rev-parse", "--is-inside-work-tree"]) {
        Ok(answer) => Ok(answer == "true"),
        Err(err) if git::is_not_installed(&err) => Ok(false),
        Err(err)
            if err
                .downcast_ref::<GitError>()
                .is_some_and(GitError::is_not_a_repository) =>
        {
            Ok(false)
        }
        Err(err) => Err(err),
    }
}

fn main_checkout(worktree_root: &Path, common_dir: &Path) -> Result<PathBuf> {
    if common_dir.file_name() == Some(OsStr::new(GIT_DIR_NAME)) {
        if let Some(parent) = common_dir.parent() {
            return Ok(parent.to_owned());
        }
    }
    let main = worktree::list(worktree_root)?
        .into_iter()
        .next()
        .with_context(|| format!("git lists no worktrees for {}", worktree_root.display()))?;
    canonical(&main.path)
}

fn origin_url(checkout: &Path) -> Result<Option<String>> {
    let remotes = git::run(checkout, ["remote"])?;
    if !remotes.lines().any(|remote| remote == ORIGIN) {
        return Ok(None);
    }
    let url = git::run(checkout, ["remote", "get-url", ORIGIN])?;
    Ok(Some(id::resolve_relative_local_url(&url, checkout)))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::testing::{commit, git, head, path_str, Fixture};
    use super::*;

    #[test]
    fn detection_from_a_subdirectory_finds_the_checkout() {
        let fixture = Fixture::new();
        let origin = fixture.origin();
        let checkout = fixture.clone_of(&origin, "amux");
        let deep = checkout.join("src/deep");
        fs::create_dir_all(&deep).unwrap();

        let detected = detect(&deep).unwrap().unwrap();

        assert_eq!(
            detected,
            Detected {
                id: ProjectId::from_remote_url(path_str(&origin)),
                name: "amux".into(),
                url: Some(path_str(&origin).into()),
                branch: Some("main".into()),
                worktree_root: checkout.clone(),
                checkout,
            }
        );
        assert_eq!(detected.id.as_str(), path_str(&origin));
    }

    #[test]
    fn detection_from_a_linked_worktree_reports_the_main_checkout() {
        let fixture = Fixture::new();
        let origin = fixture.origin();
        let checkout = fixture.clone_of(&origin, "amux");
        let linked = fixture.path("elsewhere/feature");
        git(
            &checkout,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature",
                path_str(&linked),
            ],
        );
        let inside = linked.join("nested");
        fs::create_dir_all(&inside).unwrap();

        let detected = detect(&inside).unwrap().unwrap();

        assert_eq!(detected.checkout, checkout);
        assert_eq!(detected.worktree_root, linked);
        assert_eq!(detected.name, "amux");
        assert_eq!(detected.branch.as_deref(), Some("feature"));
        assert_eq!(detected.id, detect(&checkout).unwrap().unwrap().id);
    }

    #[test]
    fn a_detached_head_is_unbound() {
        let fixture = Fixture::new();
        let repo = fixture.repo("repo");
        git(&repo, &["checkout", "--quiet", "--detach"]);

        let detected = detect(&repo).unwrap().unwrap();

        assert_eq!(detected.branch, None);
        assert_eq!(detected.checkout, repo);
    }

    #[test]
    fn outside_a_repo_nothing_is_detected() {
        let fixture = Fixture::new();
        let plain = fixture.path("plain");
        fs::create_dir_all(&plain).unwrap();

        assert_eq!(detect(&plain).unwrap(), None);
    }

    #[test]
    fn inside_the_git_dir_nothing_is_detected() {
        let fixture = Fixture::new();
        let repo = fixture.repo("repo");

        assert_eq!(detect(&repo.join(".git")).unwrap(), None);
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let fixture = Fixture::new();
        assert!(detect(&fixture.path("missing")).is_err());
    }

    #[test]
    fn without_a_remote_the_root_commit_is_the_id() {
        let fixture = Fixture::new();
        let repo = fixture.repo("notes");
        let root = head(&repo);
        commit(&repo, "second");

        let detected = detect(&repo).unwrap().unwrap();

        assert_eq!(detected.id.as_str(), root);
        assert_eq!(detected.url, None);
        assert_eq!(detected.name, "notes");
    }

    #[test]
    fn with_several_roots_the_smallest_is_the_id() {
        let fixture = Fixture::new();
        let repo = fixture.repo("notes");
        let first_root = head(&repo);
        git(&repo, &["switch", "--quiet", "--orphan", "pages"]);
        let second_root = commit(&repo, "unrelated history");

        let id = ProjectId::from_root_commit(&repo).unwrap().unwrap();

        assert_eq!(id.as_str(), first_root.min(second_root));
    }

    #[test]
    fn a_repo_without_commits_or_remote_is_unbound() {
        let fixture = Fixture::new();
        let empty = fixture.path("empty");
        fs::create_dir_all(&empty).unwrap();
        git(&empty, &["init", "--quiet"]);

        assert_eq!(ProjectId::from_root_commit(&empty).unwrap(), None);
        assert_eq!(detect(&empty).unwrap(), None);
    }

    #[test]
    fn a_relative_origin_resolves_against_the_checkout() {
        let fixture = Fixture::new();
        let origin = fixture.origin();
        let repo = fixture.repo("repo");
        git(&repo, &["remote", "add", "origin", "../origin.git"]);

        let detected = detect(&repo).unwrap().unwrap();

        assert_eq!(detected.id.as_str(), path_str(&origin));
        assert_eq!(detected.url.as_deref(), Some(path_str(&origin)));
    }
}
