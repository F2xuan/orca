use orca_core::templates::{AppTemplate, PASSWORD_PLACEHOLDER};

/// Community catalog URL — a static `templates.json` document.
///
/// Self-hosted on `orca.9988770.xyz` (Cloudflare-fronted, `cache-control:
/// max-age=60`). The body is a plain `Vec<AppTemplate>`; see
/// [`fetch_community_templates`] for the size cap and cache behaviour.
const CATALOG_URL: &str = "https://orca.9988770.xyz/templates.json";

/// Cache duration: 1 hour.
const CACHE_MAX_AGE_SECS: u64 = 3600;

/// Max accepted community-template catalog body size (2 MiB). Anything
/// bigger is almost certainly an error or attack; we log and discard it.
const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;

/// Path to cached community templates.
fn community_cache_path() -> std::path::PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    config_dir.join("orca").join("community-templates.json")
}

/// Path to user-created templates.
fn user_templates_path() -> std::path::PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    config_dir.join("orca").join("templates.json")
}

/// Delete the cache file to force a re-fetch on next request.
pub async fn invalidate_cache() {
    let _ = tokio::fs::remove_file(community_cache_path()).await;
}

/// Fetch community templates from the online catalog.
/// Cached locally for 1 hour. Falls back to cache if offline.
pub async fn fetch_community_templates() -> Vec<AppTemplate> {
    let cache_path = community_cache_path();

    // Check if cache is fresh enough
    let cache_fresh = match tokio::fs::metadata(&cache_path).await {
        Ok(m) => m
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age: std::time::Duration| age.as_secs() < CACHE_MAX_AGE_SECS)
            .unwrap_or(false),
        Err(_) => false,
    };

    if cache_fresh
        && let Ok(data) = tokio::fs::read_to_string(&cache_path).await
        && let Ok(templates) = serde_json::from_str::<Vec<AppTemplate>>(&data)
    {
        return templates;
    }

    // Cache is stale or missing — fetch from the web, streaming with a cap.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    if let Ok(resp) = client.get(CATALOG_URL).send().await
        && resp.status().is_success()
    {
        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut over_cap = false;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if buf.len() + bytes.len() > MAX_CATALOG_BYTES {
                        over_cap = true;
                        break;
                    }
                    buf.extend_from_slice(&bytes);
                }
                Err(e) => {
                    tracing::warn!("error streaming community template catalog: {e}");
                    break;
                }
            }
        }
        if over_cap {
            tracing::warn!(
                "community template catalog exceeded {} bytes; refusing to process",
                MAX_CATALOG_BYTES
            );
            return vec![];
        }
        if let Ok(body) = std::str::from_utf8(&buf)
            && let Ok(templates) = serde_json::from_str::<Vec<AppTemplate>>(body)
        {
            let _ = tokio::fs::write(&cache_path, body).await;
            return templates;
        }
    }

    // Fall back to stale cache
    if let Ok(data) = tokio::fs::read_to_string(&cache_path).await {
        return serde_json::from_str(&data).unwrap_or_default();
    }

    vec![]
}

/// Load user-defined templates from disk.
pub async fn load_user_templates() -> Vec<AppTemplate> {
    let path = user_templates_path();
    match tokio::fs::read_to_string(&path).await {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => vec![],
    }
}

/// Save user-defined templates to disk.
pub async fn save_user_templates(templates: &[AppTemplate]) -> anyhow::Result<()> {
    let path = user_templates_path();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let data = serde_json::to_string_pretty(templates)?;
    tokio::fs::write(&path, data).await?;
    Ok(())
}

/// The random secret substituted for [`PASSWORD_PLACEHOLDER`].
///
/// 16 characters from the OS CSPRNG — these values become database/session/API
/// secrets. There is exactly **one** definition of the generated secret, so the
/// value shown in the UI and the value a leftover placeholder falls back to can
/// never diverge in length or source.
pub fn placeholder_secret() -> String {
    use rand::Rng;
    use rand::distributions::Alphanumeric;
    rand::rngs::OsRng
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

/// Env keys of a template that still carry [`PASSWORD_PLACEHOLDER`].
///
/// Metadata only — never contains a secret.
fn placeholder_env_keys(env: &[String]) -> Vec<String> {
    env.iter()
        .filter(|spec| spec.contains(PASSWORD_PLACEHOLDER))
        .filter_map(|spec| spec.split_once('=').map(|(key, _)| key.trim().to_string()))
        .collect()
}

/// Load every template (community catalog + user-defined) exactly as stored,
/// with [`PASSWORD_PLACEHOLDER`] left intact.
///
/// This is what the deploy paths use. They must **not** see a freshly substituted
/// secret: the client already holds the one it was shown when the catalog was
/// listed, and sends it back in `env` / `compose_yaml`. Re-minting here would put
/// a different password in the response than the one the container receives —
/// the very mismatch that showing the generated value is meant to eliminate.
pub async fn raw_templates() -> Vec<AppTemplate> {
    let mut templates = Vec::new();

    // Load cached community templates
    let cache_path = community_cache_path();
    if let Ok(data) = tokio::fs::read_to_string(&cache_path).await
        && let Ok(community) = serde_json::from_str::<Vec<AppTemplate>>(&data)
    {
        templates.extend(community);
    }

    // Add user templates (skip duplicates by id)
    for t in load_user_templates().await {
        if !templates.iter().any(|existing| existing.id == t.id) {
            templates.push(t);
        }
    }

    for t in &mut templates {
        t.generated_password_keys = placeholder_env_keys(&t.default_env);
    }

    templates
}

/// Templates for the catalog/UI: [`raw_templates`] with
/// [`PASSWORD_PLACEHOLDER`] substituted by a random secret.
///
/// The substitution covers every surface the UI can see — `default_env`, the
/// compose YAML and `notes` — so the client sends the real secret straight back
/// when it deploys and the credential the user reads in the dialog is the one the
/// container is created with. One secret per template is shared by all three, so
/// a stack's database password and the service consuming it agree.
///
/// `generated_password_keys` reports which env keys were substituted, so the UI
/// can say "generated" rather than claiming a default password.
pub async fn all_templates() -> Vec<AppTemplate> {
    let mut templates = raw_templates().await;

    for t in &mut templates {
        let secret = placeholder_secret();
        for env in &mut t.default_env {
            if env.contains(PASSWORD_PLACEHOLDER) {
                *env = env.replace(PASSWORD_PLACEHOLDER, &secret);
            }
        }
        t.notes = t.notes.replace(PASSWORD_PLACEHOLDER, &secret);
        if let Some(yaml) = &mut t.compose_yaml {
            *yaml = yaml.replace(PASSWORD_PLACEHOLDER, &secret);
        }
    }

    templates
}
