//! Daemon lifecycle management — start/stop the orca-daemon process.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;

use tokio::process::{Child, Command};

/// Manages the daemon process lifecycle.
pub struct DaemonManager {
    child: Mutex<Option<Child>>,
}

impl DaemonManager {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
        }
    }

    /// Start the daemon if not already running.
    /// If a daemon is running but is a different build, restart it.
    pub async fn start(&self) -> Result<(), String> {
        // Check if daemon is already responding
        if self.health_check().await {
            let expected_version = env!("CARGO_PKG_VERSION");
            let running_version = self.daemon_version().await;
            // The version string is not proof of the same build: a daemon
            // rebuilt from newer source still reports it. Comparing only
            // versions therefore keeps the *old* process alive and every backend
            // change silently fails to take effect — a rebuild looks like a
            // no-op. Compare the on-disk sidecar's fingerprint too.
            let running_build = self.daemon_build().await;
            let on_disk_build = sidecar_fingerprint();
            let same_build = builds_match(running_build.as_deref(), on_disk_build.as_deref());
            match running_version {
                Some(v) if v == expected_version && same_build => {
                    tracing::info!(
                        "Orca daemon already running (v{v}, build {})",
                        running_build.unwrap_or_default()
                    );
                    return Ok(());
                }
                Some(v) if v != expected_version => {
                    tracing::info!("Daemon version mismatch: running v{v}, app is v{expected_version} — restarting");
                }
                _ => {
                    tracing::info!(
                        "Daemon build mismatch (running {running_build:?} vs on-disk {on_disk_build:?}) — restarting to pick up the current sidecar"
                    );
                }
            }
            self.kill_existing_daemon().await;
        }

        let daemon_path = find_daemon_binary();
        tracing::info!("Starting orca-daemon from: {}", daemon_path);

        // Log daemon output to a file for debugging
        let log_dir = orca_core::config::OrcaConfig::config_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        let _ = std::fs::create_dir_all(&log_dir);
        let log_path = log_dir.join("daemon.log");
        let log_file = std::fs::File::create(&log_path)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let err_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        tracing::info!("Daemon log: {}", log_path.display());

        let mut cmd = Command::new(&daemon_path);
        cmd.stdout(log_file).stderr(err_file).kill_on_drop(true);

        // On Windows, prevent the daemon from opening a visible console window
        #[cfg(target_os = "windows")]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

        let child = cmd.spawn().map_err(|e| format!("Failed to start daemon: {e}"))?;

        *self.child.lock().map_err(|e| format!("Lock poisoned: {e}"))? = Some(child);

        // Wait for daemon to become ready
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if self.health_check().await {
                tracing::info!("Orca daemon is ready");
                return Ok(());
            }
        }

        Err("Daemon started but failed to respond within 7.5 seconds".into())
    }

    /// Stop the daemon process (fire-and-forget).
    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().ok().and_then(|mut guard| guard.take()) {
            tracing::info!("Stopping orca-daemon");
            let _ = child.start_kill();
            // Brief sleep to give the process time to exit
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    /// Stop the daemon and wait for the process to fully exit.
    /// Important on Windows where the binary file is locked while the process runs.
    pub async fn stop_and_wait(&self) {
        if let Some(mut child) = self.child.lock().ok().and_then(|mut guard| guard.take()) {
            tracing::info!("Stopping orca-daemon and waiting for exit");
            let _ = child.start_kill();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
            tracing::info!("orca-daemon stopped");
        }
    }

    /// Kill any existing daemon process — even one spawned by a previous app version.
    async fn kill_existing_daemon(&self) {
        // First try our tracked child
        self.stop();

        // Also kill by process name (catches daemons from previous app installs)
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/f", "/im", "orca-daemon.exe"])
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .output();
        }
        #[cfg(not(target_os = "windows"))]
        {
            // Match the process name exactly, NOT the command line. Using
            // `-f` would also kill `cargo run --bin orca-daemon`, editor
            // sessions viewing the code, log tails, etc.
            let _ = std::process::Command::new("pkill").args(["-x", "orca-daemon"]).output();
        }

        // Wait for the process to fully exit and release the port
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            if !self.health_check().await {
                tracing::info!("Old daemon has exited");
                return;
            }
        }
        tracing::warn!("Old daemon may still be running after kill attempts");
    }

    /// Get the version of the running daemon, if available.
    async fn daemon_version(&self) -> Option<String> {
        let resp = reqwest::Client::new()
            .get("http://127.0.0.1:9477/api/v1/health")
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .ok()?;
        let json: serde_json::Value = resp.json().await.ok()?;
        json["version"].as_str().map(|s| s.to_string())
    }

    /// The build fingerprint the running daemon reports, if any.
    ///
    /// `None` for a daemon predating the field — which is by definition an older
    /// build, so the caller treats it as a mismatch and restarts.
    async fn daemon_build(&self) -> Option<String> {
        let resp = reqwest::Client::new()
            .get("http://127.0.0.1:9477/api/v1/health")
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .ok()?;
        let json: serde_json::Value = resp.json().await.ok()?;
        json["build"].as_str().map(|s| s.to_string())
    }

    async fn health_check(&self) -> bool {
        // Be strict: verify the endpoint returned 2xx AND a valid JSON body
        // with a "status" field of "ok". This prevents false-positives when
        // an unrelated process binds port 9477.
        let resp = match reqwest::Client::new()
            .get("http://127.0.0.1:9477/api/v1/health")
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            _ => return false,
        };
        match resp.json::<serde_json::Value>().await {
            Ok(json) => json.get("status").and_then(|v| v.as_str()) == Some("ok"),
            Err(_) => false,
        }
    }
}

impl Drop for DaemonManager {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Find the daemon binary. Search order:
/// 1. Tauri sidecar location (bundled with the app)
/// 2. Next to the current executable
/// 3. Development build paths
/// 4. System PATH
pub fn find_daemon_binary() -> String {
    let bin_name = daemon_binary_name();

    // 1. Tauri sidecar: the binary is bundled alongside the app
    //    Tauri puts sidecars next to the exe with target triple suffix
    let sidecar_name = if cfg!(target_os = "windows") {
        format!(
            "orca-daemon-{}.exe",
            std::env::consts::ARCH.replace("x86_64", "x86_64-pc-windows-msvc")
        )
    } else if cfg!(target_os = "macos") {
        format!("orca-daemon-{}-apple-darwin", std::env::consts::ARCH)
    } else {
        format!("orca-daemon-{}-unknown-linux-gnu", std::env::consts::ARCH)
    };

    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        // Check with target triple suffix (Tauri sidecar convention)
        let sidecar_triple = parent.join(&sidecar_name);
        if sidecar_triple.exists() {
            return sidecar_triple.to_string_lossy().to_string();
        }

        // Check without suffix
        let sidecar = parent.join(bin_name);
        if sidecar.exists() {
            return sidecar.to_string_lossy().to_string();
        }

        // Check in a binaries/ subdirectory
        let sidecar_sub = parent.join("binaries").join(bin_name);
        if sidecar_sub.exists() {
            return sidecar_sub.to_string_lossy().to_string();
        }
        let sidecar_sub_triple = parent.join("binaries").join(&sidecar_name);
        if sidecar_sub_triple.exists() {
            return sidecar_sub_triple.to_string_lossy().to_string();
        }

        // Check Tauri resource directory patterns
        // On macOS: ../Resources/binaries/
        if let Some(grandparent) = parent.parent() {
            for name in &[bin_name, sidecar_name.as_str()] {
                let macos_resource = grandparent.join("Resources").join("binaries").join(name);
                if macos_resource.exists() {
                    return macos_resource.to_string_lossy().to_string();
                }
            }
        }
    }

    // 2. Development build paths
    let dev_paths = if cfg!(windows) {
        vec![
            PathBuf::from("./target/debug/orca-daemon.exe"),
            PathBuf::from("./target/release/orca-daemon.exe"),
        ]
    } else {
        vec![
            PathBuf::from("./target/debug/orca-daemon"),
            PathBuf::from("./target/release/orca-daemon"),
        ]
    };

    for path in &dev_paths {
        if path.exists() {
            return path
                .canonicalize()
                .map(|c| c.to_string_lossy().to_string())
                .unwrap_or_else(|_| path.to_string_lossy().to_string());
        }
    }

    // 3. Fall back to PATH
    bin_name.to_string()
}

fn daemon_binary_name() -> &'static str {
    if cfg!(windows) {
        "orca-daemon.exe"
    } else {
        "orca-daemon"
    }
}

/// Fingerprint of the sidecar currently sitting on disk, in the same form the
/// daemon reports from `/health` (see `build_fingerprint` in the daemon).
///
/// `None` when the binary can't be stat'd or has no mtime; the caller treats
/// that as a mismatch and restarts, because "unknown" must not be read as
/// "current".
fn sidecar_fingerprint() -> Option<String> {
    std::fs::canonicalize(find_daemon_binary())
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().to_string())
}

/// Whether a running daemon is the same build as the sidecar on disk.
///
/// Deliberately strict: an absent fingerprint on either side is a mismatch. A
/// needless restart costs about a second; keeping a stale daemon costs a wrong
/// answer and makes every rebuild look like it had no effect.
fn builds_match(running: Option<&str>, on_disk: Option<&str>) -> bool {
    match (running, on_disk) {
        (Some(a), Some(b)) => !a.is_empty() && a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::builds_match;

    #[test]
    fn identical_fingerprints_match() {
        assert!(builds_match(Some("1759490000"), Some("1759490000")));
    }

    #[test]
    fn a_rebuilt_sidecar_does_not_match() {
        // The bug this guards: same version string, newer binary on disk. The
        // running process must be replaced.
        assert!(!builds_match(Some("1759490000"), Some("1759499999")));
    }

    #[test]
    fn unknown_fingerprints_are_treated_as_mismatch() {
        assert!(!builds_match(None, Some("1759490000")));
        assert!(!builds_match(Some("1759490000"), None));
        assert!(!builds_match(None, None));
        // A daemon that crashed while formatting its own fingerprint.
        assert!(!builds_match(Some(""), Some("")));
    }
}
