# Completion checklist: Ignis #11, #13 and #17

Updated: **2026-10-19**. Status: **source recovery implemented and locally verified; release qualification remains**.

This is the shared delivery checklist for:

- [Ignis #11: trustworthy image-pull detection](https://github.com/AWaleed-Ahmed/Ignis/issues/11)
- [Ignis #13: clone line-ending policy and repository fidelity](https://github.com/AWaleed-Ahmed/Ignis/issues/13)
- [Ignis #17: deployment deadlines, local HTTP bounds and lease coordination](https://github.com/AWaleed-Ahmed/Ignis/issues/17)

The same document is saved in both repositories at
`docs/ignis-11-13-17-completion-checklist.md`. Preservation branches remain `feature/ignis-11-13-17-completion`; the recovered implementation is on `fix/restore-clone-leases` in both repositories. An unchecked box means no reviewed evidence
for the delivered pair has been attached here. Existing code is not proof of
completion. The user reviews merges and releases; pushing these branches does
not authorize either.

## 0. Current recovery checkpoint

The old Git objects remain unavailable. Inspected source-edit records from local
session history allowed reconstruction on current persistent branches, followed
by new fixes and fresh tests. These are new commits, not recreated old hashes.
See the [implementation/review record](restore-13-17-review-2026-10-19.md) and
[plan](restore-13-17-plan-2026-10-18.md) for full tested identities and results.

| Repository | Recovery base | Present implementation |
| --- | --- | --- |
| Raphael | Main `903370a` | Current BYOK/#48/#49 plus persisted claims, atomic result/expiry, publication intent/total deadline, handover and redacted restart harness |
| Ignis | Preservation `9d88f47`, candidate code `c2597f7` | Existing image evidence plus owned checkout policy, confirmed-expiry heartbeat, aggregate deadlines, cancellation and partial-journal recovery |

The original seven connector JSON schemas and released contract pin remain
unchanged. Additive HTTP/lease schemas are candidate contracts; released-pair
sync, pins, hosted qualification and #11 positive repair remain open. Checked
items below refer to local evidence in the review, not an issue closure.

Recovery reference (short IDs from the previous session, not available objects):

| Repository | Missing commits / changes to restore or reimplement |
| --- | --- |
| Raphael | `d38c644`: atomic result/expiry transitions and durable publication intent; `eef8089`: deadline-bound GitHub publication; `9440eb3`: in-progress result409 handling and delayed restart harness; `5a07ec7`: additive lease snapshot/OpenAPI; `0335607`: count patch attempts by unique action ID; `682344a`: final evidence/handoff |
| Ignis | `7520494`: retain Git helper/proxy/CA settings while isolating checkout policy; `d1016a4`: confirmed-expiry heartbeat, aggregate controller deadlines, cleanup reserve, denial/cancellation tests; `c9f7957`: HTTP/OpenAPI documentation; `a11c6cf`: final verification record |

- [x] REC-01 Restore from any subsequently located bundle/checkout, or reimplement the missing work in small commits on persistent branches. Do not recreate a commit merely from its claimed hash.
- [x] REC-02 Review restored/reimplemented code against current main and #19. Preserve newer main's refusal/provenance rules and #48/#49 decisions.
- [x] REC-03 Fresh tests and exact code heads are recorded in the linked review. Earlier reported counts (dispatch85, agent397/4skip, Ignis76, contracts19, wire-counter2, paired3) are historical and not a verified result for this branch.
- [x] REC-04 Push completed implementation commits regularly; retain durable clones/bundles. Do not keep the only copy of work in `/tmp`.

Local qualification: 441 agent passes / 4 skips, 86 dispatch, 82 Rust,
19 contract tests, 31 harness/evaluator assertions, 36 checkout combinations
(both clone paths), and all three paired mock-wire scenarios. CLN-05/06 remain
open for the owned system-attribute and complete blob/hash matrix; CLN-08 needs
an authenticated HTTP clone. LSE-11/14/16/18 retain broader and hosted controls.

**2026-10-19 correction:** the recheck's timeout cleanup, terminal replay and
architecture-log findings are resolved. Fresh results are agent441/4skip,
dispatch88, Rust86, contracts19, harness31, matrix36 and paired3. LSE-09 and ALL-02
are locally satisfied; broader hosted/release controls stay open. See
[the correction record](result-delivery-fix-review-2026-10-19.md).

## 1. Shared completion rules

- [ ] ALL-01 Record full Raphael and Ignis SHAs, Git/tool versions, backend, fixture SHAs, configuration, and contract version for every qualification run.
- [x] ALL-02 Review the complete diff against current main: standards, issue acceptance, merge conflicts, contracts, and preserved safety gates. Resolve actionable findings before release.
- [x] ALL-03 Run current-head Python agent/dispatch/evaluator suites, Ignis Rust suite, contract harness and affected harness assertions. A compilation check is not a behavioral pass.
- [ ] ALL-04 Keep remote publishing in dry-run mode and use disposable, owned clusters/fixtures. Never introduce customer credentials/kubeconfig into hosted qualification.
- [ ] ALL-05 Sanitize tokens, claim credentials and sensitive workload data before uploading traces. Save expected/actual outcomes, cleanup evidence and initial failures with corrected reruns.
- [x] ALL-06 Choose and document the supported execution platforms. Linux proof does not qualify native Windows process cancellation or checkout behavior.

## 2. #11 — trustworthy image-pull evidence

The issue permits Kubernetes-observed runtime evidence. Independent authenticated
registry lookup is not automatically required; a runtime-message classifier
must not be advertised as independent registry verification.

### Implementation and focused controls

- [ ] IMG-01 Audit/review the recovered Ignis runtime classifier and paired Raphael consumer: live evidence takes precedence, mock sentinels remain explicitly mock, and image names alone cannot authorize repair.
- [ ] IMG-02 Prove an arbitrary nonexistent image/tag without sentinel words is detected from real current Pod pull failure evidence. A healthy image with a sentinel-like name must not become confirmed absence.
- [ ] IMG-03 Verify exact namespace, current Pod UID, Pod→ReplicaSet→Deployment ownership UIDs, container name/type, current spec image and event freshness. Stale image/status/event, owner lookup denial, truncated/conflicting evidence and ambiguous multi-container attribution must refuse replacement.
- [ ] IMG-04 Verify auth denial/private ambiguous absence, DNS/TLS/network errors, throttling and backoff-only evidence cause no image replacement. Label parser fixtures separately from actual registry/cluster observations.
- [ ] IMG-05 Verify unsupported init targets refuse repair. A valid repair changes only the approved regular-container target and preserves unrelated workload content.
- [ ] IMG-06 Prove confirmed absence without verified scoped provenance refuses; with a verified healthy baseline, the approved image is the only replacement. A catalog row alone is not authorization.

### Hosted acceptance

- [ ] IMG-07 Run the positive verified-provenance real-kind case against the exact reviewed Ignis candidate SHA. Current main retains this as manual-only under [Raphael #49](https://github.com/AWaleed-Ahmed/Raphael/issues/49); do not interpret a run that omits it as proof.
- [ ] IMG-08 Inspect genuine pre-fix ErrImagePull/ImagePullBackOff evidence, selected workload/container, baseline identity, target-only patch, passed validation and independently Ready post-fix Pod. Require `fix_finalized` for the authorized positive case.
- [ ] IMG-09 Run no-provenance refusal and existing probe repair, mock/eval/report-audit and Secret-fixture regressions on the same delivered pair. Record what negative controls were real Kubernetes versus deterministic fixtures.
- [ ] IMG-10 After reviewed runtime release/pin alignment, restore the positive case to required automatic checks. It must not remain optional when #11 is declared complete.

**Close #11 only when:** runtime evidence and refusal cases pass, positive
provenance repair is required and green on the released pair, and the issue
summary states the actual registry-verification limits.

## 3. #13 — clone policy and repository fidelity

### Policy and implementation

- [x] CLN-01 Record whether fidelity means blob bytes or an explicitly defined working-tree policy. Document precedence of repository `.gitattributes`, intentional CRLF, binary files, filters, encodings and content hashes. Do not promise universal byte identity from `core.autocrlf=false/core.eol=lf`.
- [x] CLN-02 Apply only owned per-command/per-clone settings before checkout in both exact-SHA shallow-fetch and fallback paths. Preserve original HEAD/source identity; never modify host global settings or the user's source workspace.
- [x] CLN-03 Isolate ambient conversion/external attributes/templates as required by that policy while retaining credential helpers, HTTPS proxy and custom CA settings. Clearing all global/system Git config is not acceptable.
- [x] CLN-04 Preserve #17's single remaining clone budget, cancellation and cleanup through fallback. Failure must not leak an unowned workspace.

### Repeatable verification matrix

- [ ] CLN-05 Exercise ambient `core.autocrlf=false,input,true` and `core.eol=lf,crlf,native`; none/conflicting global/owned system attributes; and a hostile template containing `info/attributes`. Use owned test config/container paths, not developer system files.
- [ ] CLN-06 Cover LF blobs, intentional CRLF blobs, explicit eol=lf/eol=crlf, nested attributes, mixed `-text`, Unicode, missing final newline and binary/NUL content. Assert expected checkout and blob bytes/hash under the documented policy.
- [x] CLN-07 Run each relevant case through both fetch and fallback; assert identical policy, correct commit and cleanup.
- [ ] CLN-08 Prove retained helper/proxy/CA configuration with synthetic values and a local authenticated HTTP fixture clone using synthetic credentials. Merely reading helper config does not establish authenticated cloning.
- [x] CLN-09 Apply one localized repair to an attributed CRLF file; assert only the intended line/file changes. Run render LF/wire-output assertions separately from clone fidelity.
- [ ] CLN-10 Run hosted CRLF/render/remediation regression checks plus the repeatable Linux matrix. The existing `crlf_checkouts` workflow flag exercises Ubuntu with Windows-style Git preferences, not native Windows.
- [x] CLN-11 If native Windows is supported, qualify both checkout paths and owned child/descendant termination there. Otherwise explicitly document Linux execution support and native Windows as unqualified.

**Close #13 only when:** the policy is documented, implementation follows it,
authentication is preserved, minimal-patch/fidelity matrix passes on declared
platforms, and the issue records the supported guarantee and exclusions.

## 4. #17 — bounded execution and active-lease coordination

### Restore/review the complete lifecycle

- [x] LSE-01 Establish one finite deployment budget across clone/fallback, Helm/Kustomize, apply, digest lookup, filesystem/persistence and tool-version collection. Keep `wait_seconds=0` behavior and default receipt-hash compatibility.
- [x] LSE-02 Bound local controller connection/send/body waits with compatible headroom. Enforce the deadline on the controller server too; a dropped client request does not stop server work.
- [x] LSE-03 Carry one aggregate deadline through observation, lock acquisition, tool collection, sequential validation commands/health checks and success persistence. No item gets a fresh full budget; no successful receipt/state mutation after expiry.
- [x] LSE-04 Implement authenticated persisted job/action claims, connector ownership, monotonic renewal sequence/replay behavior and immutable job/action deadlines. Deny wrong tenant/role/connector/claim/action, terminal and expired renewals without refreshing activity.
- [x] LSE-05 Heartbeat during preparation, execution and in-flight result retry independently of the job-map lock. Bound waits/HTTP/retries by the last confirmed lease expiry and absolute deadline; update expiry only after a matching valid renewal response.
- [x] LSE-06 Serialize poll/claim/renew/result/reaper transitions. Slow hooks run outside the transaction; result commit revalidates live identity and expiry, including expiry during persistence. Late results must never revive terminal work.
- [x] LSE-07 Persist publication intent before side effects; bound remote calls by deadline. An uncertain outcome must fail closed for manual reconciliation instead of automatically repeating a publish. Document that this does not roll back remote effects or guarantee exactly-once delivery.
- [x] LSE-08 Reserve explicit cleanup/reporting headroom (the lost implementation used 72s: destroy70 + transport/report2). Validate the chosen bound with the actual server serialization behavior; do not extend the absolute job budget.
- [x] LSE-09 On timeout/lease loss, stop or finitely bound owned work, kill owned processes/descendants on supported platforms, journal sandbox/workspace cleanup, handle partial creation, and retain failed cleanup for restart. Send no late result.
- [x] LSE-10 Document poll/claim, renewal and result HTTP schemas/response envelopes, status codes (including in-progress result409), deadline propagation and backward-compatible rollout in candidate OpenAPI and contract docs.

### Required behavioral evidence

- [ ] LSE-11 Test stalled Git/descendant, slow failed-fetch→fallback, Helm/Kustomize, apply, digest lookup, optional version lookup and local HTTP body/send. Assert elapsed limits, no subsequent work and owned cleanup.
- [x] LSE-12 Test sequential stalled validation commands against one shared short budget; confirm process termination and no late state write.
- [x] LSE-13 Test nearly expired resumed claims, malformed/denied renewal and transient retries bounded by confirmed expiry. Test duplicate sequences/results, wrong identities and result persistence racing with expiry.
- [ ] LSE-14 Test create succeeded but response lost, finalize→failed redeploy, concurrent deploy/validate/finalize, cleanup failure and restart. Preserve deterministic sandbox identity, receipt compatibility and ownership.
- [x] LSE-15 Test uncertain publication plus restart: no second remote mutation. Expired publication must issue no HTTP mutation; retain a clear manual-reconciliation state.
- [ ] LSE-16 Run paired HTTP renewal denial during a slow operation: no executor completion/late result, bounded server work, workspace cleanup and journal deletion only after successful cleanup.
- [x] LSE-17 Run paired restart during an operation lasting longer than its lease (for example diagnosis35s, TTL30). Show successful renewals, same sandbox after restart, eventual terminal outcome and cleanup. Count distinct action IDs, not replay deliveries, when asserting patch attempts.
- [ ] LSE-18 Include these short-lease/deadline/denial controls in hosted qualification. Existing long-lease Secret-kind proof does not establish heartbeat behavior; mock timing tests do not prove real kubectl cancellation.

**Close #17 only when:** end-to-end execution and local/server waits are bounded,
short-lease progress survives valid renewal, denial/expiry stop work safely,
restart/cleanup and late-result controls pass on the supported platforms, and
the versioned consumer/controller pair is delivered.

## 5. Contract release, CI and hosting order

This is primarily GitHub Actions qualification on disposable Ubuntu/kind
runners. A persistent public server/domain or production Kubernetes account
is not needed to complete these synthetic integration checks.

1. [x] REL-01 Restore/reimplement and review missing code; attach fresh local evidence before requesting release approval.
2. [ ] REL-02 Publish exact candidate heads and pin all three Raphael cross-repository jobs to the same reviewed Ignis SHA for qualification. Current branch-name special cases refer to an older `codex/...` branch, not these new branches. The Secret-fixture job currently hard-pins the old release; a manual override for other jobs does not update it.
3. [ ] REL-03 Verify workflow triggers. Raphael core CI runs on `feature/**` pushes and PRs to main; cross-repository E2E does not run on this feature push alone. Ignis CI runs on main pushes/PRs to main. Pushing the branches does not prove all checks ran.
4. [ ] REL-04 Run candidate cross-repo jobs: mock/wire/real-hooks/evals/report audit; real-kind remediation including the pending positive case; Secret-fixture kind. Retain `if: always()` artifact/cluster cleanup, dry-run publishing and no application publishing token.
5. [ ] REL-05 Inspect artifacts, not just green summaries. Record skipped cases explicitly, fix failures and rerun on current heads. Complete the #13 matrix and #17 timing/denial coverage rather than substituting generic integration success.
6. [ ] REL-06 Review the additive contract diff (including deployment budget and restored lease/HTTP contracts), schema snapshots, auth/status semantics and compatibility. Obtain approval for Ignis merge/release and choose an immutable version using repository conventions; never retarget an existing tag.
7. [ ] REL-07 After the approved release exists, sync Raphael using `tools/sync-sandbox-contracts.ps1`, update `CONTRACTS_VERSION` and every runtime/workflow default to that exact reviewed release. `-Check` must pass; do not bypass it or hand-edit snapshots to mask drift.
8. [ ] REL-08 Restore required positive image proof in the consumer PR, remove obsolete candidate special cases, and run current-head core/contracts/Ignis and all paired hosted jobs on the released pair. Retain manual overrides for explicit qualification.
9. [ ] REL-09 Obtain user review/merge approval. Record final post-merge CI/run URLs and immutable identities; rerun if merges or pins change relevant code.
10. [ ] REL-10 Update both implementation reviews, Raphael handoff and each issue with delivered behavior, proof links and limits. Close each issue only after its own acceptance conditions above are met.

Useful workflow inputs for an authorized manual candidate run:
`ignis_ref=<FULL_REVIEWED_SHA>`, `pending_bad_image_positive=true`, and a separate
run with `crlf_checkouts=true`. These do not replace fixing the Secret-kind pin
or making the positive case required after release. No workflow dispatch,
release, PR or merge is authorized merely by this checklist.

## 6. Evidence record template

For each checked item, link a record with:

| Field | Required content |
| --- | --- |
| Case / requirement | Checklist ID and issue ID |
| Expected / actual | Observable result, timing bounds and pass/fail/skipped/not-run |
| Identity | Full Raphael/Ignis SHAs, contract tag, fixture SHA and tool versions |
| Environment | Real kind / mock / parser fixture; platform and relevant sanitized configuration |
| Safety | Replacement/refusal outcome, exact target/patch delta, no late mutation/result |
| Ownership | Same sandbox where required; namespace/workspace/process cleanup and retained recovery state |
| Evidence | CI URL and sanitized artifact/test locations; initial failure and corrected rerun |
| Review | Reviewer/date and remaining limits |

## 7. What is outside these three completion gates

Live BYOK/provider qualification, real Supabase baseline ingestion, trained
local inference (#40), production drift inputs and the partner pilot have
their own milestones. They do not need to be completed merely to continue
these three features. Seeded baseline controls are required for #11, but must
not be described as live ingestion proof. Do not close unrelated issues based
on these tests.
