# Recovery implementation and verification: #13 / #17

Started 2026-10-18; updated 2026-10-19. Branch in both repositories:
`fix/restore-clone-leases`. [Recovery plan](restore-13-17-plan-2026-10-18.md).

## Recovery provenance and scope

The old Git objects/temporary checkouts remain unavailable. The desktop's local
session history retained the source-edit commands. They were inspected as
recovery data and reconstructed in isolated staging copies based on Raphael
`5a1771e` and Ignis `c2597f7`; historical shell tests, pushes and Git mutations
were not replayed. Failed intermediate patch attempts were inspected, and two
missing Rust test appends were recovered separately. Source diffs applied
cleanly to persistent branches based on current Raphael `903370a` and Ignis
preservation `9d88f47`. This is source recovery with new commits, not a claim
that the lost original commit hashes have been recreated.

Current BYOK, #48/#49 provenance/refusal decisions, workflow defaults and the
original seven connector schemas are preserved. The contract pin is still
`contracts-v1.3.0`; the new schemas/HTTP documentation are reviewed candidates,
not released contract parity. Do not bypass the existing sync check.

## Implemented slices

| Slice | Commits | Behavior |
| --- | --- | --- |
| Plan | Raphael `25c56d3`; Ignis `6ebffb3` | Assumptions, resource model, sequence, success criteria and separate release gates |
| Owned checkout | Ignis `93befb0`, `867623b` | Both clone paths share policy/deadline/cancellation; guarded ownership, retained helper/proxy/CA, exact SHA-prefix validation and byte/minimal-patch fixtures |
| Dispatch lifecycle | Raphael `5c81b83`, `f4b6316` | Persist claim owner/expiry/sequence/deadlines; authenticated renewal; lock result/poll/reaper transitions; hooks outside lock; expiry rechecked after persistence |
| Publication recovery | Raphael `8c7b47c`, `f0ad8f7` | Persist intent before hook, fail closed after uncertain outcome, cancellable whole-exchange HTTP deadline, owned async client lifetime |
| Connector/controller | Ignis `2a6a3be`, `6974cee`, `e1d65e3`, `15f58ae`, `c94de3e` | Confirmed-expiry heartbeat, aggregate server/validation/deployment-preparation deadlines, body-read cancellation, safe claim handover, partial journal cleanup, mismatch rejection |
| Harness/contracts | Raphael `9995279`, `2268439`, `f1ecbe8`; Ignis `50b0a75` | Configurable long diagnosis restart, distinct action counting, additive lease JSON schemas and HTTP/OpenAPI/denial/handover docs |

## Behavioral cases

| Case | Expected | Evidence |
| --- | --- | --- |
| Expiry during result persistence | Neither live nor persisted state revives; late result rejected | `test_expiry_during_result_persistence_cannot_revive_job` |
| Repeated result while hook runs | HTTP409, cached-result retry with heartbeat; no second execution | Dispatch concurrency tests and paired restart |
| Uncertain publication plus restart | Hook invoked once; subsequent automatic publish refused for reconciliation | `test_uncertain_publication_is_not_repeated_after_restart` |
| Expired/trickling publication | No expired request; trickling response cancelled by total deadline | `test_expired_publication_deadline_prevents_http_mutation`, `test_publication_absolute_deadline_cancels_trickling_body` |
| Multiple publication exchanges | Each async client belongs to its own event loop and is closed | `test_deadline_publication_owns_client_per_exchange` |
| Resumed claim with 80ms left | Stop within 250ms despite TTL30 | `resumed_claim_stops_at_confirmed_expiry` |
| Denial during execution or stalled result body | No late result/execution; owned workspace/journal cleaned | `renewal_denial_cancels_execution_cleans_workspace_and_posts_no_result`, `stalled_result_body_is_cancelled_on_lease_denial` |
| Result committed before HTTP response delivery | Stale renewal denied without extending expiry; correct owner adopts active next claim; delayed body cannot reset its newer sequence | Python handover test and `committed_result_adopts_next_claim_before_delayed_response` |
| Failed/crashed clone preparation | Incomplete journal cleaned; unrelated restart recovery not wedged | `failed_clone_preparation_is_cleaned_and_does_not_block_rehydration` |
| Wrong job/action claim | No fallback to unclaimed legacy execution | `mismatched_claim_never_executes_as_legacy_unclaimed_work` |
| Sequential stalled validation and server future | One aggregate budget, children killed, no late success mutation | Static validation and server deadline tests |
| Deploy lock/preparation exceeds header budget | Use action deadline rather than resetting to request-body default | `deployment_lock_wait_uses_action_deadline_not_the_body_default` |
| Global Git config retention and byte policy | Auth/transport settings retained; both paths preserve documented bytes/HEAD; one-line CRLF edit stays minimal | Git unit tests and 36-case owned matrix; see Ignis `docs/clone-fidelity-policy.md` |
| TTL30 / diagnosis35 / whole-process restart | Renewals1,2,3; same sandbox; terminal success and cleanup | Paired mock wire E2E, all three scenarios; max-attempt case observes exactly two distinct patch actions |

## Fresh verification

Tested code pair (documentation commits follow without product-code changes):

- Raphael: `f1ecbe88d5b8ee49d99255df4a2cabf5541a2a51`.
- Ignis: `c94de3e7d7a02424945f8c57eb643ce04512b63b`.
- Released contract pin: `contracts-v1.3.0`; candidate lease/HTTP additions are not released snapshots.

| Check | Actual result |
| --- | --- |
| Agent pytest | **441 passed, 4 skipped**; full suite before final harness-only changes |
| Dispatch pytest | **86 passed**; full suite before final harness-only changes |
| Rust cargo test | **82 passed**; bin/doc targets contain zero tests |
| Contract harness | **19 passed** |
| Harness/evaluator assertions | **31 passed**, including combined-launcher import and credential redaction |
| Owned Git configuration matrix | **36 combinations passed**, each checks both clone paths |
| Candidate contract comparison | **11 files identical** between the candidate pair; original seven JSON schema hashes unchanged |
| Paired mock HTTP E2E | **3 scenarios passed** on the exact pair above; repair, max-attempt escalation, restart |
| Short-lease restart | TTL30 / diagnosis35; successful renewal sequences1,2,3 (HTTP200); same sandbox; eventual terminal and clone cleanup |
| Saved trace redaction | All27 present claim-ID fields redacted; connector identity absent from this wire run and covered by the recursive redaction unit test |
| Formatting/diff | cargo fmt and git diff whitespace checks passed |
| Clippy | Completed; five existing warning sites described below |
| Unavailable/not run | mypy, ruff and PowerShell release-sync check; hosted/native-Windows/authenticated-clone qualification |

Both feature branches are retained in persistent project clones and pushed as
`fix/restore-clone-leases`. No main merge, PR, tag or hosted dispatch is part of
this checkpoint.
Toolchain: Python3.12.3, Git2.43.0, rustc1.97.1, Cargo1.97.1; Linux.
Fixture default: `AmazingDude/raphael-e2e-fixture@57f0801fe46527c7531d62c5e278db80b7b56564`.

Commands (use the existing project Python environment):

```bash
PYTHONPATH=agent:dispatch python -m pytest agent/tests -q
PYTHONPATH=dispatch:agent python -m pytest dispatch/tests -q
PYTHONPATH=agent:dispatch python -m unittest e2e.test_wire_harness evals.test_run_evals e2e.test_kind_verification
cargo test --manifest-path controller/Cargo.toml --locked --offline
cargo fmt --manifest-path controller/Cargo.toml --check
cargo clippy --manifest-path controller/Cargo.toml --locked --offline --all-targets --all-features
python -m pytest harness/tests/test_contracts.py -q
python harness/verify_clone_policy.py
E2E_LEASE_TTL_SECONDS=30 E2E_DIAGNOSE_DELAY_SECONDS=35 E2E_IGNIS_BIN=<built-candidate-binary> python e2e/run_e2e.py
```

Run agent tests separately from the harness: its auto-detection tests otherwise
see the harness's local mock controller as a live backend. One concurrent run
produced two such environment failures; the serialized rerun passed. The first
mixed agent/dispatch focused invocation also hit duplicate `tests` package
collection; suites were rerun independently. Initial Rust fixture syntax and
serialization errors were corrected; a new handover assertion caught a real
renewal-sequence snapshot bug, fixed before its rerun passed.

Clippy completes with five existing warning sites: manual Option::map, two
needless returns, validation argument count (already above threshold before
this delivery), and a redundant test closure. Those unrelated style changes
were not swept into the recovery. Python emits existing Starlette/AnyIO and
jsonschema.RefResolver deprecation warnings. OpenAPI YAML plus representative
positive/negative response shapes were checked; no full Redocly lint claim.
All three launchers redact claim/connector credentials before storing trace
bodies, leaving HTTP payloads unchanged. The obsolete combined-launcher claim
cache was removed and its import tested. Malformed non-JSON bodies are omitted.
The final saved trace passed recursive redaction inspection; old unredacted
traces were local/ignored and not committed. An initial inspection incorrectly
required a connector-ID field to exist on the wire; the corrected check asserts
redaction for every credential field actually present, while the unit test
covers both field names. Only sanitized outcome/sequence summaries appear here.

## Standards review

Initial independent standards review found stale-heartbeat destructive cleanup
and the insufficient inactivity-only publication bound. The follow-up found
cross-event-loop reuse of an injected async client. All three were addressed;
the final focused read-only recheck reports **zero remaining findings**.

## Spec review

Initial independent spec review found result-body waits outside lease-loss
cancellation, publication without a total deadline, and incomplete clone
journals blocking startup. All were addressed; the final read-only recheck
reports **zero remaining findings**. Extra final hardening passes the action
header deadline into synchronous deployment preparation/lock bounds and rejects
mismatched claims before any legacy fallback.

Review summary: Standards0 remaining; Spec0 remaining. This does not substitute
for the open release qualification gates below.

## Limits and remaining gates

- Publication uncertainty requires manual reconciliation, not automatic remote
  replay or a rollback/exactly-once claim. Synchronous publishing callers use
  a worker thread; deadline-bound requests own their HTTP clients internally.
- Cleanup is bounded and journaled on failure; an unavailable controller can
  prevent successful cleanup. Distinct connectors need distinct identities;
  shared bearer credentials do not identify independent workers.
- Missing claims retain a bounded legacy compatibility path; malformed or
  mismatched supplied claims fail closed. New heartbeat behavior requires the
  paired compatible dispatcher/controller, not old released binaries.
- #13: authenticated HTTP helper fixture, owned system-attribute test, hosted
  CRLF/render qualification and any native Windows claim remain outstanding.
  The Linux preference matrix is not a native Windows qualification.
- #17: current-pair hosted deadline/denial/short-lease plus relevant real-kind
  controls and the reviewed contract release/snapshot/runtime rollout remain.
- #11 positive image repair and the separate Secret-safety delivery keep their
  own acceptance gates. Neither issue is closed by mock wire success.
- Main branches, tags, release artifacts and workflow dispatch remain unchanged
  by this recovery turn. Recovery feature branches are durable review work.
