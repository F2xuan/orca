//! Scrubbing secret material on its way out of the daemon.
//!
//! Two independent layers, because they fail in different ways:
//!
//! 1. [`redact_url_credentials`] rewrites the userinfo of any URL it finds
//!    (`scheme://user:password@host` → `scheme://***@host`). This is *shape*
//!    based, so it works on secrets the daemon has never seen — a registry
//!    token the user typed into a clone URL, or a URL echoed back by `git`,
//!    `reqwest`, or a registry client. It needs no configuration and cannot go
//!    stale.
//! 2. [`SecretRegistry`] blanks exact *values* the daemon does know: the API
//!    token, provider keys, webhook secret, registry passwords, remote-host
//!    tokens. This catches a secret that surfaces outside a URL, but only for
//!    secrets that have been registered.
//!
//! Neither layer is a substitute for not putting secrets in the message in the
//! first place — see the call sites in `orca-daemon` for the places where the
//! text is avoidable rather than merely scrubbable.
//!
//! What this deliberately does **not** do: redact daemon logs. The log is a
//! local file and is the only place the full `anyhow` chain is kept, which is
//! what makes a failure diagnosable. Redacting it would trade a local file for
//! a support burden. The boundary is "bytes that leave the process over the
//! API", and it is enforced in one place (`ApiError::into_response`).

use std::sync::{OnceLock, RwLock};

use crate::config::OrcaConfig;

/// What a scrubbed secret is replaced with.
pub const REDACTED: &str = "***";

/// Blank the credential part of every URL in `text`.
///
/// `https://user:ghp_token@github.com/o/r.git` → `https://***@github.com/o/r.git`
///
/// Only the `scheme://userinfo@host` form is touched. SCP-style git remotes
/// (`git@github.com:o/r.git`) carry a username, not a secret, and are left
/// alone. A `@` that appears later in a path is not userinfo and is left alone
/// because scanning stops at the first `/`, `?`, or `#`.
pub fn redact_url_credentials(text: &str) -> String {
    // Byte ranges of userinfo sections, collected against the *original*
    // string so that rewriting one URL cannot shift the offsets of the next.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let bytes = text.as_bytes();

    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find("://") {
        let authority_start = search_from + rel + 3;

        // Walk to the end of the authority component. Every byte in this set is
        // ASCII, and UTF-8 continuation bytes are all >= 0x80, so `end` only
        // ever lands on a `char` boundary.
        let mut end = authority_start;
        while end < bytes.len() {
            let stop = matches!(
                bytes[end],
                b'/' | b'?'
                    | b'#'
                    | b' '
                    | b'\t'
                    | b'\n'
                    | b'\r'
                    | b'"'
                    | b'\''
                    | b'<'
                    | b'>'
                    | b'\\'
                    | b'`'
                    | b'|'
                    | b'('
                    | b')'
                    | b','
                    | b';'
            );
            if stop {
                break;
            }
            end += 1;
        }

        // `rfind` rather than `find`: a password may itself contain `@`, and the
        // host separator is always the last one.
        if let Some(at) = text[authority_start..end].rfind('@') {
            spans.push((authority_start, authority_start + at));
        }

        if end >= bytes.len() {
            break;
        }
        // `end` points at a delimiter; step past it so the next search cannot
        // rediscover the same "://".
        search_from = end + 1;
    }

    if spans.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, stop) in spans {
        out.push_str(&text[cursor..start]);
        out.push_str(REDACTED);
        cursor = stop;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Exact secret values the daemon knows about.
#[derive(Debug, Default, Clone)]
pub struct SecretRegistry {
    secrets: Vec<String>,
}

impl SecretRegistry {
    /// Shortest value that will be replaced wholesale.
    ///
    /// Below this length, replacing every occurrence rewrites ordinary prose —
    /// a four-character password would blank the word "Orca" in every message
    /// the daemon produced, which is worse than the leak it prevents because it
    /// destroys diagnosability. Short secrets are still covered when they appear
    /// as URL userinfo, which is where they realistically surface in error text.
    pub const MIN_VALUE_LEN: usize = 8;

    pub fn new() -> Self {
        Self::default()
    }

    /// Register a secret value. Returns whether it was added: empty, too-short,
    /// and duplicate values are ignored.
    pub fn add(&mut self, secret: &str) -> bool {
        let secret = secret.trim();
        if secret.len() < Self::MIN_VALUE_LEN {
            return false;
        }
        if self.secrets.iter().any(|existing| existing == secret) {
            return false;
        }
        self.secrets.push(secret.to_string());
        true
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    /// Blank every registered value, then scrub URL userinfo.
    ///
    /// Values are applied longest-first: a secret that contains a shorter
    /// registered secret as a substring would otherwise be half-replaced,
    /// leaving the remainder on the wire.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        let mut ordered: Vec<&String> = self.secrets.iter().collect();
        ordered.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        for secret in ordered {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), REDACTED);
            }
        }
        redact_url_credentials(&out)
    }

    /// Every secret Orca stores in its own configuration.
    pub fn from_config(config: &OrcaConfig) -> Self {
        let mut registry = Self::new();
        if let Some(token) = &config.api_token {
            registry.add(token);
        }
        if let Some(key) = &config.anthropic_api_key {
            registry.add(key);
        }
        if let Some(key) = &config.openai_api_key {
            registry.add(key);
        }
        if let Some(secret) = &config.webhook_secret {
            registry.add(secret);
        }
        for host in &config.remote_hosts {
            registry.add(&host.token);
        }
        for credential in &config.registries {
            // The *decoded* password is the value that could appear in text.
            // The stored base64 is a different string and would never match.
            if let Ok(password) = credential.password() {
                registry.add(&password);
            }
        }
        registry
    }
}

static EGRESS_REGISTRY: OnceLock<RwLock<SecretRegistry>> = OnceLock::new();

fn registry() -> &'static RwLock<SecretRegistry> {
    EGRESS_REGISTRY.get_or_init(|| RwLock::new(SecretRegistry::new()))
}

/// Rebuild the process-wide registry from `config`.
///
/// Called from `OrcaConfig::save`, so a credential the user has just added is
/// covered from the moment it is persisted, and no individual call site has to
/// remember to refresh. A lock poisoned by a panicking thread is recovered
/// rather than propagated: refusing to redact is the unsafe choice.
pub fn refresh_from_config(config: &OrcaConfig) {
    let fresh = SecretRegistry::from_config(config);
    let mut guard = match registry().write() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("secret registry lock poisoned; recovering");
            poisoned.into_inner()
        }
    };
    *guard = fresh;
}

/// Number of registered secrets. For startup logging and tests.
pub fn registered_secret_count() -> usize {
    match registry().read() {
        Ok(guard) => guard.len(),
        Err(poisoned) => poisoned.into_inner().len(),
    }
}

/// Redact text that is about to leave the daemon over the API.
///
/// Note the failure direction: if the registry cannot be read, the URL scrubber
/// still runs. There is no path that returns `text` unchanged just because
/// bookkeeping failed.
pub fn redact_for_egress(text: &str) -> String {
    match registry().read() {
        Ok(guard) => guard.redact(text),
        Err(poisoned) => poisoned.into_inner().redact(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_userinfo_is_blanked() {
        assert_eq!(
            redact_url_credentials("https://user:ghp_secret@github.com/o/r.git"),
            "https://***@github.com/o/r.git"
        );
    }

    #[test]
    fn token_only_userinfo_is_blanked() {
        assert_eq!(
            redact_url_credentials("fatal: could not read from https://ghp_abc123@github.com/o/r"),
            "fatal: could not read from https://***@github.com/o/r"
        );
    }

    #[test]
    fn password_containing_at_sign_keeps_only_the_host_separator() {
        // The `@` inside the password must not be mistaken for the separator.
        assert_eq!(
            redact_url_credentials("https://user:p@ssw0rd@example.com:5000/v2/"),
            "https://***@example.com:5000/v2/"
        );
    }

    #[test]
    fn path_at_signs_are_not_userinfo() {
        // Scanning stops at the first `/`, so the `@` in the path is left alone.
        assert_eq!(
            redact_url_credentials("https://example.com/a@b/c"),
            "https://example.com/a@b/c"
        );
    }

    #[test]
    fn ordinary_urls_are_untouched() {
        for url in [
            "https://github.com/o/r.git",
            "http://localhost:9477/api/v1/health",
            "registry:5000/foo/bar",
        ] {
            assert_eq!(redact_url_credentials(url), url);
        }
    }

    #[test]
    fn scp_style_remotes_are_left_alone() {
        // A username is not a secret, and rewriting it would corrupt the hint.
        let text = "git@github.com:o/r.git";
        assert_eq!(redact_url_credentials(text), text);
    }

    #[test]
    fn every_url_in_a_multi_url_message_is_blanked() {
        let text = "tried https://a:1@x.com and https://b:2@y.com";
        assert_eq!(redact_url_credentials(text), "tried https://***@x.com and https://***@y.com");
    }

    #[test]
    fn adjacent_urls_do_not_swallow_each_other() {
        // The delimiter step must let the second URL be found.
        assert_eq!(
            redact_url_credentials("https://u1:p1@h1,https://u2:p2@h2"),
            "https://***@h1,https://***@h2"
        );
    }

    #[test]
    fn multiline_git_stderr_is_handled() {
        let stderr = "Cloning into 'x'...\nfatal: Authentication failed for 'https://u:tok@github.com/o/r.git/'\n";
        let out = redact_url_credentials(stderr);
        assert!(!out.contains("tok"), "token survived: {out}");
        assert!(out.contains("github.com/o/r.git"), "host was lost: {out}");
    }

    #[test]
    fn registry_replaces_known_values() {
        let mut registry = SecretRegistry::new();
        assert!(registry.add("sk-ant-super-secret-value"));
        let out = registry.redact("provider rejected key sk-ant-super-secret-value (401)");
        assert_eq!(out, "provider rejected key *** (401)");
    }

    #[test]
    fn short_values_are_refused_rather_than_corrupting_prose() {
        let mut registry = SecretRegistry::new();
        assert!(!registry.add("orca"), "a 4-char value is below the threshold");
        assert!(registry.is_empty());
        let mut registry = SecretRegistry::new();
        assert!(!registry.add(""), "the empty string is not a secret");
        assert!(!registry.add("   "), "whitespace is not a secret");
    }

    #[test]
    fn duplicates_are_registered_once() {
        let mut registry = SecretRegistry::new();
        assert!(registry.add("duplicate-secret-value"));
        assert!(!registry.add("duplicate-secret-value"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn longer_values_win_over_their_own_substrings() {
        let mut registry = SecretRegistry::new();
        registry.add("abcdefgh");          // 8 chars, registered
        registry.add("abcdefgh-extended"); // contains the above
        assert_eq!(registry.redact("abcdefgh-extended"), REDACTED);
    }

    #[test]
    fn values_are_trimmed_before_matching() {
        let mut registry = SecretRegistry::new();
        registry.add("  padded-secret-value  ");
        // The trimmed form is what a client would send on the wire.
        assert_eq!(registry.redact("got padded-secret-value"), "got ***");
    }

    #[test]
    fn value_and_url_scrubbing_compose() {
        let mut registry = SecretRegistry::new();
        registry.add("sk-live-abcdefgh");
        let out = registry.redact("key sk-live-abcdefgh for https://u:p@h.com");
        assert_eq!(out, "key *** for https://***@h.com");
    }

    #[test]
    fn egress_helper_redacts_urls_even_with_an_empty_registry() {
        // No config has been loaded in this test process, so this asserts the
        // shape-based layer is not conditional on the registry being populated.
        let out = redact_for_egress("clone failed: https://u:tok3n@github.com/o/r.git");
        assert!(!out.contains("tok3n"), "token survived: {out}");
    }
}
