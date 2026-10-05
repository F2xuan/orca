use std::process::Stdio;

use orca_core::compose::{ComposeOutput, ComposeRunner};
use orca_core::runtime::RuntimeKind;
use tokio::process::Command;

use crate::BollardRuntime;

impl ComposeRunner for BollardRuntime {
    async fn compose_up(&self, working_dir: &str, config_file: Option<&str>) -> anyhow::Result<ComposeOutput> {
        run_compose(self, working_dir, config_file, &["up", "-d"]).await
    }

    async fn compose_down(&self, working_dir: &str, config_file: Option<&str>) -> anyhow::Result<ComposeOutput> {
        run_compose(self, working_dir, config_file, &["down"]).await
    }

    async fn compose_restart(&self, working_dir: &str, config_file: Option<&str>) -> anyhow::Result<ComposeOutput> {
        run_compose(self, working_dir, config_file, &["restart"]).await
    }

    async fn compose_pull(&self, working_dir: &str, config_file: Option<&str>) -> anyhow::Result<ComposeOutput> {
        run_compose(self, working_dir, config_file, &["pull"]).await
    }
}

async fn run_compose(
    backend: &BollardRuntime,
    working_dir: &str,
    config_file: Option<&str>,
    args: &[&str],
) -> anyhow::Result<ComposeOutput> {
    let runtime = backend.detect_runtime().await;

    let (program, base_args): (&str, Vec<&str>) = match runtime {
        RuntimeKind::Podman => ("podman", vec!["compose"]),
        RuntimeKind::Docker => ("docker", vec!["compose"]),
    };

    let mut cmd = Command::new(program);
    // macOS app bundles launched from Finder/Dock inherit launchd's minimal
    // PATH, which misses /usr/local/bin (Docker Desktop) and /opt/homebrew/bin.
    // Without this, spawning the compose CLI fails with
    // "No such file or directory (os error 2)" while the Docker API itself
    // still works (the daemon connects over the socket, not the CLI).
    cmd.env("PATH", crate::environment::extended_path());
    cmd.current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for arg in &base_args {
        cmd.arg(arg);
    }

    if let Some(file) = config_file {
        cmd.arg("-f").arg(file);
    }

    for arg in args {
        cmd.arg(arg);
    }

    tracing::info!("Running: {program} {} {}", base_args.join(" "), args.join(" "));

    let output = cmd.output().await?;

    let stdout = rewrite_docker_desktop_urls(&String::from_utf8_lossy(&output.stdout));
    let stderr = rewrite_docker_desktop_urls(&String::from_utf8_lossy(&output.stderr));
    let exit_code = output.status.code().unwrap_or(-1);

    if !output.status.success() {
        // The *last* stderr line is the real failure; line 1 is often just the
        // `version` deprecation warning, which made failures look misleading.
        let detail = stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .next_back()
            .unwrap_or("(no output)");
        tracing::warn!("Compose command exited with {exit_code}: {detail}");
    }

    Ok(ComposeOutput {
        success: output.status.success(),
        stdout,
        stderr,
        exit_code,
    })
}

/// Rewrite `docker-desktop://` URLs to `orca://` URLs in output text.
fn rewrite_docker_desktop_urls(text: &str) -> String {
    text.replace("docker-desktop://dashboard/build/", "orca://build/")
        .replace("docker-desktop://", "orca://")
}
