# Issue #30 completion implementation review

Date: 2026-10-20. Raphael base f056557; Ignis base bc42504 (contracts-v1.4.1).

## Behavior and boundaries

- Complete, supported, bounded coverage with a positively missing required reference produces unresolved_secret_dependency without claiming a production-only credential requirement. Covered/optional controls continue; absent, malformed, unsupported or incomplete coverage remains unknown. Explicit textual production/privilege blocks keep precedence.
- The direct live graph obtains deployment/observation coverage before diagnosis/model refinement. Known deterministic safety blocks and diagnosis-only requests bypass this preflight. Structural terminal refusal attempts owned sandbox cleanup; failure retains the sandbox identity and a retryable cleanup error.
- One shared schema/size-checked metadata sanitizer preserves reference identities through evidence and JSON/SQLite storage. Unknown properties/value fields are rejected; ordinary sensitive fields are still redacted. Escalation next checks identify the uncovered names/keys/workloads with a bounded list.
- Ignis #20 is reconciled with current main and retains operation deadlines, locks and lease handling. Unsupported forms become incomplete/unknown; missing required references affect dependencies_available. Approved bundled fixture metadata is recomputed from the current successful deploy and combined temporarily with selected-fixture metadata, avoiding false name collisions on repeated deploys.
- No production Secret API/read is added. The approved raw-channel exceptions remain deploy rendered_files, full manifest artifacts and controller rendered_yaml; value-absence claims apply outside those channels. Synthetic sentinels are test data.

## Executed local verification

| Cases / suite | Expected | Actual |
| --- | --- | --- |
| Agent suite | Existing behavior plus metadata/precedence/live ordering/cleanup controls | 455 passed, 4 existing skips |
| Dispatch suite | Existing result/lease/persistence and structural refusal behavior | 88 passed |
| Ignis library suite | Parser, coverage, inventory, sentinel/redeploy plus existing controller/deadline/lease controls | 101 passed |
| Ignis process integration | Seven bounded-tool and child-termination controls | 1 integration passed (seven controls) |
| Workflow/eval/wire/kind/denial assertions | Required CI and runtime proof assertions retained | 47 passed |
| Owned missing fixture evaluation | Undecorated reference; structural escalation, zero patch/publish, named next checks | Passed with real agent hooks, Rust processes and mock Kubernetes |
| Owned covered fixture evaluation | Same source; covered reference permits probe-only patch and fix_finalized | Passed with real agent hooks, Rust processes and mock Kubernetes |
| Sentinel/persistence/redeploy | Values absent outside approved raw channels; JSON/SQLite reopen retains safe metadata; repeat bundled deploy stays complete | Passed in agent and Ignis suites |
| fmt / diff / actionlint | Source and workflow checks | Passed; actionlint ran without unavailable shellcheck |
| Clippy | No new warnings from these changes | Completes with five existing sites in kubectl/kustomize/validation and a service test; new bool-assert warning corrected |
| Python static tools | Explicitly report available tooling | black/ruff/mypy unavailable; no strict type/lint pass claimed |

Initial integration compilation required adapting the old PR's test request to the current numeric deployment-timeout field. Initial new eval manifest omitted required_fields; it was corrected and manifests are now schema-validated before running. No failed attempt is counted as passing evidence.

## Independent review

Standards: zero actionable findings for Raphael f056557..89c548f and Ignis bc42504..78ca9b6. Spec: zero actionable findings for the same source. These are independent source reviews, not hosted proof or release approval.

## Hosted and release acceptance

The existing cross-repo workflow now includes both additional mock evaluations and upgrades the real-kind probe case to require a covered Secret reference, a target-only probe repair, passing validation and independently Ready post-fix observation. The six existing Secret kind controls remain. Hosted run links/results and exact candidate identities are recorded in the paired PR descriptions; local mocks do not establish actual Kubernetes behavior.

Keep CONTRACTS_VERSION at released contracts-v1.4.1 until Ignis #20 is reviewed/merged and an approved immutable runtime release is available. Candidate qualification uses one explicit full Ignis SHA for every runtime job. No schema bytes changed. Do not close #30 on local tests alone; reviewed runtime delivery and released-pair hosted evidence remain requirements. Native Windows and universal credential/value detection are outside this proof.
