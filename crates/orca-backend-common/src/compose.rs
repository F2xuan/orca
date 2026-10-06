use orca_core::compose::{ComposeOutput, ComposeRunner};
use orca_core::proc::{CommandOptions, run};
use orca_core::runtime::RuntimeKind;

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

    let mut argv: Vec<String> = base_args.iter().map(|arg| (*arg).to_string()).collect();
    if let Some(file) = config_file {
        argv.push("-f".to_string());
        argv.push(file.to_string());
    }
    argv.extend(args.iter().map(|arg| (*arg).to_string()));

    tracing::info!("Running: {program} {}", argv.join(" "));

    // Deliberately no timeout: a first-run `compose up` may legitimately pull
    // several gigabytes. The process-group guard still reaps Compose's whole
    // child tree if this future is dropped (client disconnect, daemon
    // shutdown) instead of orphaning it — which is what used to leave
    // containers half-created and ports held with no parent left to reap them.
    let options = CommandOptions::new()
        .cwd(working_dir)
        // macOS app bundles launched from Finder/Dock inherit launchd's minimal
        // PATH, which misses /usr/local/bin (Docker Desktop) and /opt/homebrew/bin.
        // Without this, spawning the compose CLI fails with
        // "No such file or directory (os error 2)" while the Docker API itself
        // still works (the daemon connects over the socket, not the CLI).
        .env("PATH", crate::environment::extended_path());

    let output = run(program, &argv, options).await?;

    let stdout = rewrite_docker_desktop_urls(&output.stdout);
    let stderr = rewrite_docker_desktop_urls(&output.stderr);

    if !output.success {
        // The *last* stderr line is the real failure; line 1 is often just the
        // `version` deprecation warning, which made failures look misleading.
        tracing::warn!(
            "Compose command exited with {}: {}",
            output.exit_code,
            output.last_stderr_line()
        );
    }

    Ok(ComposeOutput {
        success: output.success,
        stdout,
        stderr,
        exit_code: output.exit_code,
    })
}

/// Rewrite `docker-desktop://` URLs to `orca://` URLs in output text.
fn rewrite_docker_desktop_urls(text: &str) -> String {
    text.replace("docker-desktop://dashboard/build/", "orca://build/")
        .replace("docker-desktop://", "orca://")
}
