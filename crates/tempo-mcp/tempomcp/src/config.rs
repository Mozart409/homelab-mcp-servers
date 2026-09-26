//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr};

/// Connection settings for a Grafana Tempo instance.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL including scheme and port, e.g. `http://tempo.lan:3200`.
    pub base_url: String,
    /// Optional bearer token, sent as `Authorization: Bearer <token>` — set this
    /// when Tempo sits behind an authenticating reverse proxy (the homelab's
    /// Caddy in front of `tempo.homelab.local`). `None` sends no auth header.
    pub token: Option<String>,
    /// Optional tenant ID, sent as the `X-Scope-OrgID` header. Required when
    /// Tempo runs with `multitenancy_enabled`; `None` omits the header
    /// (single-tenant default). Without it a multi-tenant Tempo searches the
    /// wrong tenant and answers "no traces", which reads as a healthy system
    /// rather than a misconfiguration.
    pub org_id: Option<String>,
    /// Accept self-signed / invalid TLS certificates.
    pub insecure: bool,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8092`).
    ///
    /// `8092`, not the next free port after alertmanager's `8086`: the homelab
    /// deployment (`pve-nixos-homelab`) had already allocated it to this server
    /// before the crate existed, and changing it means changing that repo too.
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
}

impl Config {
    /// Build a [`Config`] from the `TEMPO_*` environment variables.
    ///
    /// Required: `TEMPO_HOST`.
    /// Optional: `TEMPO_TOKEN`, `TEMPO_ORG_ID`, `TEMPO_INSECURE` (`1`/`true`),
    /// `TEMPO_BIND` (default `127.0.0.1:8092`), `TEMPO_ALLOWED_HOSTS`
    /// (comma-separated).
    ///
    /// # Errors
    ///
    /// Returns an error if `TEMPO_HOST` is unset.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("TEMPO_HOST")
            .wrap_err("TEMPO_HOST must be set (e.g. http://tempo.lan:3200 or tempo.lan)")?;
        let token = std::env::var("TEMPO_TOKEN").ok().filter(|s| !s.is_empty());
        let org_id = std::env::var("TEMPO_ORG_ID").ok().filter(|s| !s.is_empty());
        let insecure = matches!(
            std::env::var("TEMPO_INSECURE").as_deref(),
            Ok("1" | "true" | "yes")
        );
        let bind = std::env::var("TEMPO_BIND").unwrap_or_else(|_| "127.0.0.1:8092".to_string());
        let allowed_hosts =
            mcp_common::parse_allowed_hosts(std::env::var("TEMPO_ALLOWED_HOSTS").ok().as_deref());

        Ok(Self {
            base_url: mcp_common::normalize_base_url(&host, 3200),
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
        "TEMPO_HOST",
        "TEMPO_TOKEN",
        "TEMPO_ORG_ID",
        "TEMPO_INSECURE",
        "TEMPO_BIND",
        "TEMPO_ALLOWED_HOSTS",
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
        set_env("TEMPO_HOST", "tempo.lan");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "http://tempo.lan:3200");
        assert_eq!(cfg.bind, "127.0.0.1:8092");
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
        set_env("TEMPO_HOST", "https://tempo.homelab.local/");
        set_env("TEMPO_TOKEN", "secret");
        set_env("TEMPO_ORG_ID", "tenant-7");
        set_env("TEMPO_INSECURE", "true");
        set_env("TEMPO_BIND", "0.0.0.0:9999");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://tempo.homelab.local");
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
        set_env("TEMPO_HOST", "tempo.lan");
        set_env("TEMPO_TOKEN", "");
        set_env("TEMPO_ORG_ID", "");

        let cfg = Config::from_env().unwrap();
        assert!(cfg.token.is_none());
        assert!(cfg.org_id.is_none());

        clear_env();
    }

    #[test]
    fn insecure_is_true_only_for_1_true_yes() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("TEMPO_HOST", "tempo.lan");

        for truthy in ["1", "true", "yes"] {
            set_env("TEMPO_INSECURE", truthy);
            assert!(
                Config::from_env().unwrap().insecure,
                "{truthy} should enable insecure"
            );
        }
        // Case-sensitive and strictly literal: nothing else counts.
        for falsy in ["0", "false", "no", "", "TRUE", "Yes", "on", "2"] {
            set_env("TEMPO_INSECURE", falsy);
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
        set_env("TEMPO_HOST", "tempo.lan");
        set_env("TEMPO_ALLOWED_HOSTS", " a.example.com , localhost ,, ");

        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.allowed_hosts.unwrap(),
            vec!["a.example.com".to_string(), "localhost".to_string()]
        );

        // A value that trims to nothing collapses to `None`, not `Some(vec![])`:
        // an empty allow-list would reach rmcp as "accept any Host".
        for blank in [" , ", "", ",", "   ", ",,,"] {
            set_env("TEMPO_ALLOWED_HOSTS", blank);
            assert_eq!(
                Config::from_env().unwrap().allowed_hosts,
                None,
                "{blank:?} should fall back to the loopback default"
            );
        }

        clear_env();
    }
}
