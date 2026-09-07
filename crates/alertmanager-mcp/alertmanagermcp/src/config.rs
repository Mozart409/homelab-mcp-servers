//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr};

/// Connection settings for an Alertmanager instance.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL including scheme and port, e.g. `http://alertmanager.lan:9093`.
    pub base_url: String,
    /// Optional bearer token, sent as `Authorization: Bearer <token>` — set this
    /// when Alertmanager sits behind an authenticating reverse proxy. `None`
    /// sends no auth header (the common case for an internal instance).
    pub token: Option<String>,
    /// Accept self-signed / invalid TLS certificates.
    pub insecure: bool,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8086`).
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
    /// Register the two silence-write tools (`create_silence`, `expire_silence`).
    ///
    /// Defaults to `false`, which is the whole point of the flag. Every other
    /// server in this workspace is read-only, and `homeassistant-mcp` is the one
    /// deliberate exception (see AGENTS.md Hard rules §1). This server is the
    /// second, and it earns that only by making the mutating surface opt-in: a
    /// silence is homelab-wide alert blindness, so enabling it should be a
    /// deliberate deployment act rather than a side effect of pulling an image.
    ///
    /// When `false` the write tools are not merged into the router at all, so
    /// they never appear in `tools/list`. A tool a client can see is a tool the
    /// model will try, and discovering the gate by calling into an error is a
    /// worse experience than never being offered the capability.
    pub allow_silence: bool,
}

impl Config {
    /// Build a [`Config`] from the `ALERTMANAGER_*` environment variables.
    ///
    /// Required: `ALERTMANAGER_HOST`.
    /// Optional: `ALERTMANAGER_TOKEN`, `ALERTMANAGER_INSECURE` (`1`/`true`/`yes`),
    /// `ALERTMANAGER_BIND` (default `127.0.0.1:8086`),
    /// `ALERTMANAGER_ALLOWED_HOSTS` (comma-separated),
    /// `ALERTMANAGER_ALLOW_SILENCE` (`1`/`true`/`yes`).
    ///
    /// # Errors
    ///
    /// Returns an error if `ALERTMANAGER_HOST` is unset.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("ALERTMANAGER_HOST").wrap_err(
            "ALERTMANAGER_HOST must be set (e.g. http://alertmanager.lan:9093 or alertmanager.lan)",
        )?;
        let token = std::env::var("ALERTMANAGER_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        let insecure = is_truthy(std::env::var("ALERTMANAGER_INSECURE").as_deref().ok());
        let bind =
            std::env::var("ALERTMANAGER_BIND").unwrap_or_else(|_| "127.0.0.1:8086".to_string());
        let allowed_hosts =
            parse_allowed_hosts(std::env::var("ALERTMANAGER_ALLOWED_HOSTS").ok().as_deref());
        let allow_silence = is_truthy(std::env::var("ALERTMANAGER_ALLOW_SILENCE").as_deref().ok());

        Ok(Self {
            base_url: normalize_base_url(&host),
            token,
            insecure,
            bind,
            allowed_hosts,
            allow_silence,
        })
    }
}

/// Interpret a boolean-ish env var. Anything other than the affirmative spellings
/// — including unset, empty, and `"false"` — reads as `false`, so a flag that
/// grants write access is never enabled by a typo.
fn is_truthy(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim),
        Some("1" | "true" | "TRUE" | "True" | "yes" | "YES" | "Yes")
    )
}

/// Turn a user-supplied host into a full base URL, defaulting scheme to `http`
/// and port to Alertmanager's `9093` when not already specified.
fn normalize_base_url(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else if h.contains(':') {
        format!("http://{h}")
    } else {
        format!("http://{h}:9093")
    }
}

/// Parse a comma-separated allow-list, treating "set but empty" as unset.
///
/// Returning `Some(vec![])` here would be actively dangerous: `run()` passes it
/// to rmcp's `with_allowed_hosts`, and an empty allow-list rejects *every*
/// inbound `Host` header. A value like `" , "` — a typo, or a template that
/// expanded to nothing — would therefore produce a server that silently accepts
/// no connections at all. Collapsing that to `None` falls back to rmcp's
/// loopback-only default instead, which is the safe reading of "unset".
fn parse_allowed_hosts(raw: Option<&str>) -> Option<Vec<String>> {
    let hosts: Vec<String> = raw?
        .split(',')
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect();

    if hosts.is_empty() { None } else { Some(hosts) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, PoisonError};

    // `from_env` reads process-global state, so serialize the tests that mutate
    // it. `unwrap_or_else(into_inner)` keeps a panicking test from poisoning the
    // lock and cascading failures into the others.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const KEYS: &[&str] = &[
        "ALERTMANAGER_HOST",
        "ALERTMANAGER_TOKEN",
        "ALERTMANAGER_INSECURE",
        "ALERTMANAGER_BIND",
        "ALERTMANAGER_ALLOWED_HOSTS",
        "ALERTMANAGER_ALLOW_SILENCE",
    ];

    // SAFETY: all env mutation is confined to tests holding `ENV_LOCK`, so no
    // other thread reads or writes the environment concurrently.
    fn clear_env() {
        for k in KEYS {
            unsafe { std::env::remove_var(k) };
        }
    }
    fn set_env(k: &str, v: &str) {
        unsafe { std::env::set_var(k, v) };
    }

    #[test]
    fn from_env_requires_host() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        assert!(Config::from_env().is_err());
        clear_env();
    }

    #[test]
    fn from_env_applies_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("ALERTMANAGER_HOST", "alertmanager.lan");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "http://alertmanager.lan:9093");
        assert_eq!(cfg.bind, "127.0.0.1:8086");
        assert!(cfg.token.is_none());
        assert!(!cfg.insecure);
        assert!(cfg.allowed_hosts.is_none());
        // The write gate must be off unless explicitly turned on.
        assert!(!cfg.allow_silence);

        clear_env();
    }

    #[test]
    fn allow_silence_defaults_off_and_ignores_non_affirmative_values() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("ALERTMANAGER_HOST", "alertmanager.lan");

        for value in ["", "false", "0", "no", "off", "maybe"] {
            set_env("ALERTMANAGER_ALLOW_SILENCE", value);
            assert!(
                !Config::from_env().unwrap().allow_silence,
                "{value:?} must not enable silence writes"
            );
        }

        clear_env();
    }

    #[test]
    fn allow_silence_accepts_affirmative_spellings() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("ALERTMANAGER_HOST", "alertmanager.lan");

        for value in ["1", "true", "TRUE", "yes"] {
            set_env("ALERTMANAGER_ALLOW_SILENCE", value);
            assert!(
                Config::from_env().unwrap().allow_silence,
                "{value:?} must enable silence writes"
            );
        }

        clear_env();
    }

    #[test]
    fn normalize_base_url_variants() {
        assert_eq!(
            normalize_base_url("alertmanager.lan"),
            "http://alertmanager.lan:9093"
        );
        assert_eq!(
            normalize_base_url("alertmanager.lan:9999"),
            "http://alertmanager.lan:9999"
        );
        assert_eq!(
            normalize_base_url("https://am.example.com/"),
            "https://am.example.com"
        );
    }

    #[test]
    fn parse_allowed_hosts_collapses_empty_to_none() {
        assert!(parse_allowed_hosts(None).is_none());
        assert!(parse_allowed_hosts(Some(" , ")).is_none());
        assert_eq!(
            parse_allowed_hosts(Some("a.lan, b.lan")),
            Some(vec!["a.lan".to_string(), "b.lan".to_string()])
        );
    }
}
