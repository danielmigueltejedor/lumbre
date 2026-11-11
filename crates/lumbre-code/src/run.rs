use std::process::{Command, Stdio};

use crate::{Grant, Workspace};

/// `git` subcommands that only read; they need no grant.
pub const READ_ONLY_GIT: &[&str] =
    &["status", "diff", "log", "show", "branch", "blame", "ls-files", "rev-parse"];

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("no command given")]
    Empty,
    #[error("`{0}` is not allowed in this repository; allow it with `lumbre-code allow exec \"<prefix>\"`")]
    Denied(String),
    #[error("could not run `{command}`: {error}")]
    Spawn { command: String, error: std::io::Error },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutput {
    /// Exit code; `None` when the process was killed by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

fn is_read_only_git(argv: &[String]) -> bool {
    // Only the plain `git <subcommand> ...` form: options before the subcommand (`-c`, `-C`,
    // `--exec-path`) can change what git runs, and `branch` with arguments can modify.
    match argv {
        [git, sub, rest @ ..] if git == "git" => {
            READ_ONLY_GIT.contains(&sub.as_str())
                && (sub != "branch" || rest.is_empty())
                && !rest
                    .iter()
                    .any(|arg| arg.starts_with("--output") || arg == "-o" || arg.starts_with("--ext-diff"))
        }
        _ => false,
    }
}

/// Runs a command in the workspace root, without a shell, if the grant allows it.
///
/// There is no shell on purpose: the allowed prefix is compared with the real argument vector,
/// so `cargo test; rm -rf ~` cannot ride along on a grant for `cargo test`.
pub fn run(workspace: &Workspace, argv: &[String], grant: &Grant) -> Result<RunOutput, RunError> {
    let (program, args) = argv.split_first().ok_or(RunError::Empty)?;
    let command = argv.join(" ");
    if !is_read_only_git(argv) && !grant.allows_exec(argv) {
        return Err(RunError::Denied(command));
    }
    let output = Command::new(program)
        .args(args)
        .current_dir(workspace.root())
        .stdin(Stdio::null())
        .output()
        .map_err(|error| RunError::Spawn { command, error })?;
    Ok(RunOutput {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::tests::{cleanup, sandbox};

    fn argv(command: &str) -> Vec<String> {
        command.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn read_only_git_is_recognised_narrowly() {
        for allowed in ["git status", "git diff --stat", "git log -n 3", "git branch"] {
            assert!(is_read_only_git(&argv(allowed)), "{allowed}");
        }
        for denied in [
            "git push",
            "git branch -D main",
            "git -c core.pager=sh log",
            "git diff --output=/tmp/x",
            "gitx status",
            "git",
        ] {
            assert!(!is_read_only_git(&argv(denied)), "{denied}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn commands_run_in_the_root_only_when_granted_and_without_a_shell() {
        let workspace = sandbox("run");
        let nothing = Grant::default();
        assert!(matches!(run(&workspace, &argv("ls src"), &nothing), Err(RunError::Denied(_))));
        assert!(matches!(run(&workspace, &[], &nothing), Err(RunError::Empty)));

        let grant = Grant { write: false, exec: vec!["ls".into(), "echo".into()] };
        let output = run(&workspace, &argv("ls src"), &grant).unwrap();
        assert_eq!((output.code, output.stdout.as_str()), (Some(0), "main.rs\nutil.py\n"));

        // Shell syntax is just text: nothing is chained, nothing is created.
        let output = run(&workspace, &argv("echo hi ; touch pwned"), &grant).unwrap();
        assert_eq!(output.stdout, "hi ; touch pwned\n");
        assert!(!workspace.root().join("pwned").exists());

        let failing = run(&workspace, &argv("ls does-not-exist"), &grant).unwrap();
        assert_ne!(failing.code, Some(0));
        assert!(!failing.stderr.is_empty());
        cleanup(workspace);
    }
}
