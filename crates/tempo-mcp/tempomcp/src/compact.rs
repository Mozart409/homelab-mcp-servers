//! Tempo's answers, cut down to what fits in an LLM's context.
//!
//! A trace from `/api/v2/traces/{id}` is an OTLP JSON document: every span with
//! every attribute, every event and every resource attribute, repeated per
//! batch. A single request through a few services is tens of kilobytes; a
//! batch job is megabytes. Returned raw, one lookup can spend a context window.
//!
//! [`trace_tree`] replaces it with a **flat, depth-annotated span tree**: one
//! entry per span in depth-first order (children by start time), carrying the
//! span and parent ID, name, service, kind, start offset and duration in
//! milliseconds, status, and only the attributes the caller asked for. A flat
//! list with `depth` reads as a tree without the nesting that makes JSON
//! indentation cost more than the content.
//!
//! Failure modes it guards against, written before the code:
//!
//! - **IDs the caller cannot use.** Tempo's JSON encodes span and trace IDs as
//!   base64 protobuf bytes (`"spanId": "AAAAAAAAAAE="`), while every other
//!   surface — `search`, `TraceQL`, Grafana, logs — uses hex. Converted to
//!   lowercase hex, so an ID read here can be pasted into the next query.
//! - **Silent truncation.** Spans beyond `max_spans` are cut, and the answer
//!   says so (`truncated`, `omittedSpans`). The error list and the slowest
//!   spans are computed over the **whole** trace before the cut, so the span
//!   that answers "why was it slow / what failed" is never the one dropped.
//! - **Partial traces.** A span whose parent never arrived (sampling, a
//!   still-ingesting batch, a crashed service) is promoted to a root rather
//!   than lost; its `parentSpanID` still says where it hung. Tempo's own
//!   `PARTIAL` status is passed through.
//! - **Malformed trees.** A span listed as its own parent, or a parent cycle,
//!   must not loop or drop spans: every span is emitted exactly once.
//! - **Shape drift.** Tempo 2.x answers `{trace: {resourceSpans}}` on the v2
//!   endpoint, older builds `{batches}` with `instrumentationLibrarySpans`;
//!   enums arrive as strings (`SPAN_KIND_SERVER`) or integers. All accepted.
//!
//! [`search_summary`] does the same for `/api/search`: trace summaries and a
//! count of matching spans, never the spans themselves.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use base64::Engine as _;
use serde_json::{Map, Value, json};

/// Spans returned by `trace` when the caller does not say.
pub(crate) const DEFAULT_MAX_SPANS: usize = 200;
/// Error spans listed in full; the count is always exact.
const MAX_ERRORS: usize = 20;
/// Entries in the slowest-spans list.
const SLOWEST: usize = 5;

const EMPTY: &[Value] = &[];

/// What the caller asked to see of a trace.
pub(crate) struct TraceView {
    /// Spans to include in `spans` before cutting.
    pub max_spans: usize,
    /// Attribute keys to copy onto each span (span attributes first, then the
    /// span's resource attributes).
    pub attributes: Vec<String>,
}

/// One span, borrowed from the OTLP document.
struct Span<'a> {
    id: String,
    parent: Option<String>,
    name: &'a str,
    service: &'a str,
    kind: Option<&'static str>,
    start: u64,
    end: u64,
    status: Option<&'static str>,
    status_message: Option<&'a str>,
    attributes: &'a [Value],
    resource: &'a [Value],
    events: &'a [Value],
}

impl Span<'_> {
    fn duration(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// The best one-line reason this span failed: its status message, or the
    /// first `exception` event's type and message.
    fn failure(&self) -> Option<String> {
        if let Some(msg) = self.status_message.filter(|m| !m.is_empty()) {
            return Some(msg.to_string());
        }
        let event = self
            .events
            .iter()
            .find(|e| e.get("name").and_then(Value::as_str) == Some("exception"))?;
        let attrs = event
            .get("attributes")
            .and_then(Value::as_array)
            .map_or(EMPTY, Vec::as_slice);
        let text = |k| attr(attrs, k).and_then(|v| any_value(v).as_str().map(str::to_string));
        match (text("exception.type"), text("exception.message")) {
            (Some(t), Some(m)) => Some(format!("{t}: {m}")),
            (t, m) => t.or(m),
        }
    }
}

/// The compact span tree for `doc`, the body of `/api/v2/traces/{trace_id}`.
pub(crate) fn trace_tree(trace_id: &str, doc: &Value, view: &TraceView) -> Value {
    let spans = collect_spans(doc);
    let mut out = Map::new();
    out.insert("traceID".into(), json!(trace_id));
    if let Some(status) = doc.get("status").and_then(Value::as_str)
        && status != "COMPLETE"
    {
        out.insert("tempoStatus".into(), json!(status));
        if let Some(msg) = doc.get("message").and_then(Value::as_str) {
            out.insert("tempoMessage".into(), json!(msg));
        }
    }
    out.insert("spanCount".into(), json!(spans.len()));
    if spans.is_empty() {
        return Value::Object(out);
    }

    let order = tree_order(&spans);
    let trace_start = spans.iter().map(|s| s.start).min().unwrap_or(0);
    let trace_end = spans.iter().map(|s| s.end).max().unwrap_or(0);
    let span = |i: usize| spans.get(i);

    if let Some(root) = order.first().and_then(|&(i, _)| span(i)) {
        out.insert("rootService".into(), json!(root.service));
        out.insert("rootName".into(), json!(root.name));
    }
    if let Some(t) = rfc3339(trace_start) {
        out.insert("startTime".into(), json!(t));
    }
    out.insert(
        "durationMs".into(),
        json!(ms(trace_end.saturating_sub(trace_start))),
    );

    let mut services: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &spans {
        *services.entry(s.service).or_default() += 1;
    }
    out.insert("services".into(), json!(services));

    // Errors and the slowest spans are taken over the whole trace, before the
    // `max_spans` cut, so truncation can never hide the answer.
    let errors: Vec<&Span> = order
        .iter()
        .filter_map(|&(i, _)| span(i))
        .filter(|s| s.status == Some("error"))
        .collect();
    out.insert("errorCount".into(), json!(errors.len()));
    if !errors.is_empty() {
        let listed: Vec<Value> = errors
            .iter()
            .take(MAX_ERRORS)
            .map(|s| {
                let mut e = Map::new();
                e.insert("spanID".into(), json!(s.id));
                e.insert("name".into(), json!(s.name));
                e.insert("service".into(), json!(s.service));
                if let Some(msg) = s.failure() {
                    e.insert("message".into(), json!(msg));
                }
                Value::Object(e)
            })
            .collect();
        out.insert("errors".into(), Value::Array(listed));
    }

    let mut by_duration: Vec<&Span> = order.iter().filter_map(|&(i, _)| span(i)).collect();
    // Stable sort: equal durations keep tree order.
    by_duration.sort_by_key(|s| std::cmp::Reverse(s.duration()));
    let slowest: Vec<Value> = by_duration
        .iter()
        .take(SLOWEST)
        .map(|s| {
            json!({
                "spanID": s.id,
                "name": s.name,
                "service": s.service,
                "durationMs": ms(s.duration()),
            })
        })
        .collect();
    out.insert("slowest".into(), Value::Array(slowest));

    let listed: Vec<Value> = order
        .iter()
        .take(view.max_spans)
        .filter_map(|&(i, depth)| span(i).map(|s| span_entry(s, depth, trace_start, view)))
        .collect();
    let omitted = spans.len().saturating_sub(listed.len());
    out.insert("truncated".into(), json!(omitted > 0));
    if omitted > 0 {
        out.insert("omittedSpans".into(), json!(omitted));
        out.insert(
            "note".into(),
            json!(format!(
                "showing the first {} of {} spans in tree order; raise max_spans (or pass \
                 raw: true) to see the rest. errors and slowest cover the whole trace.",
                listed.len(),
                spans.len()
            )),
        );
    }
    out.insert("spans".into(), Value::Array(listed));
    Value::Object(out)
}

fn span_entry(s: &Span, depth: usize, trace_start: u64, view: &TraceView) -> Value {
    let mut e = Map::new();
    e.insert("spanID".into(), json!(s.id));
    if let Some(p) = &s.parent {
        e.insert("parentSpanID".into(), json!(p));
    }
    e.insert("depth".into(), json!(depth));
    e.insert("name".into(), json!(s.name));
    e.insert("service".into(), json!(s.service));
    if let Some(k) = s.kind {
        e.insert("kind".into(), json!(k));
    }
    e.insert(
        "startMs".into(),
        json!(ms(s.start.saturating_sub(trace_start))),
    );
    e.insert("durationMs".into(), json!(ms(s.duration())));
    if let Some(st) = s.status {
        e.insert("status".into(), json!(st));
    }
    if s.status == Some("error")
        && let Some(msg) = s.failure()
    {
        e.insert("statusMessage".into(), json!(msg));
    }
    if !s.events.is_empty() {
        e.insert("events".into(), json!(s.events.len()));
    }
    let mut attrs = Map::new();
    for key in &view.attributes {
        if let Some(v) = attr(s.attributes, key).or_else(|| attr(s.resource, key)) {
            attrs.insert(key.clone(), any_value(v));
        }
    }
    if !attrs.is_empty() {
        e.insert("attributes".into(), Value::Object(attrs));
    }
    Value::Object(e)
}

/// Every span in `doc`, whichever of Tempo's shapes it arrived in.
fn collect_spans(doc: &Value) -> Vec<Span<'_>> {
    let batches = [
        "/trace/resourceSpans",
        "/resourceSpans",
        "/trace/batches",
        "/batches",
    ]
    .iter()
    .find_map(|p| doc.pointer(p).and_then(Value::as_array))
    .map_or(EMPTY, Vec::as_slice);

    let mut spans = Vec::new();
    for batch in batches {
        let resource = batch
            .pointer("/resource/attributes")
            .and_then(Value::as_array)
            .map_or(EMPTY, Vec::as_slice);
        let service = attr(resource, "service.name")
            .and_then(|v| v.get("stringValue"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let scopes = ["scopeSpans", "instrumentationLibrarySpans"]
            .iter()
            .find_map(|k| batch.get(*k).and_then(Value::as_array))
            .map_or(EMPTY, Vec::as_slice);
        for raw in scopes
            .iter()
            .filter_map(|s| s.get("spans").and_then(Value::as_array))
            .flatten()
        {
            let list = |k: &str| {
                raw.get(k)
                    .and_then(Value::as_array)
                    .map_or(EMPTY, Vec::as_slice)
            };
            spans.push(Span {
                id: hex_id(raw.get("spanId")).unwrap_or_else(|| "unknown".to_string()),
                parent: hex_id(raw.get("parentSpanId")),
                name: raw.get("name").and_then(Value::as_str).unwrap_or(""),
                service,
                kind: kind(raw.get("kind")),
                start: nanos(raw.get("startTimeUnixNano")),
                end: nanos(raw.get("endTimeUnixNano")),
                status: status(raw.pointer("/status/code")),
                status_message: raw.pointer("/status/message").and_then(Value::as_str),
                attributes: list("attributes"),
                resource,
                events: list("events"),
            });
        }
    }
    spans
}

/// Depth-first order over the parent links, as `(index, depth)`. Roots and
/// siblings are ordered by start time (then ID, for determinism).
///
/// A span is a root when it has no parent, or its parent is not in the trace
/// (a partial trace), or it is its own parent. Spans reachable from no root
/// (a parent cycle) are emitted afterwards at depth 0. Every span appears
/// exactly once either way.
fn tree_order(spans: &[Span]) -> Vec<(usize, usize)> {
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (i, s) in spans.iter().enumerate() {
        by_id.entry(s.id.as_str()).or_insert(i);
    }
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        match s.parent.as_deref().and_then(|p| by_id.get(p)) {
            Some(&p) if p != i => children.entry(p).or_default().push(i),
            _ => roots.push(i),
        }
    }
    let key = |i: &usize| spans.get(*i).map(|s| (s.start, s.id.as_str()));
    roots.sort_by(|a, b| key(a).cmp(&key(b)));
    for list in children.values_mut() {
        list.sort_by(|a, b| key(a).cmp(&key(b)));
    }

    let mut order = Vec::with_capacity(spans.len());
    let mut visited = HashSet::new();
    for start in roots.into_iter().chain(0..spans.len()) {
        let mut stack = vec![(start, 0)];
        while let Some((i, depth)) = stack.pop() {
            if !visited.insert(i) {
                continue;
            }
            order.push((i, depth));
            if let Some(kids) = children.get(&i) {
                stack.extend(
                    kids.iter()
                        .rev()
                        .filter(|k| !visited.contains(*k))
                        .map(|&k| (k, depth + 1)),
                );
            }
        }
    }
    order
}

/// The `value` of the first attribute named `key` in an OTLP key/value list.
fn attr<'a>(attrs: &'a [Value], key: &str) -> Option<&'a Value> {
    attrs
        .iter()
        .find(|kv| kv.get("key").and_then(Value::as_str) == Some(key))
        .and_then(|kv| kv.get("value"))
}

/// An OTLP `AnyValue` (`{"stringValue": "x"}`, `{"intValue": "42"}`, …) as
/// plain JSON. `intValue` arrives as a string (protobuf JSON for 64-bit ints)
/// and becomes a number when it parses as one.
fn any_value(v: &Value) -> Value {
    if let Some(s) = v.get("stringValue") {
        return s.clone();
    }
    if let Some(i) = v.get("intValue") {
        return match i {
            Value::String(s) => s.parse::<i64>().map_or_else(|_| i.clone(), |n| json!(n)),
            other => other.clone(),
        };
    }
    for k in ["boolValue", "doubleValue", "bytesValue"] {
        if let Some(x) = v.get(k) {
            return x.clone();
        }
    }
    if let Some(values) = v.pointer("/arrayValue/values").and_then(Value::as_array) {
        return Value::Array(values.iter().map(any_value).collect());
    }
    if let Some(values) = v.pointer("/kvlistValue/values").and_then(Value::as_array) {
        let mut m = Map::new();
        for kv in values {
            if let (Some(k), Some(val)) = (kv.get("key").and_then(Value::as_str), kv.get("value")) {
                m.insert(k.to_string(), any_value(val));
            }
        }
        return Value::Object(m);
    }
    Value::Null
}

/// A span or trace ID as lowercase hex. Hex input (16 or 32 digits) is kept;
/// otherwise it is read as base64 protobuf bytes. Absent, empty and all-zero
/// IDs (how some SDKs spell "no parent") are `None`. An ID in neither form is
/// passed through untouched rather than dropped.
fn hex_id(v: Option<&Value>) -> Option<String> {
    let raw = v?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let hex = if matches!(raw.len(), 16 | 32) && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        raw.to_ascii_lowercase()
    } else {
        match base64::engine::general_purpose::STANDARD.decode(raw) {
            Ok(bytes) if !bytes.is_empty() => bytes.iter().fold(String::new(), |mut hex, b| {
                let _ = write!(hex, "{b:02x}");
                hex
            }),
            _ => return Some(raw.to_string()),
        }
    };
    if hex.bytes().all(|b| b == b'0') {
        None
    } else {
        Some(hex)
    }
}

/// A protobuf-JSON nanosecond timestamp: a string of digits, or a number.
fn nanos(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

fn kind(v: Option<&Value>) -> Option<&'static str> {
    match v? {
        Value::String(s) => match s.as_str() {
            "SPAN_KIND_INTERNAL" => Some("internal"),
            "SPAN_KIND_SERVER" => Some("server"),
            "SPAN_KIND_CLIENT" => Some("client"),
            "SPAN_KIND_PRODUCER" => Some("producer"),
            "SPAN_KIND_CONSUMER" => Some("consumer"),
            _ => None,
        },
        Value::Number(n) => match n.as_u64()? {
            1 => Some("internal"),
            2 => Some("server"),
            3 => Some("client"),
            4 => Some("producer"),
            5 => Some("consumer"),
            _ => None,
        },
        _ => None,
    }
}

fn status(v: Option<&Value>) -> Option<&'static str> {
    match v? {
        Value::String(s) if s == "STATUS_CODE_OK" => Some("ok"),
        Value::String(s) if s == "STATUS_CODE_ERROR" => Some("error"),
        Value::Number(n) if n.as_u64() == Some(1) => Some("ok"),
        Value::Number(n) if n.as_u64() == Some(2) => Some("error"),
        _ => None,
    }
}

/// Nanoseconds as milliseconds, to microsecond precision.
// `u64 as f64` is exact up to 2^53 ns (about 104 days); a span longer than
// that loses sub-millisecond digits, which is no loss at all. There is no
// lossless `From` for this conversion, hence the allow.
#[allow(clippy::cast_precision_loss)]
fn ms(ns: u64) -> f64 {
    ((ns / 1_000) as f64) / 1_000.0
}

/// Unix nanoseconds as an RFC3339 timestamp with milliseconds, UTC.
fn rfc3339(ns: u64) -> Option<String> {
    let ns = i64::try_from(ns).ok()?;
    Some(
        chrono::DateTime::from_timestamp_nanos(ns)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

/// `/api/search`'s answer as trace summaries: ID, root service and name,
/// start time, duration, and how many spans matched. The matched spans
/// themselves (`spanSet`/`spanSets`) are dropped; `trace` is how to see them.
pub(crate) fn search_summary(doc: &Value, limit: u64) -> Value {
    let traces: Vec<Value> = doc
        .get("traces")
        .and_then(Value::as_array)
        .map_or(EMPTY, Vec::as_slice)
        .iter()
        .map(|t| {
            let mut out = Map::new();
            for k in ["traceID", "rootServiceName", "rootTraceName"] {
                if let Some(v) = t.get(k) {
                    out.insert(k.into(), v.clone());
                }
            }
            if let Some(start) = rfc3339(nanos(t.get("startTimeUnixNano"))) {
                out.insert("startTime".into(), json!(start));
            }
            // Tempo omits `durationMs` for traces shorter than a millisecond.
            out.insert(
                "durationMs".into(),
                t.get("durationMs").cloned().unwrap_or(json!(0)),
            );
            let sets = t
                .get("spanSets")
                .and_then(Value::as_array)
                .map(|a| a.iter().collect::<Vec<_>>())
                .or_else(|| t.get("spanSet").map(|s| vec![s]))
                .unwrap_or_default();
            if !sets.is_empty() {
                let matched: u64 = sets
                    .iter()
                    .filter_map(|s| s.get("matched").and_then(Value::as_u64))
                    .sum();
                out.insert("matchedSpans".into(), json!(matched));
            }
            if let Some(stats) = t.get("serviceStats") {
                out.insert("serviceStats".into(), stats.clone());
            }
            Value::Object(out)
        })
        .collect();

    let returned = traces.len();
    let mut out = Map::new();
    out.insert("returned".into(), json!(returned));
    out.insert("limit".into(), json!(limit));
    out.insert(
        "limitReached".into(),
        json!(u64::try_from(returned).unwrap_or(u64::MAX) >= limit),
    );
    if let Some(metrics) = doc.get("metrics") {
        out.insert("metrics".into(), metrics.clone());
    }
    out.insert("traces".into(), Value::Array(traces));
    Value::Object(out)
}
