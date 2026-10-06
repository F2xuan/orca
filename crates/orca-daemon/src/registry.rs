//! Registry manifest lookups, for image update detection.
//!
//! Answers one question: *what digest does this tag resolve to upstream right
//! now?* Comparing that against the local image's repo digest is what turns
//! "you have `alpine:3.20`" into "the `alpine:3.20` you have is no longer what
//! `alpine:3.20` means".
//!
//! Why this is not a `docker` CLI call: `docker manifest inspect` exists, but it
//! hides the failure modes. Private registries need a token from an auth
//! service, multi-arch tags need the right `Accept` header, and a registry that
//! simply does not report a digest has to be distinguished from one that
//! reported a *different* digest. Those distinctions are the whole value of the
//! feature, so they are made explicitly here.
//!
//! Nothing in this module is scheduled or backgrounded. Update checks run only
//! when a client asks, because an unprompted poller would hammer registries on
//! behalf of a user who never asked — and Docker Hub rate-limits anonymous
//! pulls by IP, so the cost of being wrong lands on the user's other tooling.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Context;
use orca_core::config::RegistryCredential;
use orca_core::registry::{self, BearerChallenge, ImageReference, UpdateCheck};

/// Manifests a registry may serve for a tag.
///
/// A multi-arch tag is served as an *index/list*, and registries choose the
/// representation from this header. Requesting only the single-image media type
/// would make a multi-arch tag look like it has a different digest than the
/// local index digest, producing a permanent false "update available".
const MANIFEST_ACCEPTS: &str = "application/vnd.oci.image.index.v1+json,\
application/vnd.docker.distribution.manifest.list.v2+json,\
application/vnd.oci.image.manifest.v1+json,\
application/vnd.docker.distribution.manifest.v2+json";

/// How long an obtained token is reused.
///
/// A conservative floor rather than the response's `expires_in`: a token used
/// slightly past its life costs one extra 401 and retry, while caching one
/// beyond its life is indistinguishable from the failure above.
const TOKEN_TTL: Duration = Duration::from_secs(60);

/// Registry hostnames that are served over plain HTTP in practice.
const INSECURE_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// Cached tokens as `(registry|repository, token, expires_at)`.
///
/// A `Vec` because `HashMap::new` is not `const` and the entry count here is the
/// number of repositories checked in the last minute — a handful. A linear scan
/// is not worth a `OnceLock` to avoid.
static TOKEN_CACHE: Mutex<Vec<(String, String, Instant)>> = Mutex::new(Vec::new());

/// Shared HTTP client so repeated checks reuse connections and one timeout
/// policy. Registry calls are on the request path, so the timeout is short: an
/// unreachable registry must fail the check, not hang the user's page.
static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("orca-desktop")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// The base URL for a registry host.
///
/// `https` everywhere except the loopback names, where a local registry is
/// almost never serving TLS — and `https://localhost:5000` fails with a
/// confusing handshake error rather than a clear "no TLS here".
fn registry_base_url(host: &str) -> String {
    let bare = host.split(':').next().unwrap_or(host);
    if INSECURE_HOSTS.contains(&bare) {
        format!("http://{host}")
    } else {
        format!("https://{host}")
    }
}

/// The stored credential for a registry, if any.
fn credential_for<'a>(
    credentials: &'a [RegistryCredential],
    registry: &str,
) -> Option<&'a RegistryCredential> {
    credentials
        .iter()
        .find(|credential| registry::server_matches(&credential.server, registry))
}

fn cached_token(key: &str) -> Option<String> {
    let cache = TOKEN_CACHE.lock().ok()?;
    let now = Instant::now();
    cache
        .iter()
        .find(|(cached_key, _, expires)| cached_key == key && *expires > now)
        .map(|(_, token, _)| token.clone())
}

fn cache_token(key: &str, token: String) {
    let Ok(mut cache) = TOKEN_CACHE.lock() else {
        return;
    };
    let now = Instant::now();
    cache.retain(|(_, _, expires)| *expires > now);
    cache.push((key.to_string(), token, now + TOKEN_TTL));
}

/// Exchange a `WWW-Authenticate` challenge for a bearer token.
///
/// Credentials go to the **token endpoint the challenge named**, never to the
/// registry itself. Sending them to the registry would hand the password to
/// whatever host answered the request — which is exactly the party the token
/// flow exists to keep the password away from.
async fn fetch_token(
    challenge: &BearerChallenge,
    credentials: Option<&RegistryCredential>,
) -> anyhow::Result<String> {
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(service) = &challenge.service {
        query.push(("service", service));
    }
    if let Some(scope) = &challenge.scope {
        query.push(("scope", scope));
    }

    let mut request = http_client().get(&challenge.realm).query(&query);
    if let Some(credentials) = credentials {
        // `password()` errors name only the server, never the secret (§2.1).
        let password = credentials.password()?;
        request = request.basic_auth(&credentials.username, Some(password));
    }

    let response = request
        .send()
        .await
        .context("cannot reach the registry token endpoint")?;
    if !response.status().is_success() {
        anyhow::bail!(
            "the registry token endpoint returned {}",
            response.status().as_u16()
        );
    }

    let body: serde_json::Value = response
        .json()
        .await
        .context("the registry token response was not JSON")?;
    body.get("token")
        .or_else(|| body.get("access_token"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("the registry token response contained no token"))
}

/// Read the digest from a response, falling back to hashing the body.
///
/// `Docker-Content-Digest` is only a *should* in the spec, so a missing header
/// is not an error — but the fallback must hash the exact bytes received, which
/// is why this takes the response rather than a parsed form.
async fn digest_of(response: reqwest::Response) -> anyhow::Result<String> {
    if let Some(digest) = response
        .headers()
        .get("docker-content-digest")
        .and_then(|value| value.to_str().ok())
    {
        return Ok(digest.to_string());
    }
    let bytes = response
        .bytes()
        .await
        .context("could not read the manifest body")?;
    Ok(registry::sha256_digest(&bytes))
}

/// The digest the registry currently serves for `reference`.
pub async fn remote_digest(
    reference: &ImageReference,
    credentials: &[RegistryCredential],
) -> anyhow::Result<String> {
    let url = format!(
        "{}{}",
        registry_base_url(&reference.registry),
        reference.manifest_path()
    );
    let cache_key = format!("{}|{}", reference.registry, reference.repository);
    let stored = credential_for(credentials, &reference.registry);

    let mut token = cached_token(&cache_key);

    // At most two attempts: the first with whatever token is cached (usually
    // none), the second after answering a 401 challenge. More attempts would
    // only repeat a failing request.
    for attempt in 0..2 {
        let mut request = http_client()
            .head(&url)
            .header(reqwest::header::ACCEPT, MANIFEST_ACCEPTS);
        if let Some(token) = &token {
            request = request.bearer_auth(token);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("cannot reach registry {}", reference.registry))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            let challenge = response
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .and_then(registry::parse_bearer_challenge);
            let Some(challenge) = challenge else {
                anyhow::bail!(
                    "registry {} requires authentication but sent no usable Bearer challenge \
                     (is the image private, and are credentials saved?)",
                    reference.registry
                );
            };
            let fetched = fetch_token(&challenge, stored).await?;
            cache_token(&cache_key, fetched.clone());
            token = Some(fetched);
            continue;
        }

        if !response.status().is_success() {
            anyhow::bail!(
                "registry {} answered {} for {}",
                reference.registry,
                response.status().as_u16(),
                reference.display()
            );
        }

        // Some registries set the digest only on `GET`, so fall back rather than
        // reporting the image as un-comparable.
        if response
            .headers()
            .get("docker-content-digest")
            .is_some()
        {
            return digest_of(response).await;
        }

        let mut get = http_client()
            .get(&url)
            .header(reqwest::header::ACCEPT, MANIFEST_ACCEPTS);
        if let Some(token) = &token {
            get = get.bearer_auth(token);
        }
        let response = get
            .send()
            .await
            .with_context(|| format!("cannot reach registry {}", reference.registry))?;
        if !response.status().is_success() {
            anyhow::bail!(
                "registry {} answered {} for {}",
                reference.registry,
                response.status().as_u16(),
                reference.display()
            );
        }
        return digest_of(response).await;
    }

    anyhow::bail!(
        "registry {} still refused the request after authenticating",
        reference.registry
    )
}

/// The map key identifying one distinct manifest.
pub fn reference_key(reference: &ImageReference) -> String {
    format!(
        "{}/{}:{}",
        reference.registry, reference.repository, reference.tag
    )
}

/// Turn an already-fetched remote digest into a verdict for one local image.
///
/// Split from the network call so a shared tag is fetched **once** but still
/// compared against *each* image's own digest. Folding the two together would
/// make two images that share a tag inherit each other's verdict.
///
/// A registry being unreachable is *data* — the UI shows "unknown" — not an
/// error that fails the batch: one offline registry must not blank out every
/// other image's status.
pub fn resolve(
    reference: &ImageReference,
    repo_digests: &[String],
    remote: Option<&Result<String, String>>,
) -> UpdateCheck {
    let local = registry::local_digest_for(repo_digests, reference);
    if local.is_none() {
        // No local digest means no comparison is possible. Report "unknown"
        // rather than asking the registry, and never "up to date".
        return UpdateCheck::compare(None, None);
    }
    match remote {
        Some(Ok(remote)) => UpdateCheck::compare(local.as_deref(), Some(remote)),
        Some(Err(reason)) => UpdateCheck::Unknown {
            reason: reason.clone(),
        },
        None => UpdateCheck::Unknown {
            reason: "this tag was not checked".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(server: &str, username: &str) -> RegistryCredential {
        RegistryCredential::new(server, "test", username, "hunter2")
    }

    #[test]
    fn loopback_registries_use_http() {
        // A local registry is almost never serving TLS, and `https://localhost`
        // fails with a handshake error that reads like a network problem.
        assert_eq!(registry_base_url("localhost"), "http://localhost");
        assert_eq!(registry_base_url("localhost:5000"), "http://localhost:5000");
        assert_eq!(registry_base_url("127.0.0.1:5001"), "http://127.0.0.1:5001");
    }

    #[test]
    fn everything_else_uses_https() {
        assert_eq!(registry_base_url("ghcr.io"), "https://ghcr.io");
        assert_eq!(
            registry_base_url("registry-1.docker.io"),
            "https://registry-1.docker.io"
        );
        // A host that merely *contains* a loopback name is not loopback.
        assert_eq!(
            registry_base_url("localhost.example.com"),
            "https://localhost.example.com"
        );
    }

    #[test]
    fn a_docker_hub_credential_is_found_under_its_stored_spelling() {
        // Users store `https://index.docker.io/v1/`; the request goes to
        // `registry-1.docker.io`. Failing to match means silent anonymous access,
        // which for a private image looks like "not found".
        let credentials = vec![credential("https://index.docker.io/v1/", "alice")];
        let found = credential_for(
            &credentials,
            orca_core::registry::DOCKER_HUB_API,
        )
        .expect("the Docker Hub credential should be found");
        assert_eq!(found.username, "alice");
    }

    #[test]
    fn credentials_are_not_borrowed_across_registries() {
        let credentials = vec![credential("https://ghcr.io", "alice")];
        assert!(credential_for(&credentials, "registry-1.docker.io").is_none());
        assert!(credential_for(&credentials, "quay.io").is_none());
        // But the right one is still found.
        assert_eq!(
            credential_for(&credentials, "ghcr.io").map(|c| c.username.as_str()),
            Some("alice")
        );
    }

    #[test]
    fn token_cache_round_trips_and_expires() {
        let key = "test-registry|library/alpine";
        cache_token(key, "token-a".to_string());
        assert_eq!(cached_token(key).as_deref(), Some("token-a"));
        // A different repository has its own token: scopes are per-repository,
        // so reusing one across repositories would produce 401s.
        assert_eq!(cached_token("test-registry|library/busybox"), None);
    }

    #[test]
    fn expired_tokens_are_not_returned() {
        // Insert directly with a past expiry rather than sleeping.
        {
            let mut cache = TOKEN_CACHE.lock().unwrap();
            cache.retain(|(k, _, _)| k != "expired-test");
            cache.push((
                "expired-test".to_string(),
                "stale".to_string(),
                Instant::now() - Duration::from_secs(1),
            ));
        }
        assert_eq!(cached_token("expired-test"), None);
    }

    #[test]
    fn manifest_accepts_cover_both_index_and_single_image_types() {
        // A multi-arch tag needs the index media type, or the registry serves a
        // single-arch manifest whose digest never matches the local index digest.
        assert!(MANIFEST_ACCEPTS.contains("oci.image.index.v1+json"));
        assert!(MANIFEST_ACCEPTS.contains("manifest.list.v2+json"));
        assert!(MANIFEST_ACCEPTS.contains("oci.image.manifest.v1+json"));
        assert!(MANIFEST_ACCEPTS.contains("manifest.v2+json"));
    }
}
