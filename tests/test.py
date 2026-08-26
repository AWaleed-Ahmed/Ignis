#!/usr/bin/env python3
"""Raphael sandbox manual test runner.

Run from anywhere:

  python3 sandbox/tests/test.py
  python3 sandbox/tests/test.py --list
  python3 sandbox/tests/test.py health
  python3 sandbox/tests/test.py create deploy observe
  python3 sandbox/tests/test.py pipeline --failfast

Requires the controller to be running, e.g.:

  cd sandbox/controller
  RAPHAEL_CLUSTER_BACKEND=mock RAPHAEL_LISTEN=127.0.0.1:8090 cargo run

Optional:

  export RAPHAEL_SANDBOX_URL=http://127.0.0.1:8090
"""

from __future__ import annotations

import argparse
import importlib
import sys
import time
import traceback
from dataclasses import dataclass
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
if str(TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(TESTS_DIR))

# Feature modules in run order (phases / features).
FEATURE_MODULES = [
    ("health", "features.health", "Phase 0 — controller alive"),
    ("create", "features.create", "Phase 2 — create_sandbox"),
    ("destroy", "features.destroy", "Phase 2 — destroy_sandbox + stress"),
    ("deploy", "features.deploy", "Phase 3 — deploy_revision (YAML)"),
    ("observe", "features.observe", "Phase 3 — observe_failure signatures"),
    ("validate", "features.validate", "Phase 4 — run_validation"),
    ("finalize", "features.finalize", "finalize_result + GET result"),
    ("helm", "features.helm", "Phase 5 — Helm renderer"),
    ("kustomize", "features.kustomize", "Phase 7 — Kustomize renderer"),
    ("policy", "features.policy", "Phase 6 — policy + fidelity"),
    ("p0", "features.p0", "P0 — clone-at-SHA, fixtures, artifacts"),
    ("p1", "features.p1", "P1 — scenarios, fidelity claim, HTTP health"),
    ("p2", "features.p2", "P2 — persistence, admin cleanup, stress"),
    ("pipeline", "features.pipeline", "End-to-end + stress / break attempts"),
]


@dataclass
class CaseResult:
    feature: str
    name: str
    description: str
    ok: bool
    seconds: float
    error: str | None = None


def load_feature(mod_path: str):
    return importlib.import_module(mod_path)


def list_tests() -> None:
    print("Available features (run with: python test.py <feature> ...)\n")
    for key, mod_path, title in FEATURE_MODULES:
        mod = load_feature(mod_path)
        print(f"  {key:12}  {title}")
        for name, _fn, desc in mod.TESTS:
            print(f"               - {name}: {desc}")
        print()


def selected_features(names: list[str] | None) -> list[tuple[str, str, str]]:
    if not names:
        return FEATURE_MODULES
    wanted = {n.lower() for n in names}
    known = {k for k, _, _ in FEATURE_MODULES}
    unknown = wanted - known
    if unknown:
        raise SystemExit(
            f"Unknown feature(s): {', '.join(sorted(unknown))}\n"
            f"Known: {', '.join(k for k, _, _ in FEATURE_MODULES)}"
        )
    return [row for row in FEATURE_MODULES if row[0] in wanted]


def run_case(feature: str, name: str, fn, description: str) -> CaseResult:
    start = time.perf_counter()
    try:
        fn()
        return CaseResult(feature, name, description, True, time.perf_counter() - start)
    except Exception as exc:  # noqa: BLE001
        tb = traceback.format_exc()
        return CaseResult(
            feature,
            name,
            description,
            False,
            time.perf_counter() - start,
            error=f"{exc}\n{tb}",
        )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Raphael sandbox feature test runner")
    parser.add_argument(
        "features",
        nargs="*",
        help="Feature names to run (default: all). Use --list to see names.",
    )
    parser.add_argument("--list", action="store_true", help="List features and cases")
    parser.add_argument(
        "--failfast",
        action="store_true",
        help="Stop on first failing test",
    )
    parser.add_argument(
        "-k",
        "--keyword",
        default="",
        help="Only run cases whose name contains this substring",
    )
    args = parser.parse_args(argv)

    if args.list:
        list_tests()
        return 0

    # Ensure controller is up before doing a lot of work.
    from common import require_controller

    client = require_controller()
    print(f"Controller OK at {client.base_url}\n")

    results: list[CaseResult] = []
    for key, mod_path, title in selected_features(args.features):
        mod = load_feature(mod_path)
        print(f"=== {key}: {title} ===")
        for name, fn, desc in mod.TESTS:
            if args.keyword and args.keyword not in name:
                continue
            print(f"  → {name} — {desc} ... ", end="", flush=True)
            result = run_case(key, name, fn, desc)
            results.append(result)
            if result.ok:
                print(f"PASS ({result.seconds:.2f}s)")
            else:
                print(f"FAIL ({result.seconds:.2f}s)")
                print("----- error -----")
                print(result.error)
                print("-----------------")
                if args.failfast:
                    break
        if args.failfast and results and not results[-1].ok:
            break
        print()

    passed = sum(1 for r in results if r.ok)
    failed = sum(1 for r in results if not r.ok)
    print("=" * 60)
    print(f"Results: {passed} passed, {failed} failed, {len(results)} total")
    if failed:
        print("\nFailed cases:")
        for r in results:
            if not r.ok:
                print(f"  - {r.feature}/{r.name}: {r.description}")
        return 1
    print("All selected tests passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
