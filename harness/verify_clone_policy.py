"""Run the owned Linux Git-preference matrix against the real clone fixture."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
CASE = "gitclone::tests::checkout_policy_preserves_repository_attributes_and_binary_bytes"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    results = []
    build = subprocess.run(
        ["cargo", "test", "--manifest-path", str(ROOT / "controller/Cargo.toml"),
         "--locked", "--offline", "--lib", "--no-run", "--message-format=json"],
        check=True, capture_output=True, text=True,
    )
    binaries = [item["executable"] for line in build.stdout.splitlines()
                if (item := json.loads(line)).get("reason") == "compiler-artifact"
                and item.get("executable") and "lib" in item.get("target", {}).get("kind", [])]
    if len(binaries) != 1:
        raise RuntimeError("expected one library test binary")
    count = 0
    with tempfile.TemporaryDirectory(prefix="ignis-clone-policy-") as temporary:
        directory = Path(temporary)
        attributes = directory / "attributes"
        attributes.write_text("plain.txt text eol=crlf\n", encoding="utf-8")
        template = directory / "template"
        (template / "info").mkdir(parents=True)
        (template / "info/attributes").write_text("plain.txt text eol=crlf\n", encoding="utf-8")
        config = directory / "gitconfig"
        for autocrlf in ("false", "input", "true"):
            for eol in ("lf", "crlf", "native"):
                for external_attributes in (False, True):
                    for external_template in (False, True):
                        config.write_text(
                            f"[core]\n autocrlf = {autocrlf}\n eol = {eol}\n"
                            + (f" attributesFile = {attributes}\n" if external_attributes else ""),
                            encoding="utf-8",
                        )
                        env = dict(os.environ, GIT_CONFIG_GLOBAL=str(config))
                        if external_template:
                            env["GIT_TEMPLATE_DIR"] = str(template)
                        else:
                            env.pop("GIT_TEMPLATE_DIR", None)
                        run = subprocess.run([binaries[0], "--exact", CASE], env=env,
                                             capture_output=True, text=True)
                        if run.returncode:
                            print(f"FAIL autocrlf={autocrlf} eol={eol} attributes={external_attributes} template={external_template}")
                            print(run.stdout)
                            print(run.stderr)
                            return 1
                        results.append({"autocrlf": autocrlf, "eol": eol,
                                        "global_attributes": external_attributes,
                                        "hostile_template": external_template,
                                        "system_attributes": os.getenv("IGNIS_SYSTEM_ATTRIBUTES_CASE", "not-qualified"),
                                        "clone_paths": ["shallow", "fallback"], "files_with_byte_and_sha256_checks": 8,
                                        "passed": True})
                        count += 1
    print(f"PASS {count} owned preference combinations; both clone paths and minimal patch checked per case")
    auth = subprocess.run([binaries[0], "--exact", "gitclone::tests::authenticated_http_clone_uses_retained_credential_helper_in_both_paths"],
                          capture_output=True, text=True)
    if auth.returncode:
        print(auth.stdout)
        print(auth.stderr)
        return 1
    print("PASS synthetic credential-helper smart-HTTP clone; shallow and fallback independently checked")
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps({"cases": results, "passed": True,
                                          "authenticated_http_clone": True}, indent=2) + "\n")
    print("Native Windows remains unqualified; system-attribute evidence requires the owned container job")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
