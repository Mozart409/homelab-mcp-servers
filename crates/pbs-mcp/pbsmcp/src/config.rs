//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr};

/// Port assumed when `PBS_HOST` names a host without one.
const DEFAULT_PORT: u16 = 8007;

/// Connection settings for a Proxmox Backup Server instance.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL including scheme and port, e.g. `https://pbs.lan:8007`.
    pub base_url: String,
    /// API token, formatted `user@realm!tokenname:secret` (the value sent after
    /// `PBSAPIToken=`).
    pub api_key: String,
    /// PBS node name used for `/nodes/{node}/...` endpoints (default `localhost`).
    pub node: String,
    /// Accept self-signed / invalid TLS certificates.
    pub insecure: bool,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8080`).
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
}

impl Config {
    /// Build a [`Config`] from the `PBS_*` environment variables.
    ///
    /// Required: `PBS_HOST`, `PBS_API_KEY`.
    /// Optional: `PBS_NODE` (default `localhost`), `PBS_INSECURE` (`1`/`true`),
    /// `PBS_BIND` (default `127.0.0.1:8080`), `PBS_ALLOWED_HOSTS` (comma-separated).
    ///
    /// # Errors
    ///
    /// Returns an error if any required environment variable is unset.
    pub fn from_env() -> Result<Self> {
        let host = std::env::var("PBS_HOST")
            .wrap_err("PBS_HOST must be set (e.g. https://pbs.lan:8007 or pbs.lan)")?;
        let api_key = std::env::var("PBS_API_KEY")
            .wrap_err("PBS_API_KEY must be set (e.g. user@pbs!tokenname:secret)")?;
        let node = std::env::var("PBS_NODE").unwrap_or_else(|_| "localhost".to_string());
        let insecure = matches!(
            std::env::var("PBS_INSECURE").as_deref(),
            Ok("1" | "true" | "yes")
        );
        let bind = std::env::var("PBS_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
        let allowed_hosts =
            mcp_common::parse_allowed_hosts(std::env::var("PBS_ALLOWED_HOSTS").ok().as_deref());

        Ok(Self {
            base_url: normalize_base_url(&host),
            api_key,
            node,
            insecure,
            bind,
            allowed_hosts,
        })
    }
}

/// Turn a user-supplied host into a full base URL, defaulting scheme to
/// `https` and port to PBS's `8007` when not already specified.
fn normalize_base_url(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    if h.starts_with("http://") || h.starts_with("https://") {
        return h.to_string();
    }

    // Whether `h` already carries a port cannot be decided by a bare
    // `contains(':')`: IPv6 literals are full of colons. That test read `[::1]`
    // as "already has a port" and skipped the default, so the client silently
    // talked to 443 instead of PBS.
    if let Some(close) = h.rfind(']') {
        // Bracketed IPv6 — a port can only appear after the closing bracket.
        let has_port = h.get(close + 1..).is_some_and(|rest| rest.starts_with(':'));
        return if has_port {
            format!("https://{h}")
        } else {
            format!("https://{h}:{DEFAULT_PORT}")
        };
    }

    if h.matches(':').count() > 1 {
        // Unbracketed IPv6 literal such as `::1`. It cannot carry a port (there
        // would be no way to tell which colon started it), and it must be
        // bracketed before it is legal in a URL — previously this produced
        // `https://::1`, which reqwest rejects at request time.
        return format!("https://[{h}]:{DEFAULT_PORT}");
    }

    if h.contains(':') {
        format!("https://{h}")
    } else {
        format!("https://{h}:{DEFAULT_PORT}")
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, normalize_base_url};
    use std::sync::{Mutex, PoisonError};

    #[test]
    fn normalize_base_url_table() {
        // (input, expected) — one table so the whole contract reads at a glance.
        let cases: &[(&str, &str)] = &[
            // Bare host: scheme AND PBS's default port are supplied.
            ("pbs.lan", "https://pbs.lan:8007"),
            // Host that already carries a port: no second port appended.
            ("pbs.lan:8007", "https://pbs.lan:8007"),
            ("pbs.lan:443", "https://pbs.lan:443"),
            // An explicit scheme is preserved verbatim — `http` is NOT upgraded
            // to `https`, so a plaintext homelab target keeps working.
            ("http://pbs.lan", "http://pbs.lan"),
            ("http://pbs.lan:8007", "http://pbs.lan:8007"),
            ("https://pbs.lan:8007", "https://pbs.lan:8007"),
            // A bare `https://host` is left as-is: no default port is added once
            // a scheme is present.
            ("https://pbs.lan", "https://pbs.lan"),
            // Trailing slashes are stripped so paths concatenate cleanly.
            ("https://pbs.lan:8007/", "https://pbs.lan:8007"),
            ("https://pbs.lan:8007///", "https://pbs.lan:8007"),
            ("pbs.lan/", "https://pbs.lan:8007"),
            // Surrounding whitespace (a stray space in `.env`) is trimmed.
            ("  pbs.lan  ", "https://pbs.lan:8007"),
            ("\t https://pbs.lan:8007/ \n", "https://pbs.lan:8007"),
            // IPv4 literals behave exactly like hostnames.
            ("192.168.1.10", "https://192.168.1.10:8007"),
            ("192.168.1.10:8007", "https://192.168.1.10:8007"),
        ];

        for (input, expected) in cases {
            assert_eq!(
                normalize_base_url(input),
                *expected,
                "normalize_base_url({input:?})"
            );
        }
    }

    #[test]
    fn normalize_base_url_handles_ipv6_literals() {
        // Bracketed IPv6 with an explicit port is left alone.
        assert_eq!(normalize_base_url("[::1]:8007"), "https://[::1]:8007");
        assert_eq!(
            normalize_base_url("[fe80::1]:9999"),
            "https://[fe80::1]:9999"
        );

        // Regression: bracketed IPv6 without a port now gets the PBS default.
        // The old port check was a bare `contains(':')`, and an IPv6 literal is
        // full of colons — so this took the "already has a port" branch and the
        // client silently talked to 443 instead of 8007.
        assert_eq!(normalize_base_url("[::1]"), "https://[::1]:8007");
        assert_eq!(normalize_base_url("[fe80::1]"), "https://[fe80::1]:8007");

        // Regression: an unbracketed literal is bracketed rather than pasted in
        // raw. It previously produced `https://::1`, which is not a structurally
        // valid URL and which reqwest rejected only at request time.
        assert_eq!(normalize_base_url("::1"), "https://[::1]:8007");
        assert_eq!(
            normalize_base_url("2001:db8::1"),
            "https://[2001:db8::1]:8007"
        );

        // A single colon is still read as host:port, not as IPv6.
        assert_eq!(normalize_base_url("pbs.lan:8007"), "https://pbs.lan:8007");
    }

    // `from_env` reads process-global state, so serialize the tests that mutate
    // it. `unwrap_or_else(into_inner)` keeps a panicking test from poisoning the
    // lock and cascading failures into the others.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const KEYS: &[&str] = &[
        "PBS_HOST",
        "PBS_API_KEY",
        "PBS_NODE",
        "PBS_INSECURE",
        "PBS_BIND",
        "PBS_ALLOWED_HOSTS",
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
        set_env("PBS_API_KEY", "user@pbs!tok:secret");

        assert!(Config::from_env().is_err());

        clear_env();
    }

    #[test]
    fn from_env_requires_api_key() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PBS_HOST", "pbs.lan");

        assert!(Config::from_env().is_err());

        clear_env();
    }

    #[test]
    fn from_env_applies_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PBS_HOST", "pbs.lan");
        set_env("PBS_API_KEY", "user@pbs!tok:secret");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "https://pbs.lan:8007");
        assert_eq!(cfg.api_key, "user@pbs!tok:secret");
        assert_eq!(cfg.node, "localhost");
        // Hard rule §2: loopback by default.
        assert_eq!(cfg.bind, "127.0.0.1:8080");
        assert!(cfg.allowed_hosts.is_none());
        assert!(!cfg.insecure);

        clear_env();
    }

    #[test]
    fn from_env_honours_overrides() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PBS_HOST", "http://pbs.lan:8007/");
        set_env("PBS_API_KEY", "k");
        set_env("PBS_NODE", "pbs01");
        set_env("PBS_BIND", "0.0.0.0:9000");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.base_url, "http://pbs.lan:8007");
        assert_eq!(cfg.node, "pbs01");
        assert_eq!(cfg.bind, "0.0.0.0:9000");

        clear_env();
    }

    #[test]
    fn from_env_insecure_truthiness_is_exact_lowercase_only() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PBS_HOST", "pbs.lan");
        set_env("PBS_API_KEY", "k");

        // Unset is false.
        assert!(!Config::from_env().unwrap().insecure);

        // The impl is `matches!(.., Ok("1" | "true" | "yes"))`: only these three
        // exact, all-lowercase spellings enable TLS-verification bypass.
        for truthy in ["1", "true", "yes"] {
            set_env("PBS_INSECURE", truthy);
            assert!(
                Config::from_env().unwrap().insecure,
                "PBS_INSECURE={truthy:?} should be true"
            );
        }

        // Everything else is false — including the capitalised spellings and
        // `on`, which a user could reasonably expect to work. Failing closed
        // (certificate verification stays ON) is the safe direction.
        for falsy in [
            "TRUE", "True", "YES", "Yes", "on", "ON", "0", "false", "no", "", " ", "1 ", "enabled",
        ] {
            set_env("PBS_INSECURE", falsy);
            assert!(
                !Config::from_env().unwrap().insecure,
                "PBS_INSECURE={falsy:?} should be false"
            );
        }

        clear_env();
    }

    #[test]
    fn from_env_allowed_hosts_splits_trims_and_filters() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PBS_HOST", "pbs.lan");
        set_env("PBS_API_KEY", "k");

        // Unset stays `None` — rmcp's loopback-only default.
        assert!(Config::from_env().unwrap().allowed_hosts.is_none());

        // Single value.
        set_env("PBS_ALLOWED_HOSTS", "pbs.tailnet.ts.net");
        assert_eq!(
            Config::from_env().unwrap().allowed_hosts.unwrap(),
            vec!["pbs.tailnet.ts.net".to_string()]
        );

        // Comma-split, per-item trimmed, empty items dropped.
        set_env("PBS_ALLOWED_HOSTS", " a.example.com , localhost ,, ");
        assert_eq!(
            Config::from_env().unwrap().allowed_hosts.unwrap(),
            vec!["a.example.com".to_string(), "localhost".to_string()]
        );

        // Regression: a value that trims to nothing collapses to `None`. It used
        // to yield `Some(vec![])`, which reaches rmcp's `with_allowed_hosts` and
        // rejects *every* inbound Host header — so a typo, or a config template
        // that expanded to nothing, produced a server that silently accepted no
        // connections at all. `None` restores the loopback default (§2).
        for blank in [",, ,", "", ",", "   ", ",,,"] {
            set_env("PBS_ALLOWED_HOSTS", blank);
            assert_eq!(
                Config::from_env().unwrap().allowed_hosts,
                None,
                "{blank:?} should fall back to the loopback default"
            );
        }

        clear_env();
    }
}
