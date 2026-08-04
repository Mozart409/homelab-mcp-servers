//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr};

/// Connection settings for a Prometheus instance.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL including scheme and port, e.g. `http://prometheus.lan:9090`.
    pub base_url: String,
    /// Optional bearer token, sent as `Authorization: Bearer <token>` — set this
    /// when Prometheus sits behind an authenticating reverse proxy. `None` sends
    /// no auth header (the common case for an unauthenticated internal instance).
    pub token: Option<String>,
    /// Accept self-signed / invalid TLS certificates.
    pub insecure: bool,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8082`).
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
}

impl Config {
    /// Build a [`Config`] from the `PROM_*` environment variables.
    ///
    /// Required: `PROM_HOST`.
    /// Optional: `PROM_TOKEN`, `PROM_INSECURE` (`1`/`true`), `PROM_BIND`
    /// (default `127.0.0.1:8082`), `PROM_ALLOWED_HOSTS` (comma-separated).
    ///
    /// # Errors
    ///
    /// Returns an error if `PROM_HOST` is unset.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("PROM_HOST").wrap_err(
            "PROM_HOST must be set (e.g. http://prometheus.lan:9090 or prometheus.lan)",
        )?;
        let token = std::env::var("PROM_TOKEN").ok().filter(|s| !s.is_empty());
        let insecure = matches!(
            std::env::var("PROM_INSECURE").as_deref(),
            Ok("1" | "true" | "yes")
        );
        let bind = std::env::var("PROM_BIND").unwrap_or_else(|_| "127.0.0.1:8082".to_string());
        let allowed_hosts =
            parse_allowed_hosts(std::env::var("PROM_ALLOWED_HOSTS").ok().as_deref());

        Ok(Self {
            base_url: normalize_base_url(&host),
            token,
            insecure,
            bind,
            allowed_hosts,
        })
    }
}

/// Turn a user-supplied host into a full base URL, defaulting scheme to `http`
/// and port to Prometheus's `9090` when not already specified.
fn normalize_base_url(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else if h.contains(':') {
        format!("http://{h}")
    } else {
        format!("http://{h}:9090")
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
        "PROM_HOST",
        "PROM_TOKEN",
        "PROM_INSECURE",
        "PROM_BIND",
        "PROM_ALLOWED_HOSTS",
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
        set_env("PROM_HOST", "prometheus.lan");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "http://prometheus.lan:9090");
        assert_eq!(cfg.bind, "127.0.0.1:8082");
        assert!(cfg.token.is_none());
        assert!(!cfg.insecure);
        assert!(cfg.allowed_hosts.is_none());

        clear_env();
    }

    #[test]
    fn from_env_reads_every_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PROM_HOST", "https://prometheus.example.com/");
        set_env("PROM_TOKEN", "secret");
        set_env("PROM_INSECURE", "true");
        set_env("PROM_BIND", "0.0.0.0:9999");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://prometheus.example.com");
        assert_eq!(cfg.token.as_deref(), Some("secret"));
        assert!(cfg.insecure);
        assert_eq!(cfg.bind, "0.0.0.0:9999");

        clear_env();
    }

    #[test]
    fn from_env_treats_empty_token_as_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PROM_HOST", "prometheus.lan");
        set_env("PROM_TOKEN", "");

        assert!(Config::from_env().unwrap().token.is_none());

        clear_env();
    }

    #[test]
    fn insecure_is_true_only_for_1_true_yes() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PROM_HOST", "prometheus.lan");

        for truthy in ["1", "true", "yes"] {
            set_env("PROM_INSECURE", truthy);
            assert!(
                Config::from_env().unwrap().insecure,
                "{truthy} should enable insecure"
            );
        }
        // Case-sensitive and strictly literal: nothing else counts.
        for falsy in ["0", "false", "no", "", "TRUE", "Yes", "on", "2"] {
            set_env("PROM_INSECURE", falsy);
            assert!(
                !Config::from_env().unwrap().insecure,
                "{falsy:?} should not enable insecure"
            );
        }

        clear_env();
    }

    #[test]
    fn allowed_hosts_splits_trims_and_drops_empty_items() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PROM_HOST", "prometheus.lan");
        set_env("PROM_ALLOWED_HOSTS", " a.example.com , localhost ,, ");

        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.allowed_hosts.unwrap(),
            vec!["a.example.com".to_string(), "localhost".to_string()]
        );

        // Regression: a value that trims to nothing collapses to `None`, not
        // `Some(vec![])`. An empty allow-list reaches rmcp's
        // `with_allowed_hosts` and rejects *every* inbound Host header, so a
        // typo'd or empty-template value used to yield a server that silently
        // accepted no connections at all.
        for blank in [" , ", "", ",", "   ", ",,,"] {
            set_env("PROM_ALLOWED_HOSTS", blank);
            assert_eq!(
                Config::from_env().unwrap().allowed_hosts,
                None,
                "{blank:?} should fall back to the loopback default"
            );
        }
        clear_env();
    }

    #[test]
    fn normalize_base_url_fills_in_scheme_and_port() {
        assert_eq!(
            normalize_base_url("prometheus.lan"),
            "http://prometheus.lan:9090"
        );
        assert_eq!(
            normalize_base_url("prometheus.lan:9091"),
            "http://prometheus.lan:9091"
        );
        assert_eq!(
            normalize_base_url("http://prometheus.lan/"),
            "http://prometheus.lan"
        );
        assert_eq!(
            normalize_base_url(" https://prometheus.lan "),
            "https://prometheus.lan"
        );
    }
}
