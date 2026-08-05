//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr};

/// Connection settings for a Woodpecker CI instance.
///
/// Env prefix is `WP_` — deliberately NOT `WOODPECKER_`, because Woodpecker CI
/// injects `WOODPECKER_*` variables into every pipeline step and this repo
/// builds under Woodpecker CI, so `WOODPECKER_TOKEN` would collide with the
/// pipeline's injected token during `just ci`.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL including scheme and optional port, e.g. `https://ci.homelab.local`
    /// or `https://ci.homelab.local:8000`.
    pub base_url: String,
    /// Personal access token for the Woodpecker API, sent as `Authorization: Bearer <token>`.
    /// Required and must be non-empty — every endpoint requires it.
    pub token: String,
    /// Accept self-signed / invalid TLS certificates.
    pub insecure: bool,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8085`).
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
    /// Maximum number of log lines to return from `step_logs` (default 500).
    /// A parsed value of 0 falls back to the default 500 to avoid a footgun
    /// of returning unlimited logs by mistake.
    pub max_log_lines: usize,
}

impl Config {
    /// Build a [`Config`] from the `WP_*` environment variables.
    ///
    /// Required: `WP_HOST`, `WP_TOKEN`.
    /// Optional: `WP_INSECURE` (`1`/`true`/`yes`), `WP_BIND` (default `127.0.0.1:8085`),
    /// `WP_ALLOWED_HOSTS` (comma-separated), `WP_MAX_LOG_LINES` (default 500).
    ///
    /// # Errors
    ///
    /// Returns an error if `WP_HOST` is unset, or if `WP_TOKEN` is unset or empty.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("WP_HOST")
            .wrap_err("WP_HOST must be set (e.g. https://ci.homelab.local or ci.homelab.local)")?;
        let token = std::env::var("WP_TOKEN")
            .wrap_err("WP_TOKEN must be set (personal access token from Woodpecker CI)")?;

        if token.is_empty() {
            return Err(color_eyre::eyre::eyre!(
                "WP_TOKEN must be non-empty (personal access token from Woodpecker CI)"
            ));
        }

        let insecure = matches!(
            std::env::var("WP_INSECURE").as_deref(),
            Ok("1" | "true" | "yes")
        );
        let bind = std::env::var("WP_BIND").unwrap_or_else(|_| "127.0.0.1:8085".to_string());
        let allowed_hosts = parse_allowed_hosts(std::env::var("WP_ALLOWED_HOSTS").ok().as_deref());
        let max_log_lines = std::env::var("WP_MAX_LOG_LINES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n != 0)
            .unwrap_or(500);

        Ok(Self {
            base_url: normalize_base_url(&host),
            token,
            insecure,
            bind,
            allowed_hosts,
            max_log_lines,
        })
    }
}

/// Turn a user-supplied host into a full base URL, defaulting scheme to `https`
/// and NOT appending a default port (Woodpecker is normally behind TLS on port 443,
/// and custom ports are explicitly provided; there is no canonical non-443 Woodpecker port).
fn normalize_base_url(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else {
        format!("https://{h}")
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
        "WP_HOST",
        "WP_TOKEN",
        "WP_INSECURE",
        "WP_BIND",
        "WP_ALLOWED_HOSTS",
        "WP_MAX_LOG_LINES",
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
    fn from_env_requires_token() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("WP_HOST", "ci.homelab.local");
        assert!(Config::from_env().is_err());
        clear_env();
    }

    #[test]
    fn from_env_rejects_empty_token() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("WP_HOST", "ci.homelab.local");
        set_env("WP_TOKEN", "");
        assert!(Config::from_env().is_err());
        clear_env();
    }

    #[test]
    fn from_env_applies_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("WP_HOST", "ci.homelab.local");
        set_env("WP_TOKEN", "test-token");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://ci.homelab.local");
        assert_eq!(cfg.token, "test-token");
        assert_eq!(cfg.bind, "127.0.0.1:8085");
        assert!(!cfg.insecure);
        assert!(cfg.allowed_hosts.is_none());
        assert_eq!(cfg.max_log_lines, 500);

        clear_env();
    }

    #[test]
    fn from_env_reads_every_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("WP_HOST", "https://ci.example.com:8000/");
        set_env("WP_TOKEN", "secret-token-123");
        set_env("WP_INSECURE", "true");
        set_env("WP_BIND", "0.0.0.0:9999");
        set_env("WP_MAX_LOG_LINES", "1000");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://ci.example.com:8000");
        assert_eq!(cfg.token, "secret-token-123");
        assert!(cfg.insecure);
        assert_eq!(cfg.bind, "0.0.0.0:9999");
        assert_eq!(cfg.max_log_lines, 1000);

        clear_env();
    }

    #[test]
    fn insecure_is_true_only_for_1_true_yes() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("WP_HOST", "ci.homelab.local");
        set_env("WP_TOKEN", "token");

        for truthy in ["1", "true", "yes"] {
            set_env("WP_INSECURE", truthy);
            assert!(
                Config::from_env().unwrap().insecure,
                "{truthy} should enable insecure"
            );
        }
        // Case-sensitive and strictly literal: nothing else counts.
        for falsy in ["0", "false", "no", "", "TRUE", "Yes", "on", "2"] {
            set_env("WP_INSECURE", falsy);
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
        set_env("WP_HOST", "ci.homelab.local");
        set_env("WP_TOKEN", "token");
        set_env("WP_ALLOWED_HOSTS", " ci.homelab.local , localhost ,, ");

        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.allowed_hosts.unwrap(),
            vec!["ci.homelab.local".to_string(), "localhost".to_string()]
        );

        // Regression: a value that trims to nothing collapses to `None`, not
        // `Some(vec![])`. An empty allow-list reaches rmcp's
        // `with_allowed_hosts` and rejects *every* inbound Host header, so a
        // typo'd or empty-template value used to yield a server that silently
        // accepted no connections at all.
        for blank in [" , ", "", ",", "   ", ",,,"] {
            set_env("WP_ALLOWED_HOSTS", blank);
            assert_eq!(
                Config::from_env().unwrap().allowed_hosts,
                None,
                "{blank:?} should fall back to the loopback default"
            );
        }
        clear_env();
    }

    #[test]
    fn max_log_lines_defaults_to_500() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("WP_HOST", "ci.homelab.local");
        set_env("WP_TOKEN", "token");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.max_log_lines, 500);

        clear_env();
    }

    #[test]
    fn max_log_lines_zero_falls_back_to_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("WP_HOST", "ci.homelab.local");
        set_env("WP_TOKEN", "token");
        set_env("WP_MAX_LOG_LINES", "0");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.max_log_lines, 500);

        clear_env();
    }

    #[test]
    fn max_log_lines_parses_valid_values() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("WP_HOST", "ci.homelab.local");
        set_env("WP_TOKEN", "token");

        for lines in [1, 100, 500, 1000, 5000] {
            set_env("WP_MAX_LOG_LINES", &lines.to_string());
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.max_log_lines, lines);
        }

        clear_env();
    }

    #[test]
    fn normalize_base_url_defaults_to_https() {
        assert_eq!(
            normalize_base_url("ci.homelab.local"),
            "https://ci.homelab.local"
        );
        assert_eq!(
            normalize_base_url("ci.homelab.local:8000"),
            "https://ci.homelab.local:8000"
        );
    }

    #[test]
    fn normalize_base_url_preserves_explicit_scheme() {
        assert_eq!(normalize_base_url("http://ci.lan/"), "http://ci.lan");
        assert_eq!(
            normalize_base_url("https://ci.example.com"),
            "https://ci.example.com"
        );
    }

    #[test]
    fn normalize_base_url_trims_trailing_slash() {
        assert_eq!(
            normalize_base_url("https://ci.homelab.local/"),
            "https://ci.homelab.local"
        );
        assert_eq!(
            normalize_base_url("   ci.homelab.local   "),
            "https://ci.homelab.local"
        );
    }
}
