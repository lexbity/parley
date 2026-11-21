"""Traceability and boundary verifier for Parley (RFC-0004 INT-08, slice gates).

Two modes:

`--through SLICE [--check-manifest]`
  * verifies the coverage manifest (contract/coverage.yaml) declares rows for
    every slice up to SLICE and that requirement IDs and evidence artifacts
    exist,
  * runs the import/dependency gates day one: the domain crate must stay pure
    (no HTTP, SQL, filesystem, process or model imports), feature components
    must not call transport APIs directly, and production sources must not
    import the mock or fixtures (RFC-0004: "ci/trace.py also rejects
    production imports of test fixtures, direct API fetches in feature
    components and SQLx/HTTP/model imports in the domain crate"),
  * with --check-manifest, reads the actual Vite build manifest from
    web/dist/.vite/manifest.json, verifies every referenced asset exists and
    records asset hashes for the later embedded-bundle proof.

`--slice SLICE --require-executed`
  * verifies the fresh cargo test log for tests/SLICE.rs ran positive executed
    tests with zero failed/ignored/filtered (a filter that succeeds on zero
    matches never counts),
  * verifies the fresh Playwright JSON report executed the slice spec on both
    the mock and rust projects with positive expected results and zero
    skipped/unexpected/flaky,
  * cross-checks every coverage row's test ID against the executed test names.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path
from typing import Any

from common import (
    REPO_ROOT,
    VerificationError,
    coverage_rows,
    fail,
    load_coverage,
    run_rg,
    slices_through,
)

ARTIFACTS_DIR = REPO_ROOT / "artifacts"
DIST_DIR = REPO_ROOT / "web" / "dist"
CARGO_LOG_PATH = ARTIFACTS_DIR / "cargo-test-{slice}.log"
PLAYWRIGHT_JSON_PATH = ARTIFACTS_DIR / "playwright-{slice}.json"

# Import gates (RFC-0004). Patterns are exact regexes over the named scopes;
# behavioral correctness is owned by the test suites, these catch regressions.
DOMAIN_IMPURE_PATTERN = (
    r"^\s*use (sqlx|reqwest|axum|tokio|ureq|hyper|ollama)::|"
    r"^\s*use std::(net|fs|process|io)|"
    r"extern crate (sqlx|reqwest|axum|tokio|ureq|hyper|ollama)|"
    r"std::(net|fs|process)::"
)
FEATURES_TRANSPORT_PATTERN = (
    r"\bfetch\(|XMLHttpRequest|new WebSocket|navigator\.sendBeacon|"
    r"from ['\"][^'\"]*(/mock/|/fixtures/)"
)
PRODUCTION_MOCK_IMPORT_PATTERN = r"from ['\"][^'\"]*(/mock/|/fixtures/|@mock)"

DOMAIN_SRC = REPO_ROOT / "crates" / "parley-domain" / "src"
FEATURES_SRC = REPO_ROOT / "web" / "src" / "features"
PROD_SRC = REPO_ROOT / "web" / "src"


def check_import_gates() -> None:
    errors: list[str] = []
    if DOMAIN_SRC.is_dir():
        for line in run_rg(DOMAIN_IMPURE_PATTERN, [DOMAIN_SRC]):
            errors.append(f"domain purity violation: {line}")
    if FEATURES_SRC.is_dir():
        for line in run_rg(FEATURES_TRANSPORT_PATTERN, [FEATURES_SRC]):
            errors.append(f"feature component transport violation: {line}")
    if PROD_SRC.is_dir():
        for line in run_rg(PRODUCTION_MOCK_IMPORT_PATTERN, [PROD_SRC]):
            errors.append(f"production import of mock/fixtures: {line}")
    if errors:
        fail(errors)
    print("import gates pass (domain purity, feature transport, production closure).")


def check_coverage_through(through: str) -> None:
    coverage = load_coverage()
    errors: list[str] = []
    for slice_id in slices_through(through):
        rows = coverage_rows(coverage, slice_id)
        if not rows:
            errors.append(
                f"slice {slice_id} declares no coverage rows; the release trace enumerates "
                "every requirement individually (RFC-0004 INT-08)"
            )
            continue
        for index, row in enumerate(rows):
            requirement = str(row.get("requirement", ""))
            if not requirement:
                errors.append(f"coverage row {index} of {slice_id} has no requirement ID")
                continue
            evidence = row.get("evidence_path")
            if not evidence or not (REPO_ROOT / str(evidence)).exists():
                errors.append(
                    f"{slice_id} row {index} ({requirement}): evidence artifact "
                    f"{evidence!r} is missing"
                )
    if errors:
        fail(errors)
    print(f"coverage manifest verified through {through}.")


def check_vite_manifest() -> None:
    manifest_path = DIST_DIR / ".vite" / "manifest.json"
    if not manifest_path.exists():
        fail(
            [
                "web/dist/.vite/manifest.json is missing; run `pnpm --dir web exec vite build` "
                "before checking the package manifest"
            ]
        )
    manifest: dict[str, Any] = json.loads(manifest_path.read_text(encoding="utf-8"))
    if not manifest:
        fail(["web/dist/.vite/manifest.json is empty"])
    if not (DIST_DIR / "index.html").exists():
        fail(["web/dist/index.html is missing; the production bundle is incomplete"])
    hashes: dict[str, str] = {}
    errors: list[str] = []
    for key, entry in manifest.items():
        file = entry.get("file")
        if not file:
            errors.append(f"manifest entry {key!r} declares no file")
            continue
        asset = DIST_DIR / file
        if not asset.exists():
            errors.append(f"manifest entry {key!r} references missing file {file}")
            continue
        hashes[file] = hashlib.sha256(asset.read_bytes()).hexdigest()
    ARTIFACTS_DIR.mkdir(parents=True, exist_ok=True)
    (ARTIFACTS_DIR / "vite-asset-hashes.json").write_text(
        json.dumps(hashes, indent=2, sort_keys=True), encoding="utf-8"
    )
    if errors:
        fail(errors)
    print(
        f"Vite manifest verified: {len(hashes)} asset(s) present, hashes recorded to "
        "artifacts/vite-asset-hashes.json for the embedded-bundle proof."
    )


TEST_RESULT_PATTERN = re.compile(
    r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
    r"(\d+) measured; (\d+) filtered out"
)


def check_cargo_log(slice_id: str, log_path: Path) -> int:
    if not log_path.exists():
        fail([f"cargo test log {log_path} is missing; run scripts/prove {slice_id} first"])
    log = log_path.read_text(encoding="utf-8")
    errors: list[str] = []
    if f"Running tests/{slice_id}.rs" not in log:
        errors.append(
            f"cargo log does not contain 'Running tests/{slice_id}.rs'; the log is not the "
            f"fresh result of cargo test --test {slice_id}"
        )
    matches = TEST_RESULT_PATTERN.findall(log)
    if not matches:
        errors.append("cargo log contains no 'test result:' line; zero executed suites")
    passed = 0
    for status, p, f, ignored, _measured, filtered in matches:
        passed += int(p)
        if status != "ok":
            errors.append(f"a cargo test suite reported {status}")
        if int(f):
            errors.append(f"cargo suite reported {f} failed test(s)")
        if int(ignored):
            errors.append(f"cargo suite reported {ignored} ignored test(s); ignored never counts")
        if int(filtered):
            errors.append(
                f"cargo suite reported {filtered} filtered test(s); a filter that hides "
                "missing tests must never succeed"
            )
    if passed == 0:
        errors.append("cargo executed zero passing tests for this slice")
    if errors:
        fail(errors)
    return passed


def _walk_playwright_suites(suite: dict[str, Any], found: list[dict[str, Any]]) -> None:
    for spec in suite.get("specs", []):
        found.append(spec)
    for child in suite.get("suites", []):
        _walk_playwright_suites(child, found)


def check_playwright_report(slice_id: str, report_path: Path) -> int:
    if not report_path.exists():
        fail([f"playwright report {report_path} is missing; run scripts/prove {slice_id} first"])
    report: dict[str, Any] = json.loads(report_path.read_text(encoding="utf-8"))
    stats = report.get("stats", {})
    errors: list[str] = []
    if int(stats.get("unexpected", 0)) or int(stats.get("skipped", 0)) or int(
        stats.get("flaky", 0)
    ):
        errors.append(
            f"playwright stats are not clean: {stats}; skipped/unexpected/flaky never count"
        )
    specs: list[dict[str, Any]] = []
    for suite in report.get("suites", []):
        _walk_playwright_suites(suite, specs)
    spec_file = f"e2e/{slice_id}.spec.ts"
    slice_specs = [spec for spec in specs if spec.get("file") == spec_file]
    if not slice_specs:
        errors.append(f"report contains no specs from {spec_file}")
    expected_by_project: dict[str, int] = {}
    for spec in slice_specs:
        for test in spec.get("tests", []):
            if test.get("status") == "expected":
                project = test.get("projectName", "")
                expected_by_project[project] = expected_by_project.get(project, 0) + 1
    for project in ("mock", "rust"):
        count = expected_by_project.get(project, 0)
        if count == 0:
            errors.append(
                f"no passing tests were executed for project {project!r} from {spec_file}"
            )
    if errors:
        fail(errors)
    return sum(expected_by_project.values())


def check_require_executed(slice_id: str, cargo_log: Path, playwright_json: Path) -> None:
    cargo_count = check_cargo_log(slice_id, cargo_log)
    playwright_count = check_playwright_report(slice_id, playwright_json)

    coverage = load_coverage()
    rows = coverage_rows(coverage, slice_id)
    errors: list[str] = []
    cargo_test_names = set(re.findall(r"^test (\S+) ... ok", load_log(cargo_log), re.M))
    report = json.loads(playwright_json.read_text(encoding="utf-8"))
    titles: list[str] = []
    specs: list[dict[str, Any]] = []
    for suite in report.get("suites", []):
        _walk_playwright_suites(suite, specs)
    for spec in specs:
        titles.append(str(spec.get("title", "")))
    for row in rows:
        test_id = str(row.get("test_id", ""))
        if not test_id:
            continue
        in_cargo = any(test_id in name for name in cargo_test_names)
        in_playwright = any(test_id in title for title in titles)
        if not (in_cargo or in_playwright):
            errors.append(
                f"coverage test_id {test_id!r} (requirement {row.get('requirement')}) does not "
                "match any executed cargo test or playwright case; tests must report matching "
                "IDs in machine-readable results (RFC-0004)"
            )
    if errors:
        fail(errors)
    print(
        f"require-executed {slice_id}: cargo {cargo_count} passed, "
        f"playwright {playwright_count} expected result(s), coverage IDs matched."
    )


def load_log(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--through", metavar="SLICE", help="verify coverage through SLICE")
    parser.add_argument("--check-manifest", action="store_true", help="verify the Vite manifest")
    parser.add_argument("--slice", metavar="SLICE", help="require executed proof for SLICE")
    parser.add_argument("--require-executed", action="store_true")
    parser.add_argument("--cargo-log", type=Path, help="override cargo test log path")
    parser.add_argument("--playwright-json", type=Path, help="override playwright report path")
    args = parser.parse_args()

    if not args.through and not args.slice:
        parser.error("either --through SLICE or --slice SLICE is required")
    if args.slice and not args.require_executed:
        parser.error("--slice requires --require-executed")

    try:
        if args.through:
            check_coverage_through(args.through)
            check_import_gates()
            if args.check_manifest:
                check_vite_manifest()
        if args.slice:
            slice_id = args.slice
            cargo_log = args.cargo_log or CARGO_LOG_PATH.with_name(
                CARGO_LOG_PATH.name.format(slice=slice_id)
            )
            playwright_json = args.playwright_json or PLAYWRIGHT_JSON_PATH.with_name(
                PLAYWRIGHT_JSON_PATH.name.format(slice=slice_id)
            )
            check_require_executed(slice_id, cargo_log, playwright_json)
            check_import_gates()
    except VerificationError as exc:
        fail([str(exc)])


if __name__ == "__main__":
    main()
