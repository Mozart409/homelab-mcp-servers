# 0001 — Mutating tools are allowed only behind a default-off gate

Status: accepted (2026-09-07)
Supersedes: nothing. Amends AGENTS.md Hard rules §1.

## Context

Every server in this workspace was read-only by construction: Postgres runs its
queries inside a `READ ONLY` transaction, and the REST servers issue only GETs.
The point is not that mutation is dangerous in the abstract — it is that these
servers are driven by a model, and a model that can only read cannot cause an
outage by misreading a question.

`homeassistant-mcp` was already the one exception, exposing `set_state` and
`call_service`. AGENTS.md described that as "per-server, not a precedent."

Adding `alertmanager-mcp` forced the issue. Six of its tools read; the seventh
and eighth would create and expire silences. A silence is the one Alertmanager
action where read-only access is actively frustrating rather than merely
limited: the tool can show you that nine alerts are suppressed and that a tenth
will page you during a maintenance window, and then cannot do the one thing that
would help.

So the rule had to either hold and lose that, or bend. Bending it quietly — a
second exception justified by pointing at the first — is how a rule becomes
decoration. Three servers from now, "we already made an exception" is an
argument that works for anything.

## Decision

Mutating tools are permitted in a server other than `homeassistant-mcp` only
when **all** of the following hold:

1. The mutating surface is gated behind an environment variable that defaults to
   off.
2. When the gate is closed the tools are **not registered at all** — absent from
   `tools/list`, not present-and-refusing.
3. The exception is written down: named in AGENTS.md Hard rules §1, with a
   reason, before the code lands.

`homeassistant-mcp` is grandfathered out of (1) and (2). Control is its entire
purpose, so there is no meaningful read-only mode for it to default to, and a
gate that is always on in every deployment documents nothing.

`alertmanager-mcp` implements this with `ALERTMANAGER_ALLOW_SILENCE`, merging a
second `#[tool_router]` into the router only when the flag is set.

## Why absence rather than refusal

A tool that appears in `tools/list` is a tool a model will call. If the gate is
discovered by calling into an error, every closed-gate deployment pays for it in
wasted turns, and the model's next move after a refusal is usually to try a
variation of the same call. Absence is unambiguous: the capability is not on
offer, and the server's `get_info` instructions say why, so the model reports
"this must be changed by hand" instead of retrying.

The cost is that `tools/list` is no longer identical across deployments of the
same image. That is the honest state of affairs — the deployments genuinely
differ — and it is better surfaced in the tool list than hidden behind uniform
tools with non-uniform behaviour.

## Why default-off, when hamcp is default-on

The two servers differ in what fraction of their purpose is mutation. For hamcp
it is the purpose. For `alertmanager-mcp` it is two tools out of eight, and the
blast radius is asymmetric: a silence is homelab-wide alert blindness for its
duration, and a silence created by a misread question is invisible precisely
because its effect is the *absence* of notifications. Nothing pages you to say
that paging has stopped.

Defaulting off means enabling that is a deliberate deployment act rather than a
property of having pulled an image.

## Consequences

- AGENTS.md Hard rules §1 now names two exceptions and states the condition
  above. It also says two exceptions are not a pattern: a third needs the
  owner's say-so and a written reason.
- `alertmanager-mcp` carries tests asserting both directions of the gate — that
  the write tools are absent when it is closed and present when it is open —
  because the safety property here is the *absence* of routes, which nothing
  else would catch if the merge were made unconditional.
- Deployments that want silence writes must set the variable explicitly; the
  systemd/NixOS module and `compose.yaml` do not set it.
- A future server wanting writes has a path to follow rather than a precedent to
  argue from. If that path is followed three or four times, this ADR should be
  revisited — at that point "read-only by default" is no longer what the
  workspace does, and the rule should say what it actually is.
