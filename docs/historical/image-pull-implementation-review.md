# Ignis #11 implementation review

Date: 2026-10-06. Branch: `codex/ignis-image-pull-evidence`.
Base: reviewed #17 branch at `2ffb319`; #17 remains incomplete.
Status: implementation present, behavioral qualification pending. Do not close #11 yet.

## Small commits

| Commit | Implementation |
| --- | --- |
| `43b8ae1` | Explicit runtime/mock observation source; live image evidence precedes static image sentinels. |
| `f9fbbed` | Regular/init states, original spec image, Pod UID/time, namespace-scoped UID-verified ownership, one observation deadline. |
| `0cc1d76` | Conservative runtime cause classifier, deterministic target-based fingerprints, bounded/redacted excerpts. |
| `92a65ea` | Incomplete/truncated waiting evidence remains unknown; incomplete event evidence is excluded. |
| `d7b0939` | Event image uniqueness uses the complete regular/init Pod specification, including containers without status. |

Paired Raphael implementation: `7ec97c3`, `0163be3`, `04f87c2` on `codex/raphael-image-pull-evidence`.

## Semantics

- Existing classes: confirmed absence → bad_image_reference; explicit denial → auth_denied; DNS/TLS/connectivity → network_error; throttling → dependency_timeout; ambiguous/conflicting/backoff-only evidence → unknown.
- Scalar attributes: image, image_pull_cause, evidence_source, owner_verified, container_type.
- Mock fixtures retain static sentinel behavior and declare mock_fixture. Real observation never uses image-name sentinels as image absence evidence.
- The classifier interprets kubelet/runtime messages. This is not an independent authenticated registry verification and cannot establish whether a registry conceals a private image.
- Canonical runtime key is image_pull followed by a JSON tuple of kind, workload name, container type/name, image and cause. Verified Deployment keys exclude Pod names/timestamps. Multiple candidates sort lexically by this key; the first is selected. Unverified ownership keeps Pod identity.
- Owners require Pod → ReplicaSet → Deployment controlling references with matching UIDs and namespace-scoped retrieved objects. Optional ownership lookup has a five-second cap inside the remaining observation budget. Failure leaves owner_verified=false.
- Events supplement only the matching Pod UID/name, a quoted current spec image, unique image-to-container attribution, and timestamps after Pod creation and within five minutes. Stale/incomplete/uncorrelated events are excluded. Expanded runtime image strings not matching the original spec reference may therefore yield unknown; safety takes precedence over inferred attribution.
- Runtime waiting evidence over 2048 characters is unknown. Events/excerpts are bounded and URL/credential patterns redacted. Evidence refs are capped at eight events/four Pods.
- Init failures are observed, but automatic init-container image repair is deliberately unsupported.
- No new schema enum or top-level field was added. Attribute semantics require paired consumer rollout; older consumers can misinterpret generic pull text.

## Checks actually executed

| Command | Result |
| --- | --- |
| cargo fmt --manifest-path controller/Cargo.toml | Completed after every code step |
| cargo check --manifest-path controller/Cargo.toml --all-targets --offline (shared CARGO_TARGET_DIR) | Passed through d7b0939, including existing test-target compilation |
| git diff --check | Passed |

No tests were added or executed. Test compilation is not a test pass. No kind/registry proof, hosted Actions, push, PR, release, merge or issue closure occurred.

## Pending behavioral qualification

| Case | Expected | Actual |
| --- | --- | --- |
| Arbitrary nonexistent tag without sentinel | not_found from explicit current runtime evidence | Not run |
| Healthy image with sentinel-like name | No static bad-image classification on runtime | Not run |
| Denied pull/private ambiguous absence | auth or unknown; zero patches | Not run |
| DNS/TLS/connection/429 | Separate cause; zero patches | Not run |
| Backoff only/conflicting/truncated evidence | unknown; zero patches | Not run |
| Recreated Pod/stale event/owner UID mismatch | Stable verified Deployment key or explicit uncertain target | Not run |
| Multiple containers/Deployments/shared images | Exact attribution, deterministic selection, no guessed target | Not run |
| Init failure | Accurate detection; repair refusal | Not run |
| Missing owner RBAC/deadline expiry | Preserve available pull evidence; unverified repair target | Not run |
| Mock/provenance/Secret regressions | Preserve deterministic approved controls and refuse unsafe repairs | Not run |

Before readiness: add/run the planned deterministic and owned registry/kind controls when requested, refresh legacy image fixtures for the new explicit evidence requirements, run paired regressions and document exact candidate SHAs. Existing legacy automatic image-repair fixtures are intentionally no longer accepted without verified evidence. Coordinate consumer rollout and finish #17 lease/progress work separately.


## Follow-up review fixes and PR qualification

The user requested fixes and a GitHub PR with CI verification. Three findings are addressed:

- `ca649dc`: preserve status/spec image identity; mismatched or missing status image makes the pull cause unknown. Normalize explicit Docker Hub expansion only; do not equate different tags/digests.
- `10edf6b`: one observation deadline starts before sandbox-lock acquisition and passes through Kubernetes queries and optional logs. Skip logs after exhaustion. Subprocesses use remaining time, with Unicode-safe byte truncation for log output. Synchronous filesystem/report work remains cooperative, as documented in #17.
- Paired Raphael `0c99428`: select verified structured image diagnoses directly, without requiring matching log text.
- `b71e781`: four Rust regression tests cover stale image identity, Docker Hub expansion, cause/conflict cases and truncated evidence. The older terminal-cleanup fixture now accepts jobs through the real recovery journal path.

Final local command: `CARGO_TARGET_DIR=/tmp/ignis-deploy-deadline/controller/target cargo test --manifest-path controller/Cargo.toml --locked --offline`: **66 passed** (loopback HTTP fixtures required unsandboxed execution). Earlier sandboxed runs failed eight socket binds; the initial unsandboxed run exposed one journal-bypassing fixture, now corrected. No failures were suppressed.

Paired Raphael local checks: **396 agent tests passed, 4 skipped; 77 dispatch tests passed**. These are mocked/parser/service checks, not live registry or kind proof. The existing cross-repository jobs will test the exact Ignis implementation SHA `b71e781b7f93246f271bb15df8efa2c0fda8e662` for this paired PR. Contract snapshots remain at the published release; callers do not yet send the new #17 budget field.

Merge-tree comparison against fetched main is conflict-free. The Ignis PR necessarily includes the reviewed but incomplete #17 prerequisite commits, since this branch descends from them. Heartbeat/lease coordination, complete platform cancellation qualification and contract release remain outstanding. Open as draft for review; do not merge or close issues automatically. Hosted CI status will be reported separately from these local results.
