//! Unified external-command execution for every CLI Orca shells out to
//! (`docker compose`, `limactl`, `kubectl`, `wsl`, `k3s`, `git`, ...).
//!
//! ## Why this module exists
//!
//! Calling `std::process::Command` / `tokio::process::Command` directly at each
//! call site produced three recurring classes of bug:
//!
//! 1. **Orphaned grandchildren.** `docker compose up -d` and `limactl start`
//!    spawn their own children. Killing only the direct child leaves those
//!    running, which surfaces much later as "the VM cannot be deleted" or "the
//!    port is still held". On Unix the fix is to put the child in its own
//!    process group and signal the group, not the process.
//! 2. **No timeout, no cancellation.** A hung CLI blocked a request handler
//!    forever with no way to stop it.
//! 3. **Secrets in argv.** `ps` exposes another process's argv to every user on
//!    the machine, so a value that only needs to reach the child should be
//!    written to stdin instead of passed as an argument.
//!
//! ## Contract
//!
//! * [`run`] executes an **argv vector without a shell**. `&&`, `|`, `$VAR` and
//!   globs are ordinary characters with no special meaning, which removes the
//!   entire "did everyone remember to escape this?" class of bug.
//! * [`run_shell`] is the only way to get shell interpretation, and it requires
//!   a `reason` argument so each use is justified at its call site.
//! * On timeout, on cancellation, or if the caller's future is dropped, the
//!   **whole process group** is SIGKILLed (see [`kill_process_group`]).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// How a command should be run. Build it with [`CommandOptions::default`] plus
/// the builder methods.
#[derive(Debug, Clone, Default)]
pub struct CommandOptions {
    /// Working directory for the child.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables (added on top of the inherited environment).
    pub env: Vec<(String, String)>,
    /// Kill the command (and its process group) after this long.
    pub timeout: Option<Duration>,
    /// Kill the command (and its process group) when this token is cancelled.
    pub cancel: Option<CancellationToken>,
    /// Bytes to write to the child's stdin, which is then closed.
    ///
    /// Use this for secrets: argv is world-readable via `ps`, stdin is not.
    pub stdin: Option<Vec<u8>>,
}

impl CommandOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(data.into());
        self
    }
}

/// Result of a finished command.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub success: bool,
    /// Exit code, or `-1` when the process was signalled (timeout, cancel,
    /// external kill) and therefore never produced one.
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// True when the command was stopped by [`CommandOptions::timeout`].
    pub timed_out: bool,
}

impl CommandOutput {
    /// The *last* non-empty stderr line, which is normally the actionable part
    /// of a CLI failure (line 1 is often just a deprecation warning).
    pub fn last_stderr_line(&self) -> &str {
        self.stderr
            .lines()
            .rfind(|line| !line.trim().is_empty())
            .unwrap_or("(no output)")
    }

    /// Turn a failed command into an error carrying its last stderr line.
    pub fn into_result(self, what: &str) -> anyhow::Result<Self> {
        if self.success {
            Ok(self)
        } else if self.timed_out {
            anyhow::bail!("{what} timed out")
        } else {
            anyhow::bail!("{what} failed (exit {}): {}", self.exit_code, self.last_stderr_line())
        }
    }
}

/// Run a program with an explicit argv vector. **No shell is involved**, so
/// shell metacharacters in `args` are passed through literally.
pub async fn run<S: AsRef<str>>(
    program: &str,
    args: &[S],
    opts: CommandOptions,
) -> anyhow::Result<CommandOutput> {
    let mut cmd = Command::new(program);
    for arg in args {
        cmd.arg(arg.as_ref());
    }
    collect(cmd, opts, program).await
}

/// Run a script through the platform shell.
///
/// `reason` is required on purpose: reaching for a shell should be a
/// deliberate, reviewable decision, because shell interpolation is how
/// user-controlled data turns into command injection. If you can express the
/// work as an argv vector, use [`run`] instead.
pub async fn run_shell(script: &str, reason: &str, opts: CommandOptions) -> anyhow::Result<CommandOutput> {
    #[cfg(unix)]
    let (cmd, label) = {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        (cmd, "sh -c")
    };
    #[cfg(windows)]
    let (cmd, label) = {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(script);
        (cmd, "cmd /C")
    };

    tracing::debug!("running shell command ({reason}): {script}");
    collect(cmd, opts, label).await
}

/// Signal the entire process group led by `pid`.
///
/// The child is spawned with `process_group(0)`, so its PGID equals its PID and
/// a negative pid reaches every descendant that has not changed its own group.
pub fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: `kill` with a negative pid only delivers a signal; there is no
        // memory to invalidate. A dead or already-reaped group yields ESRCH,
        // which we deliberately ignore.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        // `taskkill /T` walks the parent/child tree. Unlike a Job Object it
        // cannot see grandchildren that were already re-parented, so a Windows
        // Job Object remains a follow-up; this still covers the common case.
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

/// Kills the process group if the owning future is dropped before the command
/// finished — e.g. because a `select!` arm lost, or the request was aborted.
struct GroupGuard {
    pgid: Option<u32>,
}

impl GroupGuard {
    fn disarm(&mut self) {
        self.pgid = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pgid {
            kill_process_group(pid);
        }
    }
}

/// Sleep forever when no timeout was configured. Used as a `select!` arm so the
/// happy path keeps a single code shape.
async fn deadline(timeout: Option<Duration>) {
    match timeout {
        Some(duration) => tokio::time::sleep(duration).await,
        None => std::future::pending::<()>().await,
    }
}

/// Await cancellation forever when no token was supplied.
async fn cancellation(token: &Option<CancellationToken>) {
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

async fn collect(mut cmd: Command, mut opts: CommandOptions, label: &str) -> anyhow::Result<CommandOutput> {
    if let Some(dir) = &opts.cwd {
        cmd.current_dir(dir);
    }
    for (key, value) in &opts.env {
        cmd.env(key, value);
    }

    cmd.stdin(if opts.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group: the precondition for signalling the whole tree.
    #[cfg(unix)]
    cmd.process_group(0);
    // Belt and braces: if the Child handle itself is dropped, tokio kills the
    // direct child even if the GroupGuard has already been disarmed.
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn `{label}`"))?;

    let pgid = child.id();
    let mut guard = GroupGuard { pgid };

    // Feed stdin from its own task: a child that fills the stdout pipe before
    // reading stdin would otherwise deadlock against our own reader.
    if let Some(mut handle) = child.stdin.take() {
        let data = opts.stdin.take().unwrap_or_default();
        tokio::spawn(async move {
            let _ = handle.write_all(&data).await;
            let _ = handle.shutdown().await;
        });
    }

    let mut timed_out = false;
    let wait = child.wait_with_output();
    tokio::pin!(wait);

    let result = tokio::select! {
        result = &mut wait => result,
        _ = deadline(opts.timeout) => {
            timed_out = true;
            if let Some(pid) = pgid {
                kill_process_group(pid);
            }
            wait.await
        }
        _ = cancellation(&opts.cancel) => {
            if let Some(pid) = pgid {
                kill_process_group(pid);
            }
            wait.await
        }
    };

    guard.disarm();
    let output = result.with_context(|| format!("failed to wait for `{label}`"))?;

    Ok(CommandOutput {
        success: output.status.success(),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn argv_is_not_shell_interpreted() {
        // A shell would treat `&&` as a command separator and print only "a".
        let out = run("echo", &["a", "&&", "b"], CommandOptions::default())
            .await
            .unwrap();
        assert!(out.success);
        assert_eq!(out.stdout.trim(), "a && b");
    }

    #[tokio::test]
    async fn timeout_is_reported_and_exit_code_is_unavailable() {
        let out = run(
            "sleep",
            &["30"],
            CommandOptions::default().timeout(Duration::from_millis(200)),
        )
        .await
        .unwrap();
        assert!(out.timed_out);
        assert!(!out.success);
        assert_eq!(out.exit_code, -1);
    }

    #[tokio::test]
    async fn cancellation_stops_the_command_quickly() {
        let token = CancellationToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            canceller.cancel();
        });

        let out = tokio::time::timeout(
            Duration::from_secs(10),
            run("sleep", &["30"], CommandOptions::default().cancel(token)),
        )
        .await
        .expect("cancellation did not stop the command")
        .unwrap();

        assert!(!out.success);
        assert!(!out.timed_out, "cancellation must not be reported as a timeout");
    }

    #[tokio::test]
    async fn stdin_is_delivered_without_touching_argv() {
        // The child prints its own argv (no secret), then reports the length of
        // what it read from stdin. The secret reaches it over stdin only.
        let out = run(
            "sh",
            &[
                "-c",
                "ps -o command= -p $$; read -r line; printf 'len:%s' \"${#line}\"",
            ],
            CommandOptions::default().stdin("super-secret".to_string()),
        )
        .await
        .unwrap();

        assert!(out.stdout.contains("len:12"), "stdin not delivered: {:?}", out.stdout);
        assert!(
            !out.stdout.contains("super-secret"),
            "secret leaked into argv: {:?}",
            out.stdout
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_grandchildren_too() {
        // `sh` starts a background `sleep` (a grandchild that holds the stdout
        // pipe open) and waits. Killing only the direct child would leave the
        // grandchild alive and the pipe open forever.
        let out = run(
            "sh",
            &["-c", "sleep 31337 & echo $!; wait"],
            CommandOptions::default().timeout(Duration::from_millis(300)),
        )
        .await
        .unwrap();

        assert!(out.timed_out);
        let pid: i32 = out
            .stdout
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("expected a grandchild pid, got {:?}", out.stdout));

        let mut alive = true;
        for _ in 0..50 {
            // SAFETY: signal 0 only probes whether the pid exists.
            if unsafe { libc::kill(pid, 0) } != 0 {
                alive = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive, "grandchild {pid} survived the process-group kill");
    }

    #[tokio::test]
    async fn last_stderr_line_picks_the_actionable_line() {
        let out = run(
            "sh",
            &["-c", "echo 'warning: deprecation' >&2; echo 'real failure' >&2; exit 3"],
            CommandOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.exit_code, 3);
        assert_eq!(out.last_stderr_line(), "real failure");
        assert!(out.into_result("demo").is_err());
    }

    #[tokio::test]
    async fn missing_program_is_a_contextual_error() {
        let err = run("orca-definitely-not-a-real-binary", &[] as &[&str], CommandOptions::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("failed to spawn"), "{err}");
    }
}
