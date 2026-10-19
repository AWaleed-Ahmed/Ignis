# Secret coverage review

The controller reports bounded reference metadata in optional `fidelity.secret_coverage` format v1. It compares rendered Pod and supported Pod-template references with the successfully applied synthetic fixture inventory. Only Secret names and key names are retained in that inventory. Required uncovered references contribute a material fidelity gap; optional uncovered references do not. Missing inventory, unsupported forms, malformed references and truncation are unknown. A workload with no Secret references needs no inventory to establish reference coverage.

Coverage is bounded to 256 references / 65,536 serialized bytes, 128 applied fixture objects, and 1,024 applied key names. Duplicate fixture Secret names are rejected before application so last-write behavior cannot disagree with the saved inventory.

Review reconciled this implementation with current main (`ece2029`) and preserved per-image digest completeness and restart recovery behavior. Local verification: 62 controller tests and 18 contract tests passed. Service tests cover known-empty selection, failed fixture load, successful inventory application, registry restart and terminal inventory cleanup. Actual Kubernetes consumption is checked by the downstream disposable-kind integration; unit/mock checks alone do not establish that claim.

The schema addition requires a new contract snapshot and coordinated consumers: older closed schemas can reject the new property. The release is `contracts-v1.3.0`; no API for production Secret payloads is introduced.

Issues #11 (image classification) and #17 (overall deploy/local request deadlines) remain open and are outside this change. PR #15's schema mismatch is already solved by merged PR #14 and `contracts-v1.2.0`; changing every kubectl configuration to report `kind` would misidentify the backend and has no remaining necessary delta.
