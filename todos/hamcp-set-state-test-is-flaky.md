# `hamcp` set_state percent-encoding test is flaky (~1.5%)

`server::tests::set_state_posts_to_percent_encoded_entity_path` in
`crates/homeassistant-mcp/hamcp/src/server.rs` fails intermittently. It will
redden CI at random, on branches that never touched hamcp.

```
panicked at crates/homeassistant-mcp/hamcp/src/server.rs:749:45:
called `Option::unwrap()` on a `None` value
```

Line 749 is `let request = recorded(&mock).await.unwrap();` — not an assertion
about the URL, which is what the test is nominally about.

## Measured rate

Run in isolation (`cargo test -p hamcp --lib set_state_posts_to_percent_encoded_entity_path`):

- 29 pass / 1 fail out of 30
- 40 pass / 0 fail out of 40, **with an `eprintln!` added to `recorded()` and
  `--nocapture`**

Roughly 1 in 70. The instrumented run not reproducing it is the interesting
part: adding I/O to the helper shifted the timing enough to hide it, so this is
a genuine race rather than a bad assertion, and printf-debugging it in place
will not work.

## What is known

`recorded()` returns `None` unless *exactly one* request was recorded, and the
test unwraps that, so the panic means the count was not 1:

```rust
async fn recorded(mock: &MockServer) -> Option<Request> {
    let mut requests = mock.received_requests().await?.into_iter();
    let first = requests.next()?;
    if requests.next().is_some() {
        return None;
    }
    Some(first)
}
```

Two candidate counts, one of which is ruled out:

- **0 requests — ruled out.** wiremock 0.6.5 records the request *before* it
  responds, inside `MockServerState::handle_request`, under the state write
  lock. By the time the client has a response the request is in the vec. The
  tool call also succeeds, so a request certainly arrived.
- **≥2 requests — the remaining explanation**, and unexplained.
  `HaClient::set_state` issues exactly one POST and nothing retries it. Where a
  second recorded request could come from is the open question.

## Next step

Do not start by guessing at a fix. `recorded()` throws away the one fact that
would identify this — the actual count — which is why a rare CI failure says
nothing useful. Change it to carry the count into the failure, e.g. return
`Result<Request, String>` or have callers assert on
`mock.received_requests().await` directly, so the next red build reports
`expected 1 request, got N` along with the offending requests.

Only once N is known is a fix worth designing. If N turns out to be 2, compare
the two recorded requests — if they are identical, suspect the client or the
connection layer; if they differ, suspect something else reaching that port.

## Not a blocker

Every other hamcp test passes consistently, and full `just ci` runs have gone
green repeatedly with this test included. Re-run once before investigating a red
CI that points here.
