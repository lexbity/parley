"""Coverage ratchet for Parley: no regression, floor rising to 70%.

Project policy (agreed on top of the RFC-0004 conformance model, which itself
mandates requirement traceability rather than line percentages): code coverage
must never regress, and the committed floor rises to the 70% minimum as slice
code lands. The state lives in ci/coverage-baseline.toml:

    floor_target = 70.0      # the minimum the floor converges to
    [rust.floors]            # per-crate line-coverage floors, in percent
    [rust.baseline]          # per-crate coverage at last baseline commit
    [web.floors] / [web.baseline]

`--check --rust` / `--check --web` measure current coverage and fail when a
tracked unit drops below max(floor, baseline). A crate whose build produces no
measurable regions (doc-only scaffolds) is exempt until code lands. `--check`
never writes; `--update-baseline` re-measures and rewrites the baseline values
(floors are preserved), printing the deltas — commit the result deliberately.

stdlib only, so the Rust CI lane can run it without pip installs.
"""

from __future__ import annotations

import argparse
import json
import re
import tomllib
from pathlib import Path

from common import REPO_ROOT, VerificationError, fail, run

BASELINE_PATH = REPO_ROOT / "ci" / "coverage-baseline.toml"
WEB_COVERAGE_SUMMARY = REPO_ROOT / "web" / "coverage" / "coverage-summary.json"
CRATE_SRC_PATTERN = re.compile(r"/crates/([^/]+)/src/")
EPSILON = 1e-6


def measure_rust() -> dict[str, float | None]:
    out = run(["cargo", "llvm-cov", "--locked", "--workspace", "--summary-only", "--json"])
    data = json.loads(out)["data"][0]
    covered: dict[str, int] = {}
    total: dict[str, int] = {}
    for file in data.get("files", []):
        match = CRATE_SRC_PATTERN.search(file["filename"])
        if not match:
            continue
        lines = file["summary"]["lines"]
        covered[match.group(1)] = covered.get(match.group(1), 0) + lines["covered"]
        total[match.group(1)] = total.get(match.group(1), 0) + lines["count"]
    return {
        crate: (round(100.0 * covered[crate] / total[crate], 2) if total[crate] else None)
        for crate in total
    }


def measure_web() -> float:
    run(["pnpm", "--dir", "web", "exec", "vitest", "run", "--coverage"])
    if not WEB_COVERAGE_SUMMARY.exists():
        raise VerificationError(
            "web/coverage/coverage-summary.json was not produced; check the vitest "
            "coverage configuration in web/vite.config.ts"
        )
    summary = json.loads(WEB_COVERAGE_SUMMARY.read_text(encoding="utf-8"))
    return float(summary["total"]["lines"]["pct"])


def load_baseline() -> dict:
    if not BASELINE_PATH.exists():
        raise VerificationError(
            "ci/coverage-baseline.toml is missing; run "
            "`python3 ci/coverage.py --update-baseline --rust --web` to record it"
        )
    return tomllib.loads(BASELINE_PATH.read_text(encoding="utf-8"))


def check_scope(
    scope: str,
    floors: dict[str, float],
    baseline: dict[str, float],
    measured: dict[str, float | None],
    errors: list[str],
) -> None:
    tracked = sorted(set(floors) | set(baseline) | set(measured))
    for key in tracked:
        current = measured.get(key)
        if current is None:
            floor = floors.get(key, 0.0)
            note = " (floor is nonzero — land code first)" if floor > 0 else ""
            print(f"  {scope}/{key}: no measurable regions yet{note}")
            continue
        floor = floors.get(key, 0.0)
        base = baseline.get(key)
        limit = max(floor, base if base is not None else 0.0)
        if current + EPSILON < limit:
            errors.append(
                f"{scope}/{key}: current coverage {current:.2f}% is below "
                f"{limit:.2f}% (floor {floor:.2f}, baseline {base:.2f} — regression or below floor)"
            )
        else:
            print(f"  {scope}/{key}: {current:.2f}% (floor {floor:.2f}, baseline {base:.2f})")


def check(rust: bool, web: bool) -> None:
    baseline_doc = load_baseline()
    floors_target_value = float(baseline_doc.get("floor_target", 70.0))
    errors: list[str] = []

    if rust:
        rust_floors = {k: float(v) for k, v in baseline_doc.get("rust", {}).get("floors", {}).items()}
        rust_baseline = {k: float(v) for k, v in baseline_doc.get("rust", {}).get("baseline", {}).items()}
        measured = measure_rust()
        print(f"coverage ratchet — rust (floor target {floors_target_value:.0f}%):")
        check_scope("rust", rust_floors, rust_baseline, measured, errors)

    if web:
        web_floors = {k: float(v) for k, v in baseline_doc.get("web", {}).get("floors", {}).items()}
        web_baseline = {k: float(v) for k, v in baseline_doc.get("web", {}).get("baseline", {}).items()}
        pct = measure_web()
        print(f"coverage ratchet — web (floor target {floors_target_value:.0f}%):")
        check_scope("web", web_floors, web_baseline, {"lines": pct}, errors)

    if errors:
        fail(errors)


def update_baseline(rust: bool, web: bool) -> None:
    doc = load_baseline() if BASELINE_PATH.exists() else {"floor_target": 70.0}
    doc.setdefault("floor_target", 70.0)
    doc.setdefault("rust", {"floors": {}, "baseline": {}})
    doc.setdefault("web", {"floors": {}, "baseline": {}})

    if rust:
        measured = measure_rust()
        for crate, pct in measured.items():
            old = doc["rust"]["baseline"].get(crate)
            new = pct if pct is not None else 0.0
            doc["rust"]["baseline"][crate] = new
            doc["rust"]["floors"].setdefault(crate, 0.0)
            delta = "" if old is None else f" (was {float(old):.2f})"
            print(f"  rust/{crate}: baseline {new:.2f}%{delta}" if pct is not None
                  else f"  rust/{crate}: no measurable regions; baseline 0.0")
        for crate in list(doc["rust"]["baseline"]):
            if crate not in measured:
                doc["rust"]["baseline"][crate] = 0.0
                print(f"  rust/{crate}: no measurable regions now; baseline reset to 0.0")

    if web:
        pct = measure_web()
        old = doc["web"]["baseline"].get("lines")
        doc["web"]["baseline"]["lines"] = pct
        doc["web"]["floors"].setdefault("lines", 0.0)
        delta = "" if old is None else f" (was {float(old):.2f})"
        print(f"  web/lines: baseline {pct:.2f}%{delta}")

    text = [f"floor_target = {doc['floor_target']}", ""]
    for scope, keys in (("rust", ("floors", "baseline")), ("web", ("floors", "baseline"))):
        for key in keys:
            text.append(f"[{scope}.{key}]")
            for name in sorted(doc[scope][key]):
                text.append(f"{name} = {doc[scope][key][name]}")
            text.append("")
    BASELINE_PATH.write_text("\n".join(text).rstrip() + "\n", encoding="utf-8")
    print(f"wrote {BASELINE_PATH.relative_to(REPO_ROOT)} — commit it deliberately.")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="enforce the ratchet")
    parser.add_argument("--update-baseline", action="store_true", help="re-measure and rewrite baselines")
    parser.add_argument("--rust", action="store_true", help="cargo llvm-cov scope")
    parser.add_argument("--web", action="store_true", help="vitest coverage scope")
    args = parser.parse_args()

    if (args.check + args.update_baseline) != 1:
        parser.error("exactly one of --check, --update-baseline is required")
    if not (args.rust or args.web):
        parser.error("at least one of --rust, --web is required")

    try:
        if args.check:
            check(args.rust, args.web)
        else:
            update_baseline(args.rust, args.web)
    except VerificationError as exc:
        fail([str(exc)])


if __name__ == "__main__":
    main()
