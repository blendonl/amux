use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use tracing::debug;

const REPOSITORY_VARIABLES: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

#[derive(Debug)]
pub struct GitError {
    pub dir: PathBuf,
    pub args: Vec<OsString>,
    pub code: Option<i32>,
    pub stderr: String,
}

impl GitError {
    pub fn is_not_a_repository(&self) -> bool {
        self.stderr.contains("not a git repository")
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let args = self
            .args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        write!(f, "`git {args}` in {} failed: ", self.dir.display())?;
        match (self.stderr.is_empty(), self.code) {
            (false, _) => f.write_str(&self.stderr),
            (true, Some(code)) => write!(f, "exit status {code}"),
            (true, None) => f.write_str("killed by a signal"),
        }
    }
}

impl std::error::Error for GitError {}

pub fn run<I, S>(dir: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect();
    debug!(dir = %dir.display(), ?args, "running git");

    let output = command(dir)
        .args(&args)
        .output()
        .context("running git; is it installed and on PATH?")?;
    if !output.status.success() {
        return Err(GitError {
            dir: dir.to_owned(),
            args,
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }
        .into());
    }
    let stdout = String::from_utf8(output.stdout)
        .with_context(|| format!("git printed non-UTF-8 output in {}", dir.display()))?;
    Ok(stdout.trim().to_owned())
}

fn command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    for variable in REPOSITORY_VARIABLES {
        command.env_remove(variable);
    }
    isolate_from_user_config(&mut command);
    command.arg("-C").arg(dir);
    command
}

#[cfg(not(test))]
fn isolate_from_user_config(_command: &mut Command) {}

#[cfg(test)]
fn isolate_from_user_config(command: &mut Command) {
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .args([
            "-c",
            "user.name=amux tests",
            "-c",
            "user.email=amux-tests@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_never_see_the_users_git_config() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = run(dir.path(), ["config", "--list", "--show-scope"]).unwrap();

        for line in scopes.lines() {
            assert!(line.starts_with("command\t"), "unexpected config: {line}");
        }
    }

    #[test]
    fn failures_carry_git_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(dir.path(), ["rev-parse", "HEAD"]).unwrap_err();

        let git_error = err.downcast_ref::<GitError>().unwrap();
        assert!(git_error.is_not_a_repository());
        assert_eq!(git_error.code, Some(128));
        let message = err.to_string();
        assert!(message.contains("`git rev-parse HEAD`"), "{message}");
        assert!(message.contains("fatal: not a git repository"), "{message}");
    }

    #[test]
    fn stdout_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), ["init", "--quiet"]).unwrap();

        assert_eq!(
            run(dir.path(), ["rev-parse", "--is-inside-work-tree"]).unwrap(),
            "true"
        );
    }
}
