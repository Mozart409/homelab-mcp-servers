//! Time arguments: what the caller types → what Tempo accepts.
//!
//! Tempo's query API takes `start`/`end` as **Unix seconds** and nothing else.
//! Its sibling APIs do not: Loki takes nanoseconds or RFC3339, Prometheus
//! seconds or RFC3339, and trace documents are stamped in nanoseconds. An LLM
//! moving between those tools passes whatever the last one wanted, and the
//! failure modes are all quiet:
//!
//! - **RFC3339** is refused by Tempo with a 400, which at least is loud — but
//!   it is also the most natural thing to type, so it is accepted and
//!   converted here instead.
//! - **Milliseconds or nanoseconds** are a valid integer to Tempo: it reads
//!   `1727229541000` as a date in the year 56 000, searches a window with no
//!   data in it, and answers with an empty result that looks exactly like
//!   "nothing matched". Refused here, naming the unit mistake.
//! - **`start` after `end`** is an empty window with the same empty answer.
//!   Refused when both are given.

use rmcp::ErrorData;

/// The largest value accepted as Unix seconds: the year 5138. Current
/// timestamps in milliseconds (`1.7e12`) and nanoseconds (`1.7e18`) are both
/// far above it, while no plausible trace timestamp in seconds comes close.
const MAX_UNIX_SECONDS: u64 = 100_000_000_000;

/// Parse one time argument (`name` is for the error message) into Unix
/// seconds: an integer, or an RFC3339 timestamp.
///
/// # Errors
///
/// `invalid_params` naming the argument, for anything else or for an integer
/// that is evidently milliseconds or nanoseconds.
pub(crate) fn unix_seconds(name: &str, raw: &str) -> Result<u64, ErrorData> {
    let raw = raw.trim();
    if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) {
        let secs: u64 = raw.parse().map_err(|_| invalid(name, raw))?;
        if secs > MAX_UNIX_SECONDS {
            return Err(ErrorData::invalid_params(
                format!(
                    "`{name}` = {raw} looks like milliseconds or nanoseconds; Tempo wants Unix \
                     seconds (e.g. 1727229541) or an RFC3339 timestamp"
                ),
                None,
            ));
        }
        return Ok(secs);
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(raw).map_err(|_| invalid(name, raw))?;
    u64::try_from(parsed.timestamp()).map_err(|_| invalid(name, raw))
}

fn invalid(name: &str, raw: &str) -> ErrorData {
    ErrorData::invalid_params(
        format!(
            "`{name}` = {raw:?} is neither Unix seconds (e.g. 1727229541) nor an RFC3339 \
             timestamp (e.g. 2024-09-25T02:00:00Z)"
        ),
        None,
    )
}

/// Parse an optional `start`/`end` pair and push them onto `query` as the
/// Unix-second strings Tempo expects. Omitted values are omitted, not sent
/// empty.
///
/// # Errors
///
/// As [`unix_seconds`], plus `invalid_params` when `start` is after `end`.
pub(crate) fn push_window(
    query: &mut Vec<(&'static str, String)>,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<(), ErrorData> {
    let start = start.map(|s| unix_seconds("start", s)).transpose()?;
    let end = end.map(|e| unix_seconds("end", e)).transpose()?;
    if let (Some(s), Some(e)) = (start, end)
        && s > e
    {
        return Err(ErrorData::invalid_params(
            format!("`start` ({s}) is after `end` ({e}): that window contains no traces"),
            None,
        ));
    }
    if let Some(s) = start {
        query.push(("start", s.to_string()));
    }
    if let Some(e) = end {
        query.push(("end", e.to_string()));
    }
    Ok(())
}
