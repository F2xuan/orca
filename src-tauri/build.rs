use std::path::PathBuf;

fn ensure_stub(bin_dir: &PathBuf, name: &str, target: &str) {
    let bin_name = if target.contains("windows") {
        format!("{name}-{target}.exe")
    } else {
        format!("{name}-{target}")
    };
    let bin_path = bin_dir.join(&bin_name);

    if !bin_path.exists() {
        std::fs::create_dir_all(bin_dir).ok();
        // Create an empty stub — enough to pass Tauri's resource check.
        // It will be replaced by the actual binary at build/release time.
        std::fs::write(&bin_path, b"").ok();
        println!(
            "cargo:warning=Created stub binary at {}, build the real binary for production use",
            bin_path.display()
        );
    }
}

fn main() {
    // Ensure external binary stubs exist so `cargo tauri dev` / `cargo check`
    // work without needing to build daemon/CLI first.
    // Real binaries are placed here by the build/release pipeline.
    let target = std::env::var("TAURI_ENV_TARGET_TRIPLE")
        .or_else(|_| std::env::var("TARGET"))
        .unwrap_or_else(|_| "x86_64-unknown-linux-gnu".into());

    let bin_dir = PathBuf::from("binaries");
    ensure_stub(&bin_dir, "orca-daemon", &target);
    ensure_stub(&bin_dir, "orca", &target);

    tauri_build::build()
}
