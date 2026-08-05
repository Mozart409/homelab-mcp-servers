//! Runtime configuration, sourced from environment variables.

use color_eyre::eyre::{Result, WrapErr, eyre};

/// Connection and serving settings for a Postgres MCP instance.
#[derive(Clone, Debug)]
pub struct Config {
    /// libpq/sqlx connection string, e.g.
    /// `postgres://user:pass@host:5432/dbname`.
    pub database_url: String,
    /// Address to bind the streamable-HTTP MCP server to (default `127.0.0.1:8081`).
    pub bind: String,
    /// Allowed `Host` header values for inbound requests. `None` keeps rmcp's
    /// loopback-only default (DNS-rebinding protection); set this when serving
    /// on a hostname (e.g. behind Tailscale or a reverse proxy).
    pub allowed_hosts: Option<Vec<String>>,
    /// Maximum size of the connection pool (default `5`).
    pub max_connections: u32,
    /// Per-statement timeout in milliseconds, applied to every query the tools
    /// run (default `5000`). Guards against runaway queries.
    pub statement_timeout_ms: u64,
    /// Hard cap on the number of rows any single tool call returns
    /// (default `1000`). Keeps result payloads bounded.
    pub max_rows: i64,
}

impl Config {
    /// Build a [`Config`] from the `PG_*` environment variables.
    ///
    /// Required: a connection string in `PG_DATABASE_URL` (or the conventional
    /// `DATABASE_URL`).
    /// Optional: `PG_BIND` (default `127.0.0.1:8081`), `PG_ALLOWED_HOSTS`
    /// (comma-separated), `PG_MAX_CONNECTIONS` (default `5`),
    /// `PG_STATEMENT_TIMEOUT_MS` (default `5000`), `PG_MAX_ROWS` (default `1000`).
    ///
    /// # Errors
    ///
    /// Returns an error if no connection string is set or a numeric override
    /// fails to parse.
    pub fn from_env() -> Result<Self> {
        let database_url = std::env::var("PG_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .wrap_err(
                "PG_DATABASE_URL (or DATABASE_URL) must be set \
                 (e.g. postgres://user:pass@host:5432/dbname)",
            )?;
        let bind = std::env::var("PG_BIND").unwrap_or_else(|_| "127.0.0.1:8081".to_string());
        let allowed_hosts = parse_allowed_hosts(std::env::var("PG_ALLOWED_HOSTS").ok().as_deref());
        let max_connections = parse_env("PG_MAX_CONNECTIONS", 5)?;
        let statement_timeout_ms = parse_env("PG_STATEMENT_TIMEOUT_MS", 5_000)?;
        let max_rows = parse_env("PG_MAX_ROWS", 1_000)?;

        Ok(Self {
            database_url,
            bind,
            allowed_hosts,
            max_connections,
            statement_timeout_ms,
            max_rows,
        })
    }
}

/// Read an optional numeric environment variable, falling back to `default`
/// when unset and erroring when set but unparseable.
fn parse_env<T>(key: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(v) => v
            .trim()
            .parse()
            .map_err(|e| eyre!("{key} must be a valid number: {e}")),
        Err(_) => Ok(default),
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

    // `from_env`/`parse_env` read process-global state, so serialize the tests
    // that mutate it. `unwrap_or_else(into_inner)` keeps a panicking test from
    // poisoning the lock and cascading failures into the others.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const KEYS: &[&str] = &[
        "PG_DATABASE_URL",
        "DATABASE_URL",
        "PG_BIND",
        "PG_ALLOWED_HOSTS",
        "PG_MAX_CONNECTIONS",
        "PG_STATEMENT_TIMEOUT_MS",
        "PG_MAX_ROWS",
        "__PG_PARSE_TEST",
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
    fn parse_env_uses_default_then_value_then_errors() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();

        assert_eq!(parse_env::<u64>("__PG_PARSE_TEST", 7).unwrap(), 7);
        set_env("__PG_PARSE_TEST", " 12 ");
        assert_eq!(parse_env::<u64>("__PG_PARSE_TEST", 7).unwrap(), 12);
        set_env("__PG_PARSE_TEST", "not-a-number");
        assert!(parse_env::<u64>("__PG_PARSE_TEST", 7).is_err());

        clear_env();
    }

    #[test]
    fn from_env_requires_a_connection_string() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        assert!(Config::from_env().is_err());
        clear_env();
    }

    #[test]
    fn from_env_prefers_pg_database_url_and_applies_defaults() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("DATABASE_URL", "postgres://fallback/db");
        set_env("PG_DATABASE_URL", "postgres://primary/db");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.database_url, "postgres://primary/db");
        assert_eq!(cfg.bind, "127.0.0.1:8081");
        assert_eq!(cfg.max_connections, 5);
        assert_eq!(cfg.statement_timeout_ms, 5_000);
        assert_eq!(cfg.max_rows, 1_000);
        assert!(cfg.allowed_hosts.is_none());

        clear_env();
    }

    #[test]
    fn from_env_parses_allowed_hosts_and_numeric_overrides() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("DATABASE_URL", "postgres://x/db");
        set_env("PG_ALLOWED_HOSTS", " a.example.com , localhost ,, ");
        set_env("PG_MAX_ROWS", "10");

        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.allowed_hosts.unwrap(),
            vec!["a.example.com".to_string(), "localhost".to_string()]
        );
        assert_eq!(cfg.max_rows, 10);

        clear_env();
    }

    #[test]
    fn from_env_falls_back_to_database_url() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("DATABASE_URL", "postgres://fallback/db");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.database_url, "postgres://fallback/db");

        clear_env();
    }

    #[test]
    fn from_env_reads_every_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PG_DATABASE_URL", "postgres://x/db");
        set_env("PG_BIND", "0.0.0.0:9999");
        set_env("PG_ALLOWED_HOSTS", "mcp.example.com");
        set_env("PG_MAX_CONNECTIONS", "17");
        set_env("PG_STATEMENT_TIMEOUT_MS", "250");
        set_env("PG_MAX_ROWS", "42");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.database_url, "postgres://x/db");
        assert_eq!(cfg.bind, "0.0.0.0:9999");
        assert_eq!(cfg.allowed_hosts.unwrap(), vec!["mcp.example.com"]);
        assert_eq!(cfg.max_connections, 17);
        assert_eq!(cfg.statement_timeout_ms, 250);
        assert_eq!(cfg.max_rows, 42);

        clear_env();
    }

    #[test]
    fn from_env_defaults_are_loopback_only() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PG_DATABASE_URL", "postgres://x/db");

        // Hard rule §2: the *code* default must stay loopback with rmcp's
        // DNS-rebinding protection intact (`allowed_hosts: None`).
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:8081");
        assert!(cfg.bind.starts_with("127.0.0.1:"));
        assert!(cfg.allowed_hosts.is_none());

        clear_env();
    }

    #[test]
    fn from_env_allowed_hosts_of_only_separators_is_empty() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PG_DATABASE_URL", "postgres://x/db");
        // Regression: set-but-blank is treated as unset. It used to produce
        // `Some(vec![])`, which reaches rmcp's `with_allowed_hosts` and rejects
        // *every* inbound Host header — a typo'd value yielded a server that
        // silently accepted no connections. Falling back to `None` restores the
        // loopback-only default (hard rule §2).
        for blank in [" , , ", "", ",", "   ", ",,,"] {
            set_env("PG_ALLOWED_HOSTS", blank);
            assert_eq!(
                Config::from_env().unwrap().allowed_hosts,
                None,
                "{blank:?} should fall back to the loopback default"
            );
        }

        clear_env();
    }

    #[test]
    fn from_env_rejects_unparseable_numbers_without_panicking() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);

        for (key, bad) in [
            ("PG_MAX_CONNECTIONS", "many"),
            ("PG_MAX_CONNECTIONS", "-1"),  // u32 rejects negatives
            ("PG_MAX_CONNECTIONS", "5.5"), // and non-integers
            ("PG_MAX_CONNECTIONS", "4294967296"), // u32 overflow
            ("PG_STATEMENT_TIMEOUT_MS", "soon"),
            ("PG_STATEMENT_TIMEOUT_MS", "-1"), // u64 rejects negatives
            ("PG_MAX_ROWS", "lots"),
            ("PG_MAX_ROWS", "9223372036854775808"), // i64 overflow
            ("PG_MAX_ROWS", ""),                    // empty is not a number
        ] {
            clear_env();
            set_env("PG_DATABASE_URL", "postgres://x/db");
            set_env(key, bad);

            let err = Config::from_env()
                .expect_err(&format!("{key}={bad:?} must be rejected"))
                .to_string();
            assert!(
                err.contains(key),
                "error for {key}={bad:?} should name the variable: {err}"
            );
        }

        clear_env();
    }

    #[test]
    fn from_env_accepts_boundary_numeric_values() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();
        set_env("PG_DATABASE_URL", "postgres://x/db");
        set_env("PG_MAX_CONNECTIONS", "0");
        set_env("PG_STATEMENT_TIMEOUT_MS", "0");
        set_env("PG_MAX_ROWS", "0");

        // Zeros parse — they are nonsensical but must not panic here; the row
        // cap is floored downstream in `client::clamp_limit`.
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.max_connections, 0);
        assert_eq!(cfg.statement_timeout_ms, 0);
        assert_eq!(cfg.max_rows, 0);

        set_env("PG_MAX_CONNECTIONS", "4294967295");
        set_env("PG_STATEMENT_TIMEOUT_MS", "18446744073709551615");
        set_env("PG_MAX_ROWS", "9223372036854775807");

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.max_connections, u32::MAX);
        assert_eq!(cfg.statement_timeout_ms, u64::MAX);
        assert_eq!(cfg.max_rows, i64::MAX);

        // Negative row caps parse (i64) and are floored at query time.
        set_env("PG_MAX_ROWS", "-1");
        assert_eq!(Config::from_env().unwrap().max_rows, -1);

        clear_env();
    }

    #[test]
    fn parse_env_tolerates_surrounding_whitespace() {
        let _g = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        clear_env();

        set_env("__PG_PARSE_TEST", "\t 99 \n");
        assert_eq!(parse_env::<i64>("__PG_PARSE_TEST", 0).unwrap(), 99);
        // But internal whitespace is still an error, not a silent truncation.
        set_env("__PG_PARSE_TEST", "9 9");
        assert!(parse_env::<i64>("__PG_PARSE_TEST", 0).is_err());

        clear_env();
    }
}
