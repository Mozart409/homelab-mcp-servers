# Architecture decision records

Decisions that constrain future work, with the reasoning that produced them.

An ADR belongs here when a decision is **binding on later changes** and its
rationale is not recoverable from the code — most often when the code shows only
the winning option and nothing about the ones rejected, or when a rule exists to
stop a plausible future change. Implementation notes that a reader can derive
from the source belong in a crate README or a code comment instead.

- Numbered `NNNN-kebab-title.md`, allocated in order, never renumbered.
- Status is `accepted`, `superseded by NNNN`, or `deprecated`. An ADR is not
  edited to reverse itself — write a new one and mark the old superseded, so the
  history of the decision survives.
- Hard rules in [`AGENTS.md`](../../AGENTS.md) are the enforceable summary; the
  ADR is where the reasoning lives. When the two disagree, AGENTS.md is what an
  agent follows and the ADR needs updating.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-gated-mutating-tools.md) | Mutating tools are allowed only behind a default-off gate | accepted |
| [0002](0002-flake-is-the-server-registry.md) | `flake.nix` is the single registry of server binaries | accepted |
