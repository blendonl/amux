use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::info;

use super::detect::current_branch;
use super::{canonical, git};

const ORIGIN: &str = "origin";
const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";
const WORKTREES_DIR_SUFFIX: &str = "-worktrees";

pub(super) struct Worktree {
    pub(super) path: PathBuf,
    pub(super) branch_ref: Option<String>,
}

pub(super) fn list(dir: &Path) -> Result<Vec<Worktree>> {
    let output = git::run(dir, ["worktree", "list", "--porcelain"])?;
    let mut worktrees: Vec<Worktree> = Vec::new();
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            worktrees.push(Worktree {
                path: PathBuf::from(path),
                branch_ref: None,
            });
        } else if let (Some(reference), Some(worktree)) =
            (line.strip_prefix("branch "), worktrees.last_mut())
        {
            worktree.branch_ref = Some(reference.to_owned());
        }
    }
    Ok(worktrees)
}

pub fn default_branch(checkout: &Path) -> Result<String> {
    let remote_head = git::run(
        checkout,
        ["for-each-ref", "--format=%(symref:lstrip=3)", ORIGIN_HEAD],
    )?;
    if let Some(branch) = remote_head.lines().next().filter(|line| !line.is_empty()) {
        return Ok(branch.to_owned());
    }
    current_branch(checkout)?.with_context(|| {
        format!(
            "cannot tell the default branch of {}: origin/HEAD is not set and its HEAD is detached",
            checkout.display()
        )
    })
}

pub fn default_worktrees_dir(checkout: &Path, project_name: &str) -> PathBuf {
    checkout
        .parent()
        .unwrap_or(checkout)
        .join(format!("{project_name}{WORKTREES_DIR_SUFFIX}"))
}

pub fn ensure_worktree(checkout: &Path, branch: &str, worktrees_dir: &Path) -> Result<PathBuf> {
    validate_branch_name(checkout, branch)?;
    let local_ref = format!("refs/heads/{branch}");

    let existing = list(checkout)?
        .into_iter()
        .find(|worktree| worktree.branch_ref.as_deref() == Some(local_ref.as_str()));
    if let Some(existing) = existing {
        if existing.path.exists() {
            return Ok(existing.path);
        }
        info!(path = %existing.path.display(), "pruning a worktree whose directory is gone");
        git::run(checkout, ["worktree", "prune"])?;
    }

    let default = default_branch(checkout);
    if default.as_deref().is_ok_and(|default| default == branch) {
        return Ok(checkout.to_owned());
    }

    let dir = std::path::absolute(worktrees_dir.join(branch))
        .with_context(|| format!("resolving {}", worktrees_dir.display()))?;
    let remote_ref = format!("refs/remotes/origin/{branch}");
    if ref_exists(checkout, &local_ref)? {
        add_worktree(checkout, &[], &dir, branch)?;
    } else if ref_exists(checkout, &remote_ref)? {
        add_worktree(checkout, &["--track", "-b", branch], &dir, &remote_ref)?;
    } else {
        let start = start_point(checkout, &default?)?;
        add_worktree(checkout, &["--no-track", "-b", branch], &dir, &start)?;
    }
    canonical(&dir)
}

pub fn fetch_branch(checkout: &Path, branch: &str, timeout: Duration) -> Result<bool> {
    validate_branch_name(checkout, branch)?;
    if ref_exists(checkout, &format!("refs/heads/{branch}"))? {
        return Ok(false);
    }
    let remotes = git::run(checkout, ["remote"])?;
    if !remotes.lines().any(|remote| remote == ORIGIN) {
        return Ok(false);
    }
    let refspec = format!("+refs/heads/{branch}:refs/remotes/{ORIGIN}/{branch}");
    git::run_with_timeout(
        checkout,
        ["fetch", "--quiet", "--no-tags", ORIGIN, refspec.as_str()],
        timeout,
    )?;
    info!(checkout = %checkout.display(), branch, "fetched branch from origin");
    Ok(true)
}

pub fn check_removable(checkout: &Path, path: &Path) -> Result<PathBuf> {
    let target = canonical(path)?;
    if target == canonical(checkout)? {
        bail!(
            "refusing to remove {}: it is the project's main checkout",
            target.display()
        );
    }
    let changes = git::run(&target, ["status", "--porcelain"])?;
    if !changes.is_empty() {
        bail!(
            "refusing to remove {}: it has uncommitted changes",
            target.display()
        );
    }
    Ok(target)
}

pub fn remove_worktree(checkout: &Path, path: &Path) -> Result<()> {
    let target = check_removable(checkout, path)?;
    git::run(
        checkout,
        [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--"),
            target.as_os_str(),
        ],
    )?;
    info!(path = %target.display(), "removed worktree");
    Ok(())
}

pub fn clone(url: &str, dest: &Path) -> Result<PathBuf> {
    let dest =
        std::path::absolute(dest).with_context(|| format!("resolving {}", dest.display()))?;
    let (Some(parent), Some(name)) = (dest.parent(), dest.file_name()) else {
        bail!("cannot clone into {}", dest.display());
    };
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    git::run(
        parent,
        [
            OsStr::new("clone"),
            OsStr::new("--quiet"),
            OsStr::new("--"),
            OsStr::new(url),
            name,
        ],
    )?;
    info!(url, path = %dest.display(), "cloned project");
    canonical(&dest)
}

fn validate_branch_name(checkout: &Path, branch: &str) -> Result<()> {
    let checked = git::run(checkout, ["check-ref-format", "--branch", branch])
        .with_context(|| format!("{branch:?} is not a valid branch name"))?;
    if checked != branch {
        bail!("{branch:?} is not a valid branch name");
    }
    Ok(())
}

fn ref_exists(checkout: &Path, reference: &str) -> Result<bool> {
    let refs = git::run(checkout, ["for-each-ref", "--format=%(refname)", reference])?;
    Ok(refs.lines().any(|line| line == reference))
}

fn start_point(checkout: &Path, default: &str) -> Result<String> {
    let local = format!("refs/heads/{default}");
    if ref_exists(checkout, &local)? {
        return Ok(local);
    }
    let remote = format!("refs/remotes/origin/{default}");
    if ref_exists(checkout, &remote)? {
        return Ok(remote);
    }
    bail!(
        "the default branch {default:?} of {} has no commits to start from",
        checkout.display()
    )
}

fn add_worktree(checkout: &Path, options: &[&str], dir: &Path, commit: &str) -> Result<()> {
    let mut args: Vec<&OsStr> = vec![
        OsStr::new("worktree"),
        OsStr::new("add"),
        OsStr::new("--quiet"),
    ];
    args.extend(options.iter().map(OsStr::new));
    args.extend([OsStr::new("--"), dir.as_os_str(), OsStr::new(commit)]);
    git::run(checkout, args)?;
    info!(path = %dir.display(), commit, "added worktree");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::detect::detect;
    use super::super::testing::{commit, git, head, path_str, Fixture, LOCAL_ONLY, REMOTE_ONLY};
    use super::*;

    struct Setup {
        fixture: Fixture,
        origin: PathBuf,
        checkout: PathBuf,
        worktrees: PathBuf,
    }

    fn setup() -> Setup {
        let fixture = Fixture::new();
        let origin = fixture.origin();
        let checkout = fixture.clone_of(&origin, "amux");
        let worktrees = default_worktrees_dir(&checkout, "amux");
        Setup {
            fixture,
            origin,
            checkout,
            worktrees,
        }
    }

    fn upstream(dir: &Path, branch: &str) -> Option<String> {
        git::run(
            dir,
            [
                "rev-parse",
                "--abbrev-ref",
                &format!("{branch}@{{upstream}}"),
            ],
        )
        .ok()
    }

    #[test]
    fn worktrees_default_to_a_sibling_directory() {
        assert_eq!(
            default_worktrees_dir(Path::new("/home/tester/projects/amux"), "amux"),
            PathBuf::from("/home/tester/projects/amux-worktrees")
        );
    }

    #[test]
    fn the_default_branch_comes_from_origin_head() {
        let project = setup();
        git(&project.checkout, &["switch", "--quiet", REMOTE_ONLY]);

        assert_eq!(default_branch(&project.checkout).unwrap(), "main");
    }

    #[test]
    fn without_origin_head_the_default_branch_is_the_current_branch() {
        let fixture = Fixture::new();
        let repo = fixture.repo("notes");
        git(&repo, &["switch", "--quiet", "-c", "trunk"]);

        assert_eq!(default_branch(&repo).unwrap(), "trunk");
    }

    #[test]
    fn the_default_branch_maps_to_the_main_checkout() {
        let project = setup();

        let path = ensure_worktree(&project.checkout, "main", &project.worktrees).unwrap();

        assert_eq!(path, project.checkout);
        assert!(!project.worktrees.exists());
    }

    #[test]
    fn an_existing_local_branch_gets_a_worktree() {
        let project = setup();
        git(&project.checkout, &["branch", LOCAL_ONLY]);

        let path = ensure_worktree(&project.checkout, LOCAL_ONLY, &project.worktrees).unwrap();

        assert_eq!(path, project.worktrees.join(LOCAL_ONLY));
        let detected = detect(&path).unwrap().unwrap();
        assert_eq!(detected.branch.as_deref(), Some(LOCAL_ONLY));
        assert_eq!(detected.checkout, project.checkout);
        assert_eq!(upstream(&path, LOCAL_ONLY), None);
    }

    #[test]
    fn a_branch_only_on_origin_gets_a_tracking_worktree() {
        let project = setup();

        let path = ensure_worktree(&project.checkout, REMOTE_ONLY, &project.worktrees).unwrap();

        assert_eq!(path, project.worktrees.join(REMOTE_ONLY));
        assert_eq!(git(&path, &["branch", "--show-current"]), REMOTE_ONLY);
        assert_eq!(
            upstream(&path, REMOTE_ONLY).as_deref(),
            Some(format!("origin/{REMOTE_ONLY}").as_str())
        );
        assert_eq!(
            head(&path),
            git(&project.origin, &["rev-parse", REMOTE_ONLY])
        );
    }

    #[test]
    fn a_new_branch_starts_from_the_default_branch_in_nested_directories() {
        let project = setup();
        git(&project.checkout, &["switch", "--quiet", REMOTE_ONLY]);
        git(&project.checkout, &["switch", "--quiet", "main"]);
        let main = commit(&project.checkout, "local work on main");
        git(&project.checkout, &["switch", "--quiet", REMOTE_ONLY]);

        let path =
            ensure_worktree(&project.checkout, "feature/new-thing", &project.worktrees).unwrap();

        assert_eq!(path, project.worktrees.join("feature").join("new-thing"));
        assert_eq!(
            git(&path, &["branch", "--show-current"]),
            "feature/new-thing"
        );
        assert_eq!(head(&path), main);
        assert_eq!(upstream(&path, "feature/new-thing"), None);
    }

    #[test]
    fn an_existing_worktree_is_reused() {
        let project = setup();
        let created = ensure_worktree(&project.checkout, "feature-x", &project.worktrees).unwrap();
        let elsewhere = project.fixture.path("elsewhere/local");
        git(
            &project.checkout,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                LOCAL_ONLY,
                path_str(&elsewhere),
            ],
        );

        assert_eq!(
            ensure_worktree(&project.checkout, "feature-x", &project.worktrees).unwrap(),
            created
        );
        assert_eq!(
            ensure_worktree(&project.checkout, LOCAL_ONLY, &project.worktrees).unwrap(),
            elsewhere
        );
        assert_eq!(list(&project.checkout).unwrap().len(), 3);
    }

    #[test]
    fn a_worktree_whose_directory_vanished_is_recreated() {
        let project = setup();
        let created = ensure_worktree(&project.checkout, "feature-x", &project.worktrees).unwrap();
        fs::remove_dir_all(&created).unwrap();

        let recreated =
            ensure_worktree(&project.checkout, "feature-x", &project.worktrees).unwrap();

        assert_eq!(recreated, created);
        assert_eq!(git(&recreated, &["branch", "--show-current"]), "feature-x");
    }

    #[test]
    fn invalid_branch_names_are_rejected() {
        let project = setup();
        for branch in ["bad..name", "-option", "../escape", "@{-1}", ""] {
            assert!(
                ensure_worktree(&project.checkout, branch, &project.worktrees).is_err(),
                "accepted {branch:?}"
            );
        }
        assert!(!project.worktrees.exists());
    }

    #[test]
    fn fetching_finds_a_branch_pushed_after_the_clone() {
        let project = setup();
        let pusher = project.fixture.clone_of(&project.origin, "pusher");
        git(&pusher, &["switch", "--quiet", "-c", "pushed-later"]);
        let pushed = commit(&pusher, "pushed after the clone");
        git(&pusher, &["push", "--quiet", "origin", "pushed-later"]);
        let timeout = Duration::from_secs(30);

        assert!(fetch_branch(&project.checkout, "pushed-later", timeout).unwrap());
        let path = ensure_worktree(&project.checkout, "pushed-later", &project.worktrees).unwrap();

        assert_eq!(head(&path), pushed);
        assert_eq!(
            upstream(&path, "pushed-later").as_deref(),
            Some("origin/pushed-later")
        );
        assert!(!fetch_branch(&project.checkout, "pushed-later", timeout).unwrap());
        assert!(fetch_branch(&project.checkout, "nowhere-yet", timeout).is_err());
        assert!(fetch_branch(&project.checkout, "bad..name", timeout).is_err());
    }

    #[test]
    fn fetching_without_an_origin_does_nothing() {
        let fixture = Fixture::new();
        let repo = fixture.repo("notes");

        assert!(!fetch_branch(&repo, "feature", Duration::from_secs(30)).unwrap());
    }

    #[test]
    fn a_clean_worktree_is_removable_and_the_check_keeps_it() {
        let project = setup();
        let path = ensure_worktree(&project.checkout, "feature-x", &project.worktrees).unwrap();

        assert_eq!(check_removable(&project.checkout, &path).unwrap(), path);
        assert!(path.exists());
        assert!(check_removable(&project.checkout, &project.checkout).is_err());
    }

    #[test]
    fn removing_refuses_the_main_checkout() {
        let project = setup();

        let err = remove_worktree(&project.checkout, &project.checkout).unwrap_err();

        assert!(err.to_string().contains("main checkout"), "{err:#}");
        assert!(project.checkout.join(".git").exists());
    }

    #[test]
    fn removing_refuses_a_dirty_worktree() {
        let project = setup();
        let path = ensure_worktree(&project.checkout, "feature-x", &project.worktrees).unwrap();
        fs::write(path.join("scratch.txt"), "unsaved work").unwrap();

        let err = remove_worktree(&project.checkout, &path).unwrap_err();

        assert!(err.to_string().contains("uncommitted changes"), "{err:#}");
        assert!(path.join("scratch.txt").exists());
    }

    #[test]
    fn removing_a_clean_worktree_deletes_it() {
        let project = setup();
        let path = ensure_worktree(&project.checkout, "feature-x", &project.worktrees).unwrap();

        remove_worktree(&project.checkout, &path).unwrap();

        assert!(!path.exists());
        assert_eq!(list(&project.checkout).unwrap().len(), 1);
        assert!(git(&project.checkout, &["branch", "--list", "feature-x"]).contains("feature-x"));
    }

    #[test]
    fn cloning_from_origin_returns_a_detectable_checkout() {
        let project = setup();
        let dest = project.fixture.path("projects/nested/amux");

        let path = clone(path_str(&project.origin), &dest).unwrap();

        assert_eq!(path, dest);
        let detected = detect(&path).unwrap().unwrap();
        assert_eq!(detected.id.as_str(), path_str(&project.origin));
        assert_eq!(detected.checkout, dest);
        assert_eq!(detected.branch.as_deref(), Some("main"));
        assert_eq!(default_branch(&path).unwrap(), "main");
    }

    #[test]
    fn cloning_into_a_non_empty_directory_fails() {
        let project = setup();

        assert!(clone(path_str(&project.origin), &project.checkout).is_err());
    }
}
