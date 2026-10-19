# Recovery verification recheck — 2026-10-19

**Correction delivered:** the findings in the recheck were fixed and freshly
verified; see [the correction record](result-delivery-fix-review-2026-10-19.md).
Historical observations and failures below are retained for review.

## Verdict and identities

**Changes requested.** Existing suites and standard paired scenarios pass, but
fresh targeted checks expose two result-delivery/cleanup gaps. This supersedes
the earlier zero-findings review conclusion; no source fixes were made in this
verification turn. The isolated Rust reproduction intentionally fails.

Tested Raphael `df3b770` (code `f1ecbe88d5b8ee49d99255df4a2cabf5541a2a51`),
Ignis `65215b6` (code `c94de3e7d7a02424945f8c57eb643ce04512b63b`).
Comparisons: `git diff 903370a...HEAD` in Raphael and
`git diff 9d88f47...HEAD` in Ignis. Linux; Python3.12.3/Git2.43.0/Rust1.97.1.
Contract pin remains `contracts-v1.3.0`; candidate schemas remain unreleased.

## Standards

Independent read-only review reports:

1. **P2 — Result transport failure abandons cleanup.** Ignis
   `controller/src/connector.rs:1122` propagates exchange errors without the
   journaled `handle_terminal` path used for lease denial. Budget exhaustion at
   lines1103–1105/1140–1142 similarly escapes. The heartbeat is dropped; local
   work remains active. Once dispatch terminalizes the expired job, polling
   excludes it, and startup restores its local active record. Namespace TTL does
   not remove connector workspaces. Retry transient delivery failures within
   confirmed bounds and reconcile/clean up when progress cannot continue.
2. **P2 — Pre-existing terminal replay gap.** Raphael
   `dispatch/raphael_dispatch/orchestrator.py:423` returns an empty message list
   for identical committed-result retries. Losing the terminal HTTP response
   therefore loses the cleanup instruction; terminal jobs are also excluded
   from queue polling. Replay the terminal outcome or provide explicit terminal
   reconciliation. This predates the recovery diff but affects its guarantees.
3. **P3 — Architecture decision record missing.** Raphael
   `CONTRIBUTING.md:86–88` requires newest-first `D-YYYYMMDD-NN` entries in
   `decision.md`. Credential-owned persisted claims, handover and publication
   intent are described in the plan/review but have no decision entry.

No additional actionable simplicity/naming smells were reported.

## Spec

Independent read-only review reports **one P1 finding**: result-delivery timeout
and budget exhaustion skip cleanup (same runtime path described above).
Plan P4 requires “bounded retry … journal cleanup”; checked LSE-09 promised
“On timeout/lease loss … journal sandbox/workspace cleanup.” HTTP timeout and
repeated409-budget exhaustion do not satisfy that requirement. Preserve cached
results for bounded retry, handle uncertain terminal delivery explicitly, and
assert successful cleanup or a terminal cleanup-retry journal as appropriate.

No additional concrete clone-policy, handover-identity, aggregate-validation or
publication-intent violations were found. Qualification gates remain open.

Review summary: Standards3 findings (worst P2); Spec1 finding (worst P1).
The shared timeout finding is not two independent bugs.

## Fresh executed checks

| Check | Expected | Actual |
| --- | --- | --- |
| Full agent pytest | Existing regression suite passes | 441 passed / 4 skipped |
| Full dispatch pytest | Existing lifecycle suite passes | 86 passed |
| Full Rust cargo test | Existing controller suite passes | 82 passed; bin/doc targets zero tests |
| Contract harness | Candidate schemas valid | 19 passed |
| Harness/evaluator assertions | Counters, launchers and proof assertions pass | 31 passed |
| Owned clone-preference matrix | Same policy in both paths | 36 combinations passed |
| Paired mock E2E | Repair, max-attempt escalation, restart/cleanup | All3 scenarios passed; TTL30/diagnosis35; same sandbox after process restart |
| Rust formatting | No formatting drift | Passed |
| Clippy | Complete with warnings identified | Completed; existing five warning sites from earlier review remain |
| Result body timeout reproduction | Error must not abandon active local resources; reconcile/retry or journal cleanup | **FAILED:** error returned, workspace exists, nonterminal journal exists, destroy calls0 |
| Lost terminal response reproduction | Retry/poll must recover terminal cleanup instruction | **CONFIRMED GAP:** first response terminal, duplicate messages empty, subsequent poll empty |

### Targeted Rust reproduction

Executed in a disposable copy of the exact Ignis controller source, leaving
both recovery branches unchanged. Test name:
`connector::tests::review_result_timeout_must_clean_workspace_and_journal`.

Setup: existing `FakeCloner`/`FakeExecutor`, a prepared owned workspace, a valid
TTL30 claim with action deadline200s/job300s, HTTP timeout50ms, and an Axum
`/v1/results` route returning JSON headers then a permanently pending body.
Execute one `observe_failure` action through `process_http_message`, with a
2s outer guard. This timeout occurs before the first heartbeat. Expected
terminal cleanup assertion checks workspace removal, journal removal after
successful destroy, and destroy count1. Actual output:

```text
result_error=true workspace_exists=true nonterminal_record=true destroy_calls=0
test connector::tests::review_result_timeout_must_clean_workspace_and_journal ... FAILED
0 passed; 1 failed; 82 filtered out
```

Command used (temporary copy path resolved locally):

```bash
CARGO_TARGET_DIR=/tmp/ignis-source-recovery/controller/target cargo test --manifest-path <isolated-copy>/controller/Cargo.toml --locked --offline review_result_timeout_must_clean_workspace_and_journal -- --nocapture
```

### Terminal replay reproduction

Using existing dispatcher test helpers in an owned temporary RunStore: intake a
tenant job, claim as one owner, submit a valid failed create result to reach
terminal, discard that first response, repeat the exact result, then poll as
the same owner. Observed without changing dispatcher source:

```text
First result kinds: ['terminal']
Duplicate result: {'messages': [], 'idempotent_replay': True}
Subsequent poll: {'messages': [], 'pending': False}
```

### Commands and evidence

Full-suite commands match [the earlier verification record](restore-13-17-review-2026-10-19.md).
Fresh local logs: `/tmp/restore-recheck-agent.log`, `restore-recheck-dispatch.log`,
`restore-recheck-rust.log`, `restore-recheck-contracts.log`,
`restore-recheck-harness.log`, `restore-recheck-clone.log`,
`restore-recheck-e2e.log`, `restore-recheck-clippy.log`, and
`restore-recheck-targeted.log` (all under `/tmp`). These logs are local evidence,
not durable hosted artifacts; the sanitized failing observations are preserved
above. No credentials or raw wire payloads are included in this report.

Hosted kind/denial/platform/authenticated-clone qualification and released
contract/pin alignment were not run. mypy/ruff/PowerShell remain unavailable.
A passing mock E2E run does not qualify those paths. Main and remote branches
were not changed during this recheck; only these local review documents changed.
