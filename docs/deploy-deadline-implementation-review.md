# Ignis #17 implementation review

Date: 2026-10-06. Status: **in progress**, not ready to close issue #17.
Branch: `codex/ignis-deploy-deadline`, based on `176a13e`.

## Small implementation commits

| Commit | Change |
| --- | --- |
| `a44e0d7` | Bound Git and kubectl subprocesses; honor the supplied apply duration. |
| `976b515` | Add monotonic deadline helper; bound Helm/Kustomize and optional tool-version commands. |
| `0dbac04` | Add independent deployment budget and share it across clone, rendering, apply and digest collection. Clean failed clone workspaces. |
| `380f0db` | Explicit local HTTP connect/request bounds; distinguish controller timeout from lease expiry and cache create-action failures as results. |

## Current semantics

- New optional request field `deploy_timeout_seconds`: default 120, valid range 1–600 seconds. Both schema and controller enforce the range.
- Existing `wait_seconds`: valid range 0–600; remains the best-effort runtime digest wait, with zero skipping waiting. It does not determine the apply timeout.
- Git preparation uses a single 180-second budget by default. Controller-managed deployment clones share the deployment deadline instead. A fetch timeout never triggers fallback cloning with a fresh budget.
- Optional digest collection reserves two seconds for report work and preserves partial observations/material gaps. Version collection is best effort, at most five seconds total and one second per invocation.
- Deployment RPC timeout is the declared controller budget plus ten seconds; local connect timeout is five seconds. Other local requests have an explicit 300-second bound; terminal destroy uses 70 seconds. Dispatch polling/result timeout remains separate.
- Timeout outputs use action status `timeout` and error `controller_timeout`, distinct from `job_lease_expired`.
- Owned Unix subprocesses start in a separate process group. Expiry kills the group and reaps the direct child. Async cancellation kills the group and relies on Tokio's kill-on-drop/background reaping for the direct child. Windows currently kills only the direct child; descendant handling is **not qualified**.
- Filesystem and YAML processing stages check the deadline between stages/entries. An individual blocking filesystem call or YAML parse is **not interruptible**; this is not a hard real-time guarantee on stalled mounts or pathological inputs.

## Checks actually executed

| Command | Actual result |
| --- | --- |
| `cargo fmt --manifest-path controller/Cargo.toml` | Completed after each code step. |
| `cargo check --manifest-path controller/Cargo.toml --offline` | Passed after each code step. |
| `cargo check --manifest-path controller/Cargo.toml --all-targets --offline` | Passed at `380f0db` content, including compilation of existing test targets. |
| `git diff --check` | Passed for each implementation step. |

No tests were added or executed in this implementation batch. Compilation of test targets is not a test pass. No hosted Actions, kind proof, PR, contract release, push or merge was performed.

## Behavioral cases still pending

| Case | Expected behavior | Actual result |
| --- | --- | --- |
| Stalled Git, including wrapper descendant | Timeout within budget; child/descendant exits; temp workspace removed | Not run |
| Failed shallow fetch followed by slow fallback | Fallback uses remaining original budget | Not run |
| Stalled Helm lint/template or either Kustomize path | Timeout class preserved; no later apply | Not run |
| Zero digest wait | Apply succeeds under deploy budget; unresolved runtime digests remain explicit | Not run |
| Slow/stalled apply | Controller timeout; no subsequent deploy stages | Not run |
| Partial digest lookup | Retain current partial snapshot and unresolved-image gaps | Not run |
| Stalled version command | Skip unavailable version within bounded optional budget | Not run |
| Stalled controller HTTP body/send | Cached `timeout/controller_timeout` result | Not run |
| Cancellation | Owned process group exits and direct child is reaped | Not run |
| Windows wrapper descendants | All owned descendants terminate | Not implemented/qualified |

## Work remaining before #17 is complete

1. Implement authenticated current-action heartbeat and finite absolute execution/job bounds in paired Raphael changes. Reject wrong tenant/role/action, terminal and expired jobs; reject late results before refreshing lease activity.
2. Renew progress during preparation and execution independently of the connector job-map lock. Coordinate lost leases, local HTTP timeout, server-side work and cleanup; a disconnected client does not prove the server has stopped.
3. Define supported-platform cancellation guarantees, then add/run the deterministic timing and cleanup controls listed above, short-lease/replay/restart controls and cross-repository regressions when test execution is requested.
4. Review/release the additive Ignis contract and update Raphael's exact contract snapshot/runtime pins through the established rollout process. The new field must not be sent to older controllers whose contract excludes it.
5. Complete controlled kind/hosted integration evidence. Existing larger harness leases do not qualify heartbeat behavior.

#11 and #13 implementation has not started. The saved Raphael plan specifies #17 → #11 → #13. No issue is marked done by this document.
