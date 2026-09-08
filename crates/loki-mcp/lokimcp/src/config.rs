//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr};

/// Connection settings for a Grafana Loki instance.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL including scheme and port, e.g. `http://loki.lan:3100`.
    pub base_url: String,
    /// Optional bearer token, sent as `Authorization: Bearer <token>` — set this
    /// when Loki sits behind an authenticating reverse proxy. `None` sends no
    /// auth header.
    pub token: Option<String>,
    /// Optional tenant ID, sent as the `X-Scope-OrgID` header. Required when Loki
    /// runs in multi-tenant mode; `None` omits the header (single-tenant default).
    pub org_id: Option<String>,
    /// Accept self-signed / invalid TLS certificates.
    pub insecure: bool,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8083`).
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
}

impl Config {
    /// Build a [`Config`] from the `LOKI_*` environment variables.
    ///
    /// Required: `LOKI_HOST`.
    /// Optional: `LOKI_TOKEN`, `LOKI_ORG_ID`, `LOKI_INSECURE` (`1`/`true`),
    /// `LOKI_BIND` (default `127.0.0.1:8083`), `LOKI_ALLOWED_HOSTS`
    /// (comma-separated).
    ///
    /// # Errors
    ///
    /// Returns an error if `LOKI_HOST` is unset.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("LOKI_HOST")
            .wrap_err("LOKI_HOST must be set (e.g. http://loki.lan:3100 or loki.lan)")?;
        let token = std::env::var("LOKI_TOKEN").ok().filter(|s| !s.is_empty());
        let org_id = std::env::var("LOKI_ORG_ID").ok().filter(|s| !s.is_empty());
        let insecure = matches!(
            std::env::var("LOKI_INSECURE").as_deref(),
            Ok("1" | "true" | "yes")
        );
        let bind = std::env::var("LOKI_BIND").unwrap_or_else(|_| "127.0.0.1:8083".to_string());
        let allowed_hosts =
            mcp_common::parse_allowed_hosts(std::env::var("LOKI_ALLOWED_HOSTS").ok().as_deref());

        Ok(Self {
            base_url: mcp_common::normalize_base_url(&host, 3100),
            token,
            org_id,
            insecure,
            bind,
            allowed_hosts,
        })
    }
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
        "LOKI_HOST",
        "LOKI_TOKEN",
        "LOKI_ORG_ID",
        "LOKI_INSECURE",
        "LOKI_BIND",
        "LOKI_ALLOWED_HOSTS",
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
        set_env("LOKI_HOST", "loki.lan");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "http://loki.lan:3100");
        assert_eq!(cfg.bind, "127.0.0.1:8083");
        assert!(cfg.token.is_none());
        assert!(cfg.org_id.is_none());
        assert!(!cfg.insecure);
        assert!(cfg.allowed_hosts.is_none());

        clear_env();
    }

    #[test]
    fn from_env_reads_every_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("LOKI_HOST", "https://loki.example.com/");
        set_env("LOKI_TOKEN", "secret");
        set_env("LOKI_ORG_ID", "tenant-7");
        set_env("LOKI_INSECURE", "true");
        set_env("LOKI_BIND", "0.0.0.0:9999");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://loki.example.com");
        assert_eq!(cfg.token.as_deref(), Some("secret"));
        assert_eq!(cfg.org_id.as_deref(), Some("tenant-7"));
        assert!(cfg.insecure);
        assert_eq!(cfg.bind, "0.0.0.0:9999");

        clear_env();
    }

    #[test]
    fn from_env_treats_empty_token_and_org_id_as_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("LOKI_HOST", "loki.lan");
        set_env("LOKI_TOKEN", "");
        set_env("LOKI_ORG_ID", "");

        let cfg = Config::from_env().unwrap();
        assert!(cfg.token.is_none());
        assert!(cfg.org_id.is_none());

        clear_env();
    }

    #[test]
    fn insecure_is_true_only_for_1_true_yes() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("LOKI_HOST", "loki.lan");

        for truthy in ["1", "true", "yes"] {
            set_env("LOKI_INSECURE", truthy);
            assert!(
                Config::from_env().unwrap().insecure,
                "{truthy} should enable insecure"
            );
        }
        // Case-sensitive and strictly literal: nothing else counts.
        for falsy in ["0", "false", "no", "", "TRUE", "Yes", "on", "2"] {
            set_env("LOKI_INSECURE", falsy);
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
        set_env("LOKI_HOST", "loki.lan");
        set_env("LOKI_ALLOWED_HOSTS", " a.example.com , localhost ,, ");

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
            set_env("LOKI_ALLOWED_HOSTS", blank);
            assert_eq!(
                Config::from_env().unwrap().allowed_hosts,
                None,
                "{blank:?} should fall back to the loopback default"
            );
        }

        clear_env();
    }
}
