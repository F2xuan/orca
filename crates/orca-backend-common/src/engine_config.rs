//! Docker Engine (`daemon.json`) configuration.
//!
//! Owns the daemon config file: where it lives, how to read it, and how to
//! write it back safely. This is the same file Docker Desktop's
//! *Settings → Docker Engine* pane edits.
//!
//! # Design rules
//!
//! 1. **Never clobber unrelated keys.** Users hand-edit this file and Docker
//!    Desktop writes its own keys into it. Every write merges into the existing
//!    object and touches only the keys we own.
//! 2. **Never overwrite a file we can't parse.** A corrupt config stops
//!    `dockerd` from starting at all, so a parse failure is a hard error rather
//!    than a reason to start fresh.
//! 3. **Validate before writing.** An invalid value (bad CIDR, unknown log
//!    driver) makes the engine fail to boot, which is far worse than rejecting
//!    the edit.
//! 4. **A restart is required.** `dockerd` reads this file only at startup, so
//!    the API reports `restart_required` instead of claiming the change is live.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// Where the *effective* daemon config lives on this platform.
///
/// * macOS / Windows — Docker Desktop's engine reads `~/.docker/daemon.json`.
///   On macOS `/etc/docker` belongs to the host, not to the VM that actually
///   runs `dockerd`, so a file written there would be silently ignored.
/// * Linux — a native `dockerd` reads `/etc/docker/daemon.json`.
///
/// Caveat: Docker Desktop for Linux also keeps its config in
/// `~/.docker/daemon.json`. When `~/.docker/desktop` exists we assume Docker
/// Desktop and prefer that path, so we never write to a file the *other* daemon
/// would ignore.
pub fn daemon_json_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        home.join(".docker").join("daemon.json")
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if home.join(".docker").join("desktop").exists() {
            home.join(".docker").join("daemon.json")
        } else {
            PathBuf::from("/etc/docker/daemon.json")
        }
    }
}

/// Read the daemon config as a JSON object.
///
/// A *missing* file is normal (fresh install) and yields `{}`. A *corrupt* file
/// is a hard error: silently replacing it would destroy whatever the user had
/// configured, and could stop `dockerd` from starting.
pub fn read_config() -> Result<Value> {
    let path = daemon_json_path();
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let parsed: Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "{} is not valid JSON; refusing to overwrite it",
            path.display()
        )
    })?;
    if !parsed.is_object() {
        bail!(
            "{} does not contain a JSON object; refusing to overwrite it",
            path.display()
        );
    }
    Ok(parsed)
}

/// Write a full config object.
///
/// Backs up the previous file and writes atomically, so an interrupted write
/// can never leave a truncated `daemon.json` behind.
pub fn write_config(cfg: &Value) -> Result<Option<PathBuf>> {
    let path = daemon_json_path();
    if !cfg.is_object() {
        bail!("daemon config must be a JSON object");
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let backup = if path.exists() {
        let bak = path.with_extension("json.bak");
        std::fs::copy(&path, &bak).with_context(|| format!("backing up to {}", bak.display()))?;
        Some(bak)
    } else {
        None
    };

    // Atomic: write a sibling temp file, then rename over the target.
    let tmp = path.with_extension("json.tmp");
    let mut body = serde_json::to_string_pretty(cfg)?;
    body.push('\n');
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;

    Ok(backup)
}

fn mirror_list(cfg: &Value) -> Vec<String> {
    cfg.get("registry-mirrors")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The mirror list currently written on disk.
pub fn read_mirrors() -> Result<Vec<String>> {
    Ok(mirror_list(&read_config()?))
}

// ───────────────────────── running-engine state ─────────────────────────

/// Values the *running* engine reports, used to decide whether a restart is
/// still pending. Only fields `docker info` exposes can be compared; anything
/// else must be reported as "cannot tell" rather than assumed applied.
#[derive(Debug, Default, serde::Serialize)]
pub struct RunningEngine {
    pub mirrors: Vec<String>,
    pub log_driver: Option<String>,
    pub storage_driver: Option<String>,
}

/// Query the running engine. `None` when it can't be reached.
pub async fn running_engine() -> Option<RunningEngine> {
    // One `docker info` call yields every field via a JSON-ish template, which
    // avoids three separate CLI invocations.
    let out = crate::environment::run_cmd(
        "docker",
        &[
            "info",
            "--format",
            "{{json .RegistryConfig.Mirrors}}|{{.LoggingDriver}}|{{.Driver}}",
        ],
    )
    .await
    .ok()?;

    let mut parts = out.trim().splitn(3, '|');
    let mirrors_raw = parts.next().unwrap_or("null");
    let log_driver = parts.next().map(str::to_string).filter(|s| !s.is_empty());
    let storage_driver = parts.next().map(str::to_string).filter(|s| !s.is_empty());

    // `docker info` prints the literal `null` (not `[]`) when no mirrors are
    // configured, so parse `Option<Vec<String>>` — parsing straight into
    // `Vec<String>` would treat "no mirrors" as "probe failed".
    let mirrors = serde_json::from_str::<Option<Vec<String>>>(mirrors_raw)
        .ok()
        .flatten()
        .unwrap_or_default();

    Some(RunningEngine {
        mirrors,
        log_driver,
        storage_driver,
    })
}

fn normalize(v: &[String]) -> Vec<String> {
    let mut out: Vec<String> = v
        .iter()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .collect();
    out.sort();
    out
}

/// Whether two mirror lists differ in content, ignoring order and a trailing
/// `/` (neither changes what dockerd does).
pub fn mirrors_differ(a: &[String], b: &[String]) -> bool {
    normalize(a) != normalize(b)
}

fn str_field(cfg: &Value, key: &str) -> Option<String> {
    cfg.get(key).and_then(Value::as_str).map(str::to_string)
}

/// dockerd's log driver when `daemon.json` does not set one.
const DEFAULT_LOG_DRIVER: &str = "json-file";

/// Whether the on-disk config holds settings the running engine hasn't picked
/// up. Pure, so the default handling can be tested without a live daemon.
///
/// The subtle part is **absent keys**. An unset key means "engine default", not
/// "different from whatever the engine reports" — comparing `None` against
/// `Some("json-file")` would make a fresh `daemon.json` claim a restart is
/// pending forever.
pub fn differs_from_running(disk: &Value, running: &RunningEngine) -> bool {
    if mirrors_differ(&mirror_list(disk), &running.mirrors) {
        return true;
    }

    let log_pending = match str_field(disk, "log-driver") {
        // Explicitly set: it must match exactly.
        Some(d) => Some(&d) != running.log_driver.as_ref(),
        // Not set: only a pending restart if the live value is *not* the default
        // (which means the key was removed from disk but the engine still runs it).
        None => running
            .log_driver
            .as_deref()
            .is_some_and(|v| v != DEFAULT_LOG_DRIVER),
    };
    if log_pending {
        return true;
    }

    // Storage drivers have no portable default (they depend on platform and
    // filesystem), so only an explicit mismatch is reported. Removing the key
    // after the engine started is therefore undetectable — acceptable, because
    // changing the storage driver is drastic and rare.
    match str_field(disk, "storage-driver") {
        Some(d) => Some(&d) != running.storage_driver.as_ref(),
        None => false,
    }
}

/// Whether the on-disk config differs from what the engine is running.
///
/// Only the fields `docker info` exposes are compared (mirrors, log driver,
/// storage driver). Other keys cannot be checked here, which is why the UI
/// states the general "engine edits need a restart" rule as well, instead of
/// relying on this flag alone.
pub async fn restart_required() -> bool {
    let disk = read_config().unwrap_or_else(|_| json!({}));
    match running_engine().await {
        Some(running) => differs_from_running(&disk, &running),
        // Engine unreachable — cannot prove it picked the change up, so assume
        // a restart is needed rather than reporting a live config.
        None => true,
    }
}

// ───────────────────────── validation ─────────────────────────

/// Log drivers dockerd accepts. An unknown one makes the engine fail to start.
const LOG_DRIVERS: &[&str] = &[
    "json-file",
    "local",
    "journald",
    "syslog",
    "fluentd",
    "gelf",
    "awslogs",
    "splunk",
    "gcplogs",
    "none",
];

fn no_ws_or_quote(s: &str) -> bool {
    !s.chars().any(|c| c.is_whitespace() || c == '"')
}

fn is_http_url(s: &str) -> bool {
    (s.starts_with("http://") || s.starts_with("https://")) && no_ws_or_quote(s)
}

/// Mirrors must be full http(s) URLs — dockerd refuses anything else at startup.
pub fn validate_registry_mirrors(list: &[String]) -> Result<()> {
    if list.len() > 10 {
        bail!(
            "too many mirrors ({}); dockerd probes them in order, keep the list short",
            list.len()
        );
    }
    for m in list {
        if !is_http_url(m) {
            bail!("mirror '{m}' must start with http:// or https://");
        }
    }
    Ok(())
}

/// `insecure-registries` are `host[:port]` or CIDR — deliberately **without** a
/// scheme, the opposite of the mirror format and a common mistake.
pub fn validate_insecure_registries(list: &[String]) -> Result<()> {
    if list.len() > 20 {
        bail!("too many insecure registries ({})", list.len());
    }
    for r in list {
        if r.contains("://") {
            bail!("insecure registry '{r}' must not include a scheme (use host:port)");
        }
        if !no_ws_or_quote(r) {
            bail!("insecure registry '{r}' contains invalid characters");
        }
    }
    Ok(())
}

/// DNS servers must be literal IPs; a hostname here makes dockerd refuse to start.
pub fn validate_dns(list: &[String]) -> Result<()> {
    if list.len() > 4 {
        bail!("at most 4 DNS servers are supported");
    }
    for d in list {
        if d.parse::<std::net::IpAddr>().is_err() {
            bail!("DNS server '{d}' is not a valid IP address");
        }
    }
    Ok(())
}

pub fn validate_mtu(mtu: u32) -> Result<()> {
    if !(576..=65535).contains(&mtu) {
        bail!("MTU {mtu} is out of range (576-65535)");
    }
    Ok(())
}

pub fn validate_bip(bip: &str) -> Result<()> {
    let (addr, prefix) = bip
        .split_once('/')
        .context("bridge IP must be CIDR notation, e.g. 172.30.0.1/24")?;
    addr.parse::<std::net::Ipv4Addr>()
        .with_context(|| format!("'{addr}' is not a valid IPv4 address"))?;
    let p: u8 = prefix.parse().context("invalid CIDR prefix length")?;
    if p > 32 {
        bail!("CIDR prefix {p} is out of range (0-32)");
    }
    Ok(())
}

pub fn validate_log_driver(driver: &str) -> Result<()> {
    if !LOG_DRIVERS.contains(&driver) {
        bail!(
            "unknown log driver '{driver}' (supported: {})",
            LOG_DRIVERS.join(", ")
        );
    }
    Ok(())
}

/// Docker accepts a number with an optional k/m/g suffix, e.g. `10m`.
pub fn validate_log_max_size(v: &str) -> Result<()> {
    let lower = v.to_ascii_lowercase();
    // Strip exactly one optional unit suffix.
    let digits = match lower.as_bytes().last() {
        Some(b'k') | Some(b'm') | Some(b'g') => &lower[..lower.len() - 1],
        _ => lower.as_str(),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        bail!("log max-size '{v}' must be a number with an optional k/m/g suffix (e.g. 10m)");
    }
    Ok(())
}

pub fn validate_log_max_file(v: &str) -> Result<()> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        bail!("log max-file '{v}' must be a positive integer");
    }
    if v.parse::<u32>().map(|n| n == 0).unwrap_or(true) {
        bail!("log max-file must be greater than zero");
    }
    Ok(())
}

pub fn validate_concurrent(n: u32) -> Result<()> {
    if !(1..=64).contains(&n) {
        bail!("{n} is out of range (1-64)");
    }
    Ok(())
}

// ───────────────────────── structured editing ─────────────────────────

/// A partial daemon.json edit. Absent/`null` means "remove this key", which is
/// what the UI sends: it always submits its complete form state.
#[derive(Debug, Default, Deserialize)]
pub struct EngineConfigPatch {
    #[serde(default, rename = "registry-mirrors")]
    pub registry_mirrors: Option<Vec<String>>,
    #[serde(default, rename = "insecure-registries")]
    pub insecure_registries: Option<Vec<String>>,
    #[serde(default)]
    pub dns: Option<Vec<String>>,
    #[serde(default)]
    pub mtu: Option<u32>,
    #[serde(default)]
    pub bip: Option<String>,
    #[serde(default, rename = "log-driver")]
    pub log_driver: Option<String>,
    #[serde(default, rename = "log-opts")]
    pub log_opts: Option<LogOpts>,
    #[serde(default, rename = "max-concurrent-downloads")]
    pub max_concurrent_downloads: Option<u32>,
    #[serde(default, rename = "max-concurrent-uploads")]
    pub max_concurrent_uploads: Option<u32>,
    #[serde(default)]
    pub experimental: Option<bool>,
    #[serde(default, rename = "live-restore")]
    pub live_restore: Option<bool>,
    #[serde(default, rename = "userland-proxy")]
    pub userland_proxy: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct LogOpts {
    #[serde(default, rename = "max-size")]
    pub max_size: Option<String>,
    #[serde(default, rename = "max-file")]
    pub max_file: Option<String>,
}

type Obj = Map<String, Value>;

fn set_array<F>(obj: &mut Obj, key: &str, v: Option<&[String]>, validate: F) -> Result<()>
where
    F: Fn(&[String]) -> Result<()>,
{
    match v {
        Some(list) => {
            let cleaned: Vec<String> = list
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if cleaned.is_empty() {
                obj.remove(key);
            } else {
                validate(&cleaned)?;
                obj.insert(key.to_string(), json!(cleaned));
            }
        }
        None => {
            obj.remove(key);
        }
    }
    Ok(())
}

fn set_str<F>(obj: &mut Obj, key: &str, v: Option<&str>, validate: F) -> Result<()>
where
    F: Fn(&str) -> Result<()>,
{
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            validate(s)?;
            obj.insert(key.to_string(), json!(s));
        }
        None => {
            obj.remove(key);
        }
    }
    Ok(())
}

fn set_num<F>(obj: &mut Obj, key: &str, v: Option<u32>, validate: F) -> Result<()>
where
    F: Fn(u32) -> Result<()>,
{
    match v {
        Some(n) => {
            validate(n)?;
            obj.insert(key.to_string(), json!(n));
        }
        None => {
            obj.remove(key);
        }
    }
    Ok(())
}

fn set_bool(obj: &mut Obj, key: &str, v: Option<bool>) {
    match v {
        Some(b) => {
            obj.insert(key.to_string(), json!(b));
        }
        None => {
            obj.remove(key);
        }
    }
}

/// `log-opts` is a shared object: other keys (e.g. `tag`) must survive, and the
/// object itself is dropped only once it becomes empty.
fn set_log_opts(obj: &mut Obj, opts: Option<&LogOpts>) {
    let Some(opts) = opts else {
        return;
    };

    let mut existing = obj
        .get("log-opts")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    match opts
        .max_size
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| validate_log_max_size(s).is_ok())
    {
        Some(v) => {
            existing.insert("max-size".to_string(), json!(v));
        }
        None => {
            existing.remove("max-size");
        }
    }

    match opts
        .max_file
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| validate_log_max_file(s).is_ok())
    {
        Some(v) => {
            existing.insert("max-file".to_string(), json!(v));
        }
        None => {
            existing.remove("max-file");
        }
    }

    if existing.is_empty() {
        obj.remove("log-opts");
    } else {
        obj.insert("log-opts".to_string(), Value::Object(existing));
    }
}

/// Validate every field of a patch without touching the filesystem.
///
/// Lets the API answer `400` for a bad value while still surfacing genuine I/O
/// failures as `500`, instead of collapsing both into one status.
pub fn validate_patch(patch: &EngineConfigPatch) -> Result<()> {
    if let Some(v) = patch.registry_mirrors.as_deref() {
        let cleaned = clean_list(v);
        if !cleaned.is_empty() {
            validate_registry_mirrors(&cleaned)?;
        }
    }
    if let Some(v) = patch.insecure_registries.as_deref() {
        let cleaned = clean_list(v);
        if !cleaned.is_empty() {
            validate_insecure_registries(&cleaned)?;
        }
    }
    if let Some(v) = patch.dns.as_deref() {
        let cleaned = clean_list(v);
        if !cleaned.is_empty() {
            validate_dns(&cleaned)?;
        }
    }
    if let Some(n) = patch.mtu {
        validate_mtu(n)?;
    }
    if let Some(b) = patch.bip.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        validate_bip(b)?;
    }
    if let Some(d) = patch.log_driver.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        validate_log_driver(d)?;
    }
    if let Some(o) = patch.log_opts.as_ref() {
        if let Some(s) = o.max_size.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            validate_log_max_size(s)?;
        }
        if let Some(s) = o.max_file.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            validate_log_max_file(s)?;
        }
    }
    if let Some(n) = patch.max_concurrent_downloads {
        validate_concurrent(n)?;
    }
    if let Some(n) = patch.max_concurrent_uploads {
        validate_concurrent(n)?;
    }
    Ok(())
}

fn clean_list(v: &[String]) -> Vec<String> {
    v.iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Apply a structured patch, preserving every key we don't manage.
///
/// Validation happens up front so a rejected edit leaves the file untouched.
pub fn apply_patch(patch: &EngineConfigPatch) -> Result<Option<PathBuf>> {
    validate_patch(patch)?;

    let mut cfg = read_config()?;
    let obj = cfg
        .as_object_mut()
        .context("daemon config is not a JSON object; refusing to overwrite it")?;

    set_array(
        obj,
        "registry-mirrors",
        patch.registry_mirrors.as_deref(),
        validate_registry_mirrors,
    )?;
    set_array(
        obj,
        "insecure-registries",
        patch.insecure_registries.as_deref(),
        validate_insecure_registries,
    )?;
    set_array(obj, "dns", patch.dns.as_deref(), validate_dns)?;
    set_num(obj, "mtu", patch.mtu, validate_mtu)?;
    set_str(obj, "bip", patch.bip.as_deref(), validate_bip)?;
    set_str(
        obj,
        "log-driver",
        patch.log_driver.as_deref(),
        validate_log_driver,
    )?;
    set_log_opts(obj, patch.log_opts.as_ref());
    set_num(
        obj,
        "max-concurrent-downloads",
        patch.max_concurrent_downloads,
        validate_concurrent,
    )?;
    set_num(
        obj,
        "max-concurrent-uploads",
        patch.max_concurrent_uploads,
        validate_concurrent,
    )?;
    set_bool(obj, "experimental", patch.experimental);
    set_bool(obj, "live-restore", patch.live_restore);
    set_bool(obj, "userland-proxy", patch.userland_proxy);

    write_config(&Value::Object(obj.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_mirror_accepts_and_rejects() {
        assert!(validate_registry_mirrors(&["https://docker.m.daocloud.io".into()]).is_ok());
        assert!(validate_registry_mirrors(&[]).is_ok());
        // A bare host would make dockerd fail to start.
        assert!(validate_registry_mirrors(&["docker.m.daocloud.io".into()]).is_err());
        assert!(validate_registry_mirrors(&["ftp://x".into()]).is_err());
        assert!(validate_registry_mirrors(&["https://a b".into()]).is_err());
    }

    #[test]
    fn validate_insecure_registries_rejects_scheme() {
        // The inverse of mirrors: a scheme here is the classic mistake.
        assert!(validate_insecure_registries(&["registry.local:5000".into()]).is_ok());
        assert!(validate_insecure_registries(&["https://registry.local:5000".into()]).is_err());
    }

    #[test]
    fn validate_dns_requires_literal_ip() {
        assert!(validate_dns(&["8.8.8.8".into(), "1.1.1.1".into()]).is_ok());
        assert!(validate_dns(&["dns.google".into()]).is_err());
    }

    #[test]
    fn validate_mtu_range() {
        assert!(validate_mtu(1500).is_ok());
        assert!(validate_mtu(1400).is_ok());
        assert!(validate_mtu(100).is_err());
        assert!(validate_mtu(70000).is_err());
    }

    #[test]
    fn validate_log_size_and_file() {
        assert!(validate_log_max_size("10m").is_ok());
        assert!(validate_log_max_size("100").is_ok());
        assert!(validate_log_max_size("2g").is_ok());
        assert!(validate_log_max_size("abc").is_err());
        assert!(validate_log_max_size("m10").is_err());
        assert!(validate_log_max_file("3").is_ok());
        assert!(validate_log_max_file("0").is_err());
        assert!(validate_log_max_file("x").is_err());
    }

    #[test]
    fn validate_log_driver_allowlist() {
        assert!(validate_log_driver("json-file").is_ok());
        assert!(validate_log_driver("local").is_ok());
        assert!(validate_log_driver("nope").is_err());
    }

    #[test]
    fn patch_removes_keys_on_none_and_keeps_unknown() {
        let mut obj: Obj = serde_json::from_str(
            r#"{"builder":{"gc":{}},"experimental":false,"registry-mirrors":["https://a"]}"#,
        )
        .unwrap();

        set_array(&mut obj, "registry-mirrors", None, validate_registry_mirrors).unwrap();
        set_bool(&mut obj, "experimental", Some(true));

        assert!(!obj.contains_key("registry-mirrors"));
        assert_eq!(obj.get("experimental"), Some(&json!(true)));
        // Unmanaged keys survive — this is what protects Docker Desktop's own
        // `builder` block from being wiped by an edit from our UI.
        assert!(obj.contains_key("builder"));
    }

    #[test]
    fn log_opts_preserves_foreign_keys_and_drops_when_empty() {
        let mut obj: Obj = serde_json::from_str(r#"{"log-opts":{"tag":"{{.Name}}"}}"#).unwrap();

        set_log_opts(
            &mut obj,
            Some(&LogOpts {
                max_size: Some("10m".into()),
                max_file: Some("3".into()),
            }),
        );
        let lo = obj.get("log-opts").unwrap().as_object().unwrap();
        assert_eq!(lo.get("max-size"), Some(&json!("10m")));
        assert_eq!(lo.get("max-file"), Some(&json!("3")));
        assert_eq!(lo.get("tag"), Some(&json!("{{.Name}}")));

        // Clearing both managed keys leaves `tag`, so the object stays.
        set_log_opts(
            &mut obj,
            Some(&LogOpts {
                max_size: None,
                max_file: None,
            }),
        );
        let lo = obj.get("log-opts").unwrap().as_object().unwrap();
        assert!(!lo.contains_key("max-size"));
        assert!(lo.contains_key("tag"));

        // With nothing left, the key is removed entirely.
        let mut only: Obj = serde_json::from_str(r#"{"log-opts":{"max-size":"1m"}}"#).unwrap();
        set_log_opts(&mut only, Some(&LogOpts::default()));
        assert!(!only.contains_key("log-opts"));
    }

    #[test]
    fn running_mirrors_parses_null_as_empty() {
        // `docker info` emits `null`, not `[]`, when no mirrors are configured.
        let parsed: Option<Vec<String>> = serde_json::from_str("null").unwrap();
        assert!(parsed.unwrap_or_default().is_empty());
        let parsed: Option<Vec<String>> = serde_json::from_str(r#"["https://a"]"#).unwrap();
        assert_eq!(parsed.unwrap_or_default(), vec!["https://a".to_string()]);
    }

    #[test]
    fn normalize_ignores_order_and_trailing_slash() {
        assert!(!mirrors_differ(
            &["https://a/".to_string(), "https://b".to_string()],
            &["https://b".to_string(), "https://a".to_string()]
        ));
        assert!(mirrors_differ(&["https://a".into()], &[]));
    }

    #[test]
    fn patch_deserializes_daemon_json_key_names() {
        let p: EngineConfigPatch = serde_json::from_str(
            r#"{"registry-mirrors":["https://a"],"log-driver":"local",
                "log-opts":{"max-size":"10m"},"live-restore":true,"mtu":1400}"#,
        )
        .unwrap();
        assert_eq!(p.registry_mirrors.unwrap(), vec!["https://a".to_string()]);
        assert_eq!(p.log_driver.as_deref(), Some("local"));
        assert_eq!(p.log_opts.unwrap().max_size.as_deref(), Some("10m"));
        assert_eq!(p.live_restore, Some(true));
        assert_eq!(p.mtu, Some(1400));
    }

    #[test]
    fn validate_bip_requires_cidr() {
        assert!(validate_bip("172.30.0.1/24").is_ok());
        assert!(validate_bip("172.30.0.1").is_err());
        assert!(validate_bip("not-an-ip/24").is_err());
        assert!(validate_bip("172.30.0.1/40").is_err());
    }

    fn running(mirrors: &[&str], log: Option<&str>, storage: Option<&str>) -> RunningEngine {
        RunningEngine {
            mirrors: mirrors.iter().map(|s| s.to_string()).collect(),
            log_driver: log.map(str::to_string),
            storage_driver: storage.map(str::to_string),
        }
    }

    /// Regression: a fresh `daemon.json` (`{"builder":{...}}`, no `log-driver`,
    /// no `storage-driver`) against a running engine reporting `json-file` /
    /// `overlay2` used to report a pending restart forever, because `None` was
    /// compared directly against `Some("json-file")`.
    #[test]
    fn absent_keys_use_engine_defaults_not_mismatch() {
        let disk: Value = serde_json::from_str(r#"{"builder":{"gc":{}}}"#).unwrap();
        assert!(!differs_from_running(
            &disk,
            &running(&[], Some("json-file"), Some("overlay2"))
        ));
    }

    #[test]
    fn explicit_log_driver_mismatch_is_reported() {
        let disk: Value = serde_json::from_str(r#"{"log-driver":"local"}"#).unwrap();
        assert!(differs_from_running(
            &disk,
            &running(&[], Some("json-file"), Some("overlay2"))
        ));
        // Once the engine runs it, the banner must clear.
        assert!(!differs_from_running(
            &disk,
            &running(&[], Some("local"), Some("overlay2"))
        ));
    }

    /// Removing `log-driver` reverts to the default; if the engine still runs a
    /// non-default driver, a restart really is pending.
    #[test]
    fn removing_log_driver_while_engine_runs_non_default_is_reported() {
        let disk: Value = serde_json::from_str(r#"{}"#).unwrap();
        assert!(differs_from_running(
            &disk,
            &running(&[], Some("local"), Some("overlay2"))
        ));
    }

    #[test]
    fn mirror_difference_always_reported() {
        let disk: Value = serde_json::from_str(r#"{"registry-mirrors":["https://a"]}"#).unwrap();
        assert!(differs_from_running(
            &disk,
            &running(&[], Some("json-file"), Some("overlay2"))
        ));
        assert!(!differs_from_running(
            &disk,
            &running(&["https://a"], Some("json-file"), Some("overlay2"))
        ));
    }

    /// No portable default exists for the storage driver, so an absent key must
    /// not be treated as a mismatch.
    #[test]
    fn absent_storage_driver_is_not_a_mismatch() {
        let disk: Value = serde_json::from_str(r#"{}"#).unwrap();
        assert!(!differs_from_running(
            &disk,
            &running(&[], Some("json-file"), Some("btrfs"))
        ));
        let disk: Value = serde_json::from_str(r#"{"storage-driver":"btrfs"}"#).unwrap();
        assert!(differs_from_running(
            &disk,
            &running(&[], Some("json-file"), Some("overlay2"))
        ));
    }

    #[test]
    fn daemon_json_path_ends_with_daemon_json() {
        assert!(daemon_json_path().ends_with("daemon.json"));
    }
}
