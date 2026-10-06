//! Image reference parsing and digest comparison.
//!
//! Everything here is pure: no network, no Docker. That is deliberate, because
//! the hard part of "is there a newer `alpine:3.20`?" is not the HTTP call — it
//! is knowing what `alpine:3.20` *means*, which follows Docker's normalisation
//! rules and is full of cases that only bite in production.
//!
//! The comparison itself is a digest comparison. A tag is a mutable pointer;
//! the digest is the only thing that actually identifies content. So "an update
//! is available" means precisely: *the registry now resolves this tag to a
//! different digest than the one this local image was pulled as*. Nothing here
//! guesses from version numbers or creation dates.
//!
//! The result type is deliberately three-valued. "I could not reach the
//! registry" must never be reported as "you are up to date" — that is the same
//! tri-state discipline as `Image::used_by`, and for the same reason: a UI that
//! claims something it does not know is worse than one that admits ignorance.

use serde::{Deserialize, Serialize};

/// The registry Docker Hub images are actually served from.
///
/// Not `docker.io`: that is the *name*, while `registry-1.docker.io` is the API
/// endpoint. Using the wrong one turns every Docker Hub check into a 404.
pub const DOCKER_HUB_API: &str = "registry-1.docker.io";

/// A parsed, normalised image reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageReference {
    /// Registry API host, e.g. `registry-1.docker.io` or `ghcr.io`.
    pub registry: String,
    /// Repository path including any namespace, e.g. `library/alpine`.
    pub repository: String,
    /// Tag, defaulted to `latest` when the reference named none.
    pub tag: String,
}

impl ImageReference {
    /// The `Accept`-able manifest URL path for this reference.
    pub fn manifest_path(&self) -> String {
        format!("/v2/{}/manifests/{}", self.repository, self.tag)
    }

    /// The repository as a user would recognise it, for messages.
    pub fn display(&self) -> String {
        format!("{}:{}", self.repository, self.tag)
    }
}

/// Split a name into its registry and repository parts, applying Docker's rules.
///
/// The rule that matters: the first path component is a registry **only** if it
/// contains a `.` or a `:`, or is exactly `localhost`. So `myregistry/app` is
/// `docker.io/myregistry/app` — a namespace, not a self-hosted registry — while
/// `myregistry.io/app` is a registry. Getting this backwards is the classic way
/// to send every request to the wrong host.
fn split_registry(name: &str) -> (String, String) {
    match name.split_once('/') {
        Some((first, rest))
            if first.contains('.') || first.contains(':') || first == "localhost" =>
        {
            let registry = match first {
                // `docker.io` and `index.docker.io` are names for the same
                // registry; requests only work against the API host.
                "docker.io" | "index.docker.io" => DOCKER_HUB_API.to_string(),
                other => other.to_string(),
            };
            (registry, rest.to_string())
        }
        // No recognisable registry: the whole thing is a Docker Hub path.
        _ => (DOCKER_HUB_API.to_string(), name.to_string()),
    }
}

/// Apply Docker Hub's implicit `library/` namespace.
///
/// Official images are stored under `library/`, so `alpine` is really
/// `library/alpine`. Omitting the prefix makes the registry return 401 or 404
/// rather than the manifest.
fn normalize_repository(registry: &str, repository: &str) -> String {
    if registry == DOCKER_HUB_API && !repository.contains('/') {
        format!("library/{repository}")
    } else {
        repository.to_string()
    }
}

/// Parse an image reference into registry, repository, and tag.
///
/// Also accepts a bare image *name* (no tag), which is what `repo_tags` and
/// `repo_digests` entries look like when they are used for matching — the
/// default tag is only used for the manifest request, never for identity.
pub fn parse_reference(reference: &str) -> ImageReference {
    let reference = reference.trim();

    // Drop a digest suffix: `alpine:3.20@sha256:...` identifies the same
    // repository and tag as `alpine:3.20`, but the digest is not the tag.
    let without_digest = reference.split('@').next().unwrap_or(reference);

    // A tag is a `:` in the final path component. `localhost:5000/app` has no
    // tag — that colon is a port — which is why this looks after the last `/`.
    let (name, tag) = match without_digest.rsplit_once('/') {
        Some((head, last)) => match last.split_once(':') {
            Some((repo_last, tag)) if !tag.is_empty() => {
                (format!("{head}/{repo_last}"), tag.to_string())
            }
            _ => (without_digest.to_string(), "latest".to_string()),
        },
        None => match without_digest.split_once(':') {
            Some((name, tag)) if !tag.is_empty() => (name.to_string(), tag.to_string()),
            _ => (without_digest.to_string(), "latest".to_string()),
        },
    };

    let (registry, repository) = split_registry(&name);
    let repository = normalize_repository(&registry, &repository);

    ImageReference {
        registry,
        repository,
        tag,
    }
}

/// Find the local repo digest that belongs to `reference`.
///
/// `repo_digests` entries look like `alpine@sha256:abc…` or
/// `ghcr.io/user/app@sha256:def…`, and Docker writes the *familiar* repository
/// name, which is not always the normalised one (it may or may not carry the
/// `library/` prefix). So both spellings are accepted; anything else is not,
/// because matching too loosely would compare digests from different images and
/// report a permanent, wrong "update available".
pub fn local_digest_for(repo_digests: &[String], reference: &ImageReference) -> Option<String> {
    let wanted_normalized = format!("{}/{}", reference.registry, reference.repository);
    // `library/alpine` is also frequently written as plain `alpine`, and
    // `registry-1.docker.io/library/alpine` as `docker.io/library/alpine`.
    let wanted_familiar = reference.repository.clone();
    let wanted_docker_io = format!("docker.io/{}", reference.repository);

    for entry in repo_digests {
        let Some((name, digest)) = entry.split_once('@') else {
            continue;
        };
        if !digest.starts_with("sha256:") {
            continue;
        }
        let (entry_registry, entry_repository) = split_registry(name);
        let matches = name == wanted_familiar
            || name == wanted_docker_io
            || (entry_registry == reference.registry
                && normalize_repository(&entry_registry, &entry_repository) == reference.repository)
            || entry_repository == wanted_normalized;
        if matches {
            return Some(digest.to_string());
        }
    }
    None
}

/// Whether the local image matches what the registry currently serves.
///
/// Three-valued on purpose: [`UpdateCheck::Unknown`] exists so a network
/// failure, a missing local digest, or an unauthenticated private registry is
/// never rendered as "up to date".
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateCheck {
    /// Same digest: the tag still points at exactly what is on disk.
    UpToDate,
    /// The tag resolves to different content upstream.
    UpdateAvailable { local: String, remote: String },
    /// Could not be determined. `reason` is user-facing.
    Unknown { reason: String },
}

impl UpdateCheck {
    /// Compare a local digest against the digest the registry reports.
    ///
    /// `None` for either side yields `Unknown`, never a guess.
    pub fn compare(local: Option<&str>, remote: Option<&str>) -> Self {
        match (local, remote) {
            (Some(local), Some(remote)) => {
                if local == remote {
                    Self::UpToDate
                } else {
                    Self::UpdateAvailable {
                        local: local.to_string(),
                        remote: remote.to_string(),
                    }
                }
            }
            (None, _) => Self::Unknown {
                reason: "this image has no local repository digest, so it cannot be compared \
                         (locally built images have none)"
                    .to_string(),
            },
            (_, None) => Self::Unknown {
                reason: "the registry did not report a digest for this tag".to_string(),
            },
        }
    }

    pub fn is_update_available(&self) -> bool {
        matches!(self, Self::UpdateAvailable { .. })
    }
}

/// A parsed `WWW-Authenticate: Bearer …` challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerChallenge {
    pub realm: String,
    pub service: Option<String>,
    pub scope: Option<String>,
}

/// Parse a registry `WWW-Authenticate` challenge.
///
/// Registries answer an unauthenticated manifest request with `401` and a
/// challenge naming where to get a token. The parameters are a comma-separated
/// list of `key="value"` pairs whose *order is not specified* and which may
/// contain commas inside the quoted values (the scope is
/// `repository:x/y:pull`, and a comma can appear in a repository name), so this
/// scans quoted strings rather than splitting on commas.
///
/// Returns `None` for anything that is not a usable `Bearer` challenge — a
/// `Basic` challenge, or a `Bearer` with no realm, cannot produce a token.
pub fn parse_bearer_challenge(header: &str) -> Option<BearerChallenge> {
    let header = header.trim();
    let rest = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))?;

    let mut realm = None;
    let mut service = None;
    let mut scope = None;

    let mut chars = rest.chars().peekable();
    let mut key = String::new();
    let mut value = String::new();
    // Each iteration consumes one `key="value"` pair (or a bare token that is
    // ignored).
    while let Some(&c) = chars.peek() {
        match c {
            '=' => {
                chars.next();
                value.clear();
                if chars.peek() == Some(&'"') {
                    chars.next();
                    // Read to the closing quote, honouring backslash escapes so a
                    // quoted value may contain `"`.
                    let mut escaped = false;
                    for c in chars.by_ref() {
                        if escaped {
                            value.push(c);
                            escaped = false;
                        } else if c == '\\' {
                            escaped = true;
                        } else if c == '"' {
                            break;
                        } else {
                            value.push(c);
                        }
                    }
                } else {
                    while let Some(&c) = chars.peek() {
                        if c == ',' {
                            break;
                        }
                        value.push(c);
                        chars.next();
                    }
                }
                match key.trim().to_ascii_lowercase().as_str() {
                    "realm" if !value.is_empty() => realm = Some(value.clone()),
                    "service" if !value.is_empty() => service = Some(value.clone()),
                    "scope" if !value.is_empty() => scope = Some(value.clone()),
                    _ => {}
                }
                key.clear();
            }
            ',' => {
                chars.next();
                key.clear();
            }
            ' ' => {
                chars.next();
            }
            _ => {
                key.push(c);
                chars.next();
            }
        }
    }

    realm.map(|realm| BearerChallenge {
        realm,
        service,
        scope,
    })
}

/// Compute the digest of a manifest body.
///
/// Needed because `Docker-Content-Digest` is only a *should* in the registry
/// spec. When a registry omits it, the digest has to be computed from the bytes
/// — and it must be computed over the exact bytes received, so this takes a
/// slice rather than re-serialising any parsed form.
pub fn sha256_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Extract the API host from a stored registry server URL.
///
/// Stored credentials look like `https://ghcr.io` or
/// `https://index.docker.io/v1/`, so the scheme, any userinfo, and the path all
/// have to come off. A port is *kept*: `localhost:5000` and `localhost` are
/// different registries.
pub fn host_of_server_url(server: &str) -> Option<String> {
    let rest = server.trim();
    let rest = rest.split_once("://").map(|(_, rest)| rest).unwrap_or(rest);
    let host = rest.split('/').next().unwrap_or("").trim();
    // Drop `user:pass@` if present.
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Collapse the several names for Docker Hub's registry into one.
///
/// A user's stored credential is typically `https://index.docker.io/v1/`, while
/// manifest requests must go to `registry-1.docker.io`. Comparing the raw strings
/// would silently never match, and the check would fall back to anonymous access
/// — which for a private image looks like "not found" rather than "not
/// authenticated".
fn canonical_registry(host: &str) -> String {
    match host {
        "docker.io" | "index.docker.io" | "registry.hub.docker.com" => DOCKER_HUB_API.to_string(),
        other => other.to_string(),
    }
}

/// Whether a stored credential's server refers to `registry`.
pub fn server_matches(server: &str, registry: &str) -> bool {
    host_of_server_url(server)
        .map(|host| canonical_registry(&host) == canonical_registry(registry))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(reference: &str) -> (String, String, String) {
        let parsed = parse_reference(reference);
        (parsed.registry, parsed.repository, parsed.tag)
    }

    #[test]
    fn bare_official_image_gains_library_and_latest() {
        assert_eq!(
            parse("alpine"),
            (
                DOCKER_HUB_API.into(),
                "library/alpine".into(),
                "latest".into()
            )
        );
    }

    #[test]
    fn official_image_with_tag_keeps_the_tag() {
        assert_eq!(
            parse("alpine:3.20"),
            (
                DOCKER_HUB_API.into(),
                "library/alpine".into(),
                "3.20".into()
            )
        );
    }

    #[test]
    fn a_user_namespace_is_not_a_registry() {
        // The trap: `someuser` has no dot and no port, so it is a Docker Hub
        // namespace — NOT a self-hosted registry called `someuser`.
        assert_eq!(
            parse("someuser/app:v1"),
            (
                DOCKER_HUB_API.into(),
                "someuser/app".into(),
                "v1".into()
            )
        );
    }

    #[test]
    fn a_dotted_host_is_a_registry() {
        assert_eq!(
            parse("ghcr.io/user/app:1.2"),
            ("ghcr.io".into(), "user/app".into(), "1.2".into())
        );
    }

    #[test]
    fn a_port_marks_a_registry_and_is_not_a_tag() {
        assert_eq!(
            parse("localhost:5000/app"),
            ("localhost:5000".into(), "app".into(), "latest".into())
        );
        assert_eq!(
            parse("registry.internal:5000/team/app:9"),
            (
                "registry.internal:5000".into(),
                "team/app".into(),
                "9".into()
            )
        );
    }

    #[test]
    fn bare_localhost_is_treated_as_a_registry() {
        // `localhost` is the one name that is a registry without a dot or port,
        // so this must not become `library/localhost/app`.
        assert_eq!(
            parse("localhost/app"),
            ("localhost".into(), "app".into(), "latest".into())
        );
    }

    #[test]
    fn docker_io_names_map_to_the_api_host() {
        for name in ["docker.io/library/alpine:3.20", "index.docker.io/library/alpine:3.20"] {
            assert_eq!(
                parse(name),
                (
                    DOCKER_HUB_API.into(),
                    "library/alpine".into(),
                    "3.20".into()
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn an_explicit_digest_is_not_a_tag() {
        assert_eq!(
            parse("alpine@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            (DOCKER_HUB_API.into(), "library/alpine".into(), "latest".into())
        );
        assert_eq!(
            parse("alpine:3.20@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            (DOCKER_HUB_API.into(), "library/alpine".into(), "3.20".into())
        );
    }

    #[test]
    fn nested_repositories_survive_parsing() {
        assert_eq!(
            parse("ghcr.io/org/team/app:v2"),
            ("ghcr.io".into(), "org/team/app".into(), "v2".into())
        );
    }

    #[test]
    fn whitespace_is_tolerated() {
        assert_eq!(
            parse("  alpine:3.20  "),
            (DOCKER_HUB_API.into(), "library/alpine".into(), "3.20".into())
        );
    }

    #[test]
    fn manifest_path_uses_the_normalised_parts() {
        let reference = parse_reference("alpine:3.20");
        assert_eq!(
            reference.manifest_path(),
            "/v2/library/alpine/manifests/3.20"
        );
        let reference = parse_reference("localhost:5000/app");
        assert_eq!(reference.manifest_path(), "/v2/app/manifests/latest");
    }

    // ---- local digest matching ----

    #[test]
    fn local_digest_is_found_by_familiar_name() {
        let reference = parse_reference("alpine:3.20");
        let digests = vec!["alpine@sha256:abc".to_string()];
        assert_eq!(
            local_digest_for(&digests, &reference).as_deref(),
            Some("sha256:abc")
        );
    }

    #[test]
    fn local_digest_is_found_by_normalised_name() {
        let reference = parse_reference("alpine:3.20");
        for spelling in [
            "library/alpine@sha256:abc",
            "docker.io/library/alpine@sha256:abc",
            "registry-1.docker.io/library/alpine@sha256:abc",
        ] {
            assert_eq!(
                local_digest_for(&[spelling.to_string()], &reference).as_deref(),
                Some("sha256:abc"),
                "{spelling} should match"
            );
        }
    }

    #[test]
    fn a_digest_for_a_different_image_is_not_borrowed() {
        // The dangerous failure: matching too loosely and comparing digests of
        // two different images, which reports a permanent false "update".
        let reference = parse_reference("alpine:3.20");
        let digests = vec![
            "busybox@sha256:bbb".to_string(),
            "ghcr.io/user/alpine@sha256:ccc".to_string(),
        ];
        assert_eq!(local_digest_for(&digests, &reference), None);
    }

    #[test]
    fn entries_without_a_usable_digest_are_skipped() {
        let reference = parse_reference("alpine:3.20");
        let digests = vec![
            "alpine".to_string(),
            "alpine@sha512:whatever".to_string(),
            "alpine@sha256:abc".to_string(),
        ];
        assert_eq!(
            local_digest_for(&digests, &reference).as_deref(),
            Some("sha256:abc")
        );
    }

    // ---- comparison ----

    #[test]
    fn equal_digests_are_up_to_date() {
        assert_eq!(
            UpdateCheck::compare(Some("sha256:a"), Some("sha256:a")),
            UpdateCheck::UpToDate
        );
    }

    #[test]
    fn differing_digests_report_an_update() {
        let check = UpdateCheck::compare(Some("sha256:a"), Some("sha256:b"));
        assert!(check.is_update_available());
        match check {
            UpdateCheck::UpdateAvailable { local, remote } => {
                assert_eq!(local, "sha256:a");
                assert_eq!(remote, "sha256:b");
            }
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_local_digest_is_unknown_not_an_update() {
        // Locally built images legitimately have no repo digest. Reporting that
        // as "update available" would be a permanent lie.
        let check = UpdateCheck::compare(None, Some("sha256:b"));
        assert!(!check.is_update_available());
        assert!(matches!(check, UpdateCheck::Unknown { .. }));
    }

    #[test]
    fn a_missing_remote_digest_is_unknown_not_up_to_date() {
        let check = UpdateCheck::compare(Some("sha256:a"), None);
        assert!(!check.is_update_available());
        match check {
            UpdateCheck::Unknown { reason } => assert!(reason.contains("registry")),
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn unknown_serializes_with_a_reason_the_ui_can_show() {
        let json = serde_json::to_value(UpdateCheck::Unknown {
            reason: "offline".into(),
        })
        .unwrap();
        assert_eq!(json["state"], "unknown");
        assert_eq!(json["reason"], "offline");

        let json = serde_json::to_value(UpdateCheck::UpToDate).unwrap();
        assert_eq!(json["state"], "up_to_date");

        let json = serde_json::to_value(UpdateCheck::UpdateAvailable {
            local: "sha256:a".into(),
            remote: "sha256:b".into(),
        })
        .unwrap();
        assert_eq!(json["state"], "update_available");
    }

    // ---- digest helper & server matching ----

    #[test]
    fn sha256_matches_known_vectors() {
        // The canonical empty-string SHA-256, so this cannot silently drift.
        assert_eq!(
            sha256_digest(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_digest(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn server_urls_yield_their_host() {
        assert_eq!(host_of_server_url("https://ghcr.io").as_deref(), Some("ghcr.io"));
        assert_eq!(
            host_of_server_url("https://index.docker.io/v1/").as_deref(),
            Some("index.docker.io")
        );
        assert_eq!(
            host_of_server_url("http://localhost:5000").as_deref(),
            Some("localhost:5000"),
            "a port distinguishes registries and must be kept"
        );
        assert_eq!(host_of_server_url("ghcr.io").as_deref(), Some("ghcr.io"));
        assert_eq!(
            host_of_server_url("https://user:pw@ghcr.io").as_deref(),
            Some("ghcr.io")
        );
        assert_eq!(host_of_server_url("   "), None);
    }

    #[test]
    fn docker_hub_credential_names_all_match() {
        // The case that would otherwise silently fall back to anonymous access.
        for server in [
            "https://index.docker.io/v1/",
            "https://docker.io",
            "https://registry-1.docker.io",
        ] {
            assert!(
                server_matches(server, DOCKER_HUB_API),
                "{server} should match the Docker Hub registry"
            );
        }
    }

    #[test]
    fn credentials_do_not_bleed_across_registries() {
        assert!(!server_matches("https://ghcr.io", DOCKER_HUB_API));
        assert!(!server_matches("https://index.docker.io/v1/", "ghcr.io"));
        // Different ports are different registries.
        assert!(!server_matches("http://localhost:5000", "localhost:5001"));
    }

    // ---- WWW-Authenticate ----

    #[test]
    fn parses_a_docker_hub_style_challenge() {
        let challenge = parse_bearer_challenge(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/alpine:pull""#,
        )
        .unwrap();
        assert_eq!(challenge.realm, "https://auth.docker.io/token");
        assert_eq!(challenge.service.as_deref(), Some("registry.docker.io"));
        assert_eq!(
            challenge.scope.as_deref(),
            Some("repository:library/alpine:pull")
        );
    }

    #[test]
    fn parameter_order_does_not_matter() {
        let challenge = parse_bearer_challenge(
            r#"Bearer scope="repository:x/y:pull", realm="https://r/token", service="s""#,
        )
        .unwrap();
        assert_eq!(challenge.realm, "https://r/token");
        assert_eq!(challenge.scope.as_deref(), Some("repository:x/y:pull"));
    }

    #[test]
    fn a_missing_service_is_not_fatal() {
        // Some registries omit `service`; the token endpoint still works.
        let challenge =
            parse_bearer_challenge(r#"Bearer realm="https://r/token""#).unwrap();
        assert_eq!(challenge.realm, "https://r/token");
        assert!(challenge.service.is_none());
    }

    #[test]
    fn non_bearer_challenges_are_rejected() {
        assert!(parse_bearer_challenge(r#"Basic realm="registry""#).is_none());
        // A Bearer with no realm cannot produce a token.
        assert!(parse_bearer_challenge(r#"Bearer service="s""#).is_none());
    }

    #[test]
    fn unquoted_values_are_accepted() {
        let challenge =
            parse_bearer_challenge("Bearer realm=https://r/token,service=s").unwrap();
        assert_eq!(challenge.realm, "https://r/token");
        assert_eq!(challenge.service.as_deref(), Some("s"));
    }

    #[test]
    fn an_empty_header_is_rejected() {
        assert!(parse_bearer_challenge("").is_none());
        assert!(parse_bearer_challenge("Bearer ").is_none());
    }

    #[test]
    fn escaped_quotes_inside_a_value_are_handled() {
        let challenge =
            parse_bearer_challenge(r#"Bearer realm="https://r/a\"b/token",scope="x""#).unwrap();
        assert_eq!(challenge.realm, r#"https://r/a"b/token"#);
        assert_eq!(challenge.scope.as_deref(), Some("x"));
    }
}
