# HTTPS security maintenance validation — 2026-10-25

Candidate branch: `fix/https-security-2026-10-25`, based on Ignis main
`f7af9ce57387169084eebf38699a624adba74a54`. Results are local; no release tag
or existing installed controller was replaced.

## Targeted dependency changes

| Dependency | Before | After | Reason |
| --- | --- | --- | --- |
| webpki-roots | 1.0.9 | 1.0.10 | Remove the root allowing compromised emSign intermediate chains |
| rustls-webpki | 0.103.15 | 0.103.16 | Reject compromised intermediate public keys |
| tokio | 1.53.1 | 1.53.3 | Patched AsyncFd maintenance release |
| rustls | 0.23.43 | 0.23.46 | October 14 TLS security fixes |

Only these four package versions/checksums changed in Cargo.lock. The direct
Tokio minimum was raised to 1.53.3. No application Rust code, public contract,
runtime flags or TLS verification behavior was changed. The lockfile remains
the reproducible source of transitive versions; build with `--locked`.

The optional Python test-client guide now bootstraps checksum-pinned Pip 26.2.2
and requires Certifi 2026.10.19 or newer. Its bootstrap file matches the one
successfully installed and validated for Raphael.

## Executed checks

From `controller/`:

| Command/check | Result |
| --- | --- |
| Certificate-only `cargo test --locked --lib connector::tests` before the Tokio/Rustls update | 31 passed |
| `cargo fmt --all -- --check` | Passed |
| `cargo check --locked` | Passed |
| `cargo test --locked` with all four updates | 101 unit tests and 1 integration test passed; 0 ignored |
| `cargo clippy --locked --all-targets --all-features` | Exit 0; five existing warnings in unchanged source |
| `cargo build --release --locked` | Passed; optimized binary rebuilt |
| Optimized binary, temporary state, loopback mock `/health` | HTTP 200, `status=ok`; owned process stopped and temporary state removed |
| Locked-version comparison against base | Only the four intended dependencies changed |
| `cargo tree --locked -e features -i webpki-roots` and `-i rustls-webpki` | Patched versions are used by Reqwest's active Rustls connector path |
| `git diff --check` | Passed |

Clippy warnings are `manual_map` in `src/k8s/kubectl.rs`, two `needless_return`
warnings in `src/render/kustomize.rs`, `too_many_arguments` in
`src/validate/mod.rs`, and a test `redundant_closure` in `src/domain/service.rs`.
These files are untouched; unrelated lint cleanup is outside this security fix.

Initial restricted connector tests could not bind loopback servers. The rerun
with local socket access passed all 31 tests. The full suite also ran with
loopback access; no tests were disabled to obtain a pass.

The first optimized-binary health attempt was not executed because the automatic
approval service reached its usage limit. After the user requested continuation,
the normal approval path succeeded and the health smoke passed.

## Release and proof limits

- The patched lockfile and passing tests do not patch binaries already installed
  by users. A reviewed rebuild/release, restart and paired verification remain
  necessary. No tag, push, merge or deployment was performed here.
- No live production endpoint, customer kubeconfig or compromised-certificate
  exploit was used. Verification covers locked dependencies, active TLS feature
  wiring and existing connector/controller regressions.
- Tokio 1.53.3 avoids the separately disclosed interval race in 1.54.0. Do not
  substitute that yanked release when regenerating dependencies.

## Upstream references

- [Root-store advisory](https://rustsec.org/advisories/RUSTSEC-2026-0381.html)
- [Certificate-verifier advisory](https://rustsec.org/advisories/RUSTSEC-2026-0382.html)
- [Tokio AsyncFd advisory](https://rustsec.org/advisories/RUSTSEC-2026-0386.html)
- [Tokio 1.54.0 interval advisory](https://rustsec.org/advisories/RUSTSEC-2026-0384.html)
- [Rustls 0.23.46 release](https://github.com/rustls/rustls/releases/tag/v%2F0.23.46)
- [Python tooling trust-store guidance](https://discuss.python.org/t/information-for-python-users-regarding-emudhra-emsign-ca-certificate-security-incident/109576/1)
