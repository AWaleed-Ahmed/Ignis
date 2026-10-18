# Owned checkout fidelity policy (#13)

Updated 2026-10-19. Scope: Ignis-owned disposable workspaces on Linux.

## Guarantee

Both shallow exact-SHA fetch and fallback clone use the same policy before
checkout: local `core.autocrlf=false`, `core.eol=lf`, an owned empty external
attributes file, no system attributes, and empty init/clone templates. Repository
`.gitattributes` (including nested files) still controls explicit path rules.
The requested commit must be a 7–40 character hexadecimal SHA; the resolved
HEAD must match the complete supplied prefix, not merely its first seven
characters. The source repository and developer's Git configuration are not
rewritten.

Fidelity means the working tree defined by that policy, not universal identity
between blobs and checkout bytes. Explicit `eol=crlf` legitimately produces
CRLF checkout text from normalized LF blobs. Unattributed CRLF-authored blobs
remain CRLF with the default conversion disabled; `-text` preserves mixed or
binary bytes. YAML render-output LF normalization remains a separate boundary.
Repository/global filters, working-tree encodings and arbitrary host hooks are
not qualified by these fixtures; do not claim byte identity for every Git
configuration. Hashes/artifacts must identify whether they describe blobs,
policy-defined checkout bytes, or normalized rendered output.

Global/system authentication and transport settings remain available: credential
helpers, HTTPS proxy and custom CA configuration are not globally blanked.
The owner supplies trusted host Git configuration. A synthetic configuration
retention test is not a successful authenticated HTTP clone proof.

## Deadlines and ownership

Sequential clone stages and fallback share one monotonic remaining deadline.
Timeout does not reset the budget for fallback. Cancellation flags stop owned
Git processes; Linux process groups terminate wrapper descendants. The temp
workspace stays guarded until its path has been journaled and registered with
the connector. Failed preparation and partial restart journals are cleaned or
retained for retry if cleanup fails; a missing workspace cannot block all
startup recovery. Neither clone path changes the requested commit to make a
patch; the CRLF repair fixture checks HEAD unchanged and exactly one insertion
and deletion in the target file.

## Local evidence and repeatable runner

```bash
cargo test --manifest-path controller/Cargo.toml --locked --offline
python harness/verify_clone_policy.py
```

The matrix runner builds the library test binary, then runs the same real Git
fixture under **36 owned preference combinations**:

- `core.autocrlf`: false / input / true;
- `core.eol`: lf / crlf / native;
- conflicting global attributes: absent / present;
- hostile template containing `info/attributes`: absent / present.

Every case exercises shallow and forced fallback paths, LF, explicit CRLF,
unattributed CRLF-authored blobs, mixed `-text`, nested LF attributes, binary/NUL,
Unicode, missing final newline, unchanged HEAD and minimal patch output. The
runner changes only subprocess environment and files in owned temporary paths.

**Not yet qualified:** authenticated helper-based HTTP cloning, owned system
attribute fixture, native Windows, hosted CRLF/render matrix. The initial
supported execution boundary is Linux; native Windows process-tree support
must not be inferred from compilation or Ubuntu's `core.eol=native` case.
#13 remains open until the remaining agreed qualification/release gates pass.
