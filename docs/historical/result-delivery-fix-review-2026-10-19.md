# Result delivery correction and verification — 2026-10-19

## Changes

The [recheck findings](restore-13-17-recheck-2026-10-19.md) are addressed in
Raphael `2bae36fb1e074cd93a270e47db2e7b6af99ecf78` and Ignis
`3a3c65a4f12fb8551302063e5a7bf94050eadd69`. Documentation commits follow these
exact tested code heads. Branch in both repositories: `fix/restore-clone-leases`.

- Result delivery keeps its heartbeat while retrying the cached result. There
  are at most three failed transport/429/5xx exchanges. Processing409 responses
  wait within the original execution deadline, never a reset budget.
- Every final delivery error, deadline exhaustion, lease denial or malformed
  successful response uses journaled terminal cleanup. Successful cleanup removes
  the record; failed cleanup leaves a terminal cleanup-retry record for restart.
- Dispatcher saves the safe terminal envelope. Identical committed results replay
  it after tenant, connector-owner and payload-fingerprint checks, including lazy
  loading after restart. Terminal jobs remain excluded from polling; executable
  patches are not replayed. Older terminal records lacking the envelope retain
  their documented prior limitation.
- Raphael decision `D-20261019-01` records claim ownership, safe handover,
  publication intent/total deadline and the delivery/replay choices.

## Targeted expected and actual results

| Regression | Expected | Actual |
| --- | --- | --- |
| `result_delivery_timeout_cleans_workspace_and_journal` | Three stalled-body exchanges at30ms each, one action execution, identical cached retries, destroy1 and workspace/journal removed | PASS |
| `result_delivery_retries_cached_result_and_recovers_terminal` | Stalled first body, successful terminal replay on retry, no second execution, cleanup1 | PASS |
| `result_delivery_processing_conflicts_end_at_budget_and_clean_up` | Repeated409 ends at shared150ms execution budget, with cleanup; no fresh budget per retry | PASS |
| `result_delivery_failed_cleanup_is_retried_after_restart` | Destroy failure leaves terminal `cleanup_failed`; reopened disk journal retries destroy and removes record after success | PASS |
| `test_terminal_result_replays_after_response_loss_and_restart[False/True]` | Exact saved terminal returned before/after restart; changed payload, wrong owner and wrong tenant refused; polling stays empty | PASS (2 cases) |
| Existing replay-noop control | Nonterminal duplicate remains an idempotent noop | PASS |

The two Python controls failed on the original code. An initial Rust test-only
`Instant` import error was corrected; the four corrected Rust controls all fail
against an isolated copy of the previous production source and pass against the
fixed source. The reproductions did not replace production fixtures or source.

## Full verification

Linux, Python3.12.3, Git2.43.0, Rust/Cargo1.97.1. Mock/public fixture:
`AmazingDude/raphael-e2e-fixture@57f0801fe46527c7531d62c5e278db80b7b56564`.
Remote publication stayed dry-run. Contract pin: `contracts-v1.3.0`, with
unreleased additive candidate HTTP/lease schemas.

| Check | Result |
| --- | --- |
| Agent pytest | 441 passed / 4 skipped |
| Dispatch pytest | 88 passed |
| Rust cargo test | 86 passed; bin/doc targets zero tests |
| Contract harness | 19 passed |
| Harness/evaluator assertions | 31 passed |
| Checkout matrix | 36 combinations, both paths/minimal patch per case, passed |
| Paired wire E2E | All3 scenarios passed; TTL30/diagnosis35, same sandbox after process restart and clone cleanup |
| Candidate comparison | All11 connector files identical; original7 JSON schemas unchanged |
| Formatting/whitespace | cargo fmt and git diff checks pass |
| Clippy | Completes with the five previously documented warning sites |
| Merge check | Both branch heads merge cleanly with fetched main via `git merge-tree --write-tree` |

One initial full Rust invocation reused the old-source reproduction's generated
artifact because the temporary manifest shared its target directory. Its four
old-bug assertions and three missing-temporary-fixture errors were not accepted
as fixed-head results. Clearing only this package's generated artifacts forced
recompilation from the persistent fixed checkout; the fresh run passed86/86.
Future isolated old-source reproductions must use a separate target directory.

Commands match the earlier recovery review, with new focused commands:

```bash
PYTHONPATH=dispatch:agent python -m pytest dispatch/tests/test_orchestrator.py -k 'terminal_result_replays or replayed_result_is_noop' -q
cargo test --manifest-path controller/Cargo.toml --locked --offline result_delivery_
E2E_LEASE_TTL_SECONDS=30 E2E_DIAGNOSE_DELAY_SECONDS=35 E2E_IGNIS_BIN=<fixed-binary> python e2e/run_e2e.py
```

Sanitized results are retained here; local diagnostic logs are
`/tmp/delivery-fix-{agent,dispatch,rust-final,contracts,harness,clone,e2e,clippy}.log`.
No raw wire bodies or credentials are committed.

## Standards

Independent read-only recheck: **zero remaining findings**. The decision entry
satisfies `CONTRIBUTING.md`; retry/cleanup/replay fixes address both runtime paths.

## Spec

Independent read-only recheck: **zero remaining findings** in the corrections.
LSE-09's final delivery-error path now uses the same journaled cleanup as lease
loss. Full timeout/denial/platform qualification remains a separate gate.

## PR and CI boundaries

The recovery branches had no open PRs. Publish one draft PR per repository for
user review; do not merge. Ignis includes the published #19 candidate prerequisites
from its preservation base, overlapping #19; #20 Secret-safety work remains separate.
Both PRs should disclose this relationship.

PR creation triggers the configured CI. Raphael's contract drift check is
expected to block: candidate HTTP/lease additions differ from the released
`contracts-v1.3.0` pin. Do not disable it or retarget an existing tag. Cross-repo
jobs still default to the old released controller, so their green status would
not qualify this candidate pair. Reviewed Ignis release, snapshot/pin alignment,
current-pair hosted kind/short-lease/denial and #11 positive proof remain open.
Authenticated cloning, owned system attributes and native Windows remain
unqualified. mypy/ruff/PowerShell were not available locally. No main merge,
release/tag or manual hosted dispatch is performed by this correction.
