"""Disk hygiene for Parley development and testing.

Development must not grow disk usage unboundedly. This script gives the
harness teeth:

`--check` (runs in scripts/check-slice, before the slice-gated verifiers, so
it executes on every verification even while slice work is red):
  * fails if database artifacts (*.sqlite, *.sqlite3, *.db, *-wal, *-shm)
    exist in the working tree outside contract/fixtures — test workspaces and
    scratch databases belong in OS temp directories, never in the repo;
  * enforces the size budgets in ci/disk-budgets.yaml for target/, the test
    artifact directories, and the repo tree excluding regenerable output.

`--report` prints the size of every known growth vector, including the
machine-global caches (Playwright browsers, cargo registry, pnpm store).

`--prune` frees regenerable space: repo-local artifacts, `cargo clean`,
`pnpm store prune` (officially removes only unreferenced packages) and stale
/tmp/parley-* scratch dirs. Machine-global caches require explicit opt-in:
`--playwright-cache` removes browser builds not referenced by the pinned
Playwright version (other projects on this machine may need
`playwright install` again); `--registry` removes cargo registry downloads and
extracted sources (re-fetched on demand). `--dry-run` lists actions without
executing them.

stdlib only, so the Rust CI lane can run it without pip installs.
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
import time
from pathlib import Path

import yaml

from common import REPO_ROOT, VerificationError, fail, run

BUDGETS_PATH = REPO_ROOT / "ci" / "disk-budgets.yaml"

DB_SUFFIXES = (".sqlite", ".sqlite3", ".db", "-wal", "-shm")
DB_ALLOWED_PREFIX = REPO_ROOT / "contract" / "fixtures"

ARTIFACT_DIRS = [
    REPO_ROOT / "artifacts",
    REPO_ROOT / "web" / "artifacts",
]

# Regenerable or third-party output that must never count against the
# repo-tree budget.
REGENERABLE_PATHS = {
    REPO_ROOT / ".git",
    REPO_ROOT / "target",
    REPO_ROOT / "artifacts",
    REPO_ROOT / "web" / "dist",
    REPO_ROOT / "web" / "artifacts",
    REPO_ROOT / "fuzz" / "target",
    REPO_ROOT / "fuzz" / "corpus",
}
REGENERABLE_DIR_NAMES = {
    "node_modules",
    "coverage",
    "test-results",
    "playwright-report",
    "blob-report",
}

TMP_SCRATCH_PREFIX = "parley-"


def parse_size(text: str) -> int:
    units = {"B": 1, "KIB": 1024, "MIB": 1024**2, "GIB": 1024**3}
    value, _, unit = text.strip().upper().partition(" ")
    unit = unit.strip() or "B"
    if unit not in units or not value:
        raise VerificationError(f"cannot parse size {text!r} (expected e.g. '256 MiB')")
    return int(float(value) * units[unit])


def human(n: int) -> str:
    for unit, factor in (("GiB", 1024**3), ("MiB", 1024**2), ("KiB", 1024)):
        if n >= factor:
            return f"{n / factor:.1f} {unit}"
    return f"{n} B"


def load_budgets(override: Path | None) -> dict[str, dict[str, int]]:
    path = override or BUDGETS_PATH
    data = yaml.safe_load(path.read_text(encoding="utf-8"))
    parsed: dict[str, dict[str, int]] = {}
    for name, levels in data.items():
        parsed[name] = {level: parse_size(amount) for level, amount in levels.items()}
    for required in ("target_dir", "artifacts", "repo_tree"):
        if required not in parsed:
            raise VerificationError(f"{path.name} is missing the {required!r} budget")
    return parsed


def scan_tree(root: Path, exclude_names: set[str] | None = None) -> tuple[int, list[Path]]:
    """Sum file sizes under root in one walk; also return db-suffixed files."""
    exclude_names = exclude_names or set()
    total = 0
    db_files: list[Path] = []
    if not root.exists():
        return 0, []
    for current, dirs, files in os.walk(root, topdown=True):
        here = Path(current)
        dirs[:] = [d for d in dirs if d not in exclude_names]
        for name in files:
            path = here / name
            try:
                total += path.stat().st_size
            except OSError:
                continue
            if name.lower().endswith(DB_SUFFIXES) and not path.is_relative_to(DB_ALLOWED_PREFIX):
                db_files.append(path)
    return total, db_files


def scan_repo() -> tuple[int, int, int, list[Path]]:
    """One walk over the whole repo (pruning .git and node_modules).

    Classifies every file into target/, artifacts/, or the counted repo tree;
    returns (target_size, artifacts_size, tree_size, db_files) so the check
    and the db-artifact rule share a single pass.
    """
    target_size = artifacts_size = tree_size = 0
    db_files: list[Path] = []
    for current, dirs, files in os.walk(REPO_ROOT, topdown=True):
        here = Path(current)
        dirs[:] = [d for d in dirs if d != "node_modules" and d != ".git"]
        for name in files:
            path = here / name
            try:
                size = path.stat().st_size
            except OSError:
                continue
            if name.lower().endswith(DB_SUFFIXES) and not path.is_relative_to(DB_ALLOWED_PREFIX):
                db_files.append(path)
            if path.is_relative_to(REPO_ROOT / "target"):
                target_size += size
            elif any(path.is_relative_to(p) for p in ARTIFACT_DIRS):
                artifacts_size += size
            elif path.is_relative_to(REPO_ROOT / ".git") or path.is_relative_to(
                REPO_ROOT / "web" / "dist"
            ) or any(
                path.is_relative_to(path_root)
                for path_root in (REPO_ROOT / "fuzz" / "target", REPO_ROOT / "fuzz" / "corpus")
            ):
                continue
            elif {p.name for p in path.parents} & REGENERABLE_DIR_NAMES:
                continue
            else:
                tree_size += size
    return target_size, artifacts_size, tree_size, db_files


def budget_check(
    name: str, size: int, levels: dict[str, int], errors: list[str], warnings: list[str]
) -> None:
    fail_at = levels.get("fail")
    warn_at = levels.get("warn")
    label = f"{name}: {human(size)}"
    if fail_at is not None and size > fail_at:
        errors.append(f"{label} exceeds the fail budget {human(fail_at)}; run scripts/prune")
    elif warn_at is not None and size > warn_at:
        warnings.append(f"{label} exceeds the warn budget {human(warn_at)}; run scripts/prune")


def check(budgets_override: Path | None) -> None:
    budgets = load_budgets(budgets_override)
    errors: list[str] = []
    warnings: list[str] = []

    target_size, artifacts_size, tree_size, db_files = scan_repo()
    for db in db_files:
        errors.append(
            f"database artifact in the working tree: {db.relative_to(REPO_ROOT)} — test "
            "workspaces and scratch databases belong in OS temp directories "
            "(only contract/fixtures may ship database-like files)"
        )

    budget_check("target/", target_size, budgets["target_dir"], errors, warnings)
    budget_check("test artifacts/", artifacts_size, budgets["artifacts"], errors, warnings)
    budget_check(
        "repo tree (excl. regenerable output)",
        tree_size,
        budgets["repo_tree"],
        errors,
        warnings,
    )

    for warning in warnings:
        print(f"WARN: {warning}", file=sys.stderr)
    if errors:
        fail(errors)
    print(
        f"disk check passed: target/ {human(target_size)}, artifacts "
        f"{human(artifacts_size)}, repo tree {human(tree_size)}, "
        f"{len(db_files)} database artifacts in the working tree."
    )


def measure(paths: list[Path]) -> int:
    return sum(scan_tree(path)[0] for path in paths)


def report() -> None:
    home = Path.home()
    target_size, artifacts_size, tree_size, _ = scan_repo()
    rows: list[tuple[str, str]] = [
        ("repo target/", human(target_size)),
        ("repo artifacts/ + web/artifacts/", human(artifacts_size)),
        ("repo web/dist/", human(scan_tree(REPO_ROOT / "web" / "dist")[0])),
        ("repo node_modules/", human(scan_tree(REPO_ROOT / "node_modules")[0])),
        ("repo tree (excl. regenerable)", human(tree_size)),
        ("playwright browser cache", human(scan_tree(home / ".cache" / "ms-playwright")[0])),
        ("cargo registry", human(scan_tree(home / ".cargo" / "registry")[0])),
    ]
    try:
        store = Path(run(["pnpm", "store", "path"]).strip())
        rows.append(("pnpm store", f"{human(scan_tree(store)[0])}  ({store})"))
    except VerificationError:
        rows.append(("pnpm store", "n/a"))
    scratch = sorted(Path("/tmp").glob(f"{TMP_SCRATCH_PREFIX}*")) + sorted(Path("/tmp").glob(f".{TMP_SCRATCH_PREFIX}*"))
    rows.append(("/tmp/parley-* and .parley-* scratch dirs", f"{len(scratch)} dir(s) {human(measure(scratch))}"))
    print(f"{'vector':<36}{'size':>12}")
    for label, size in rows:
        print(f"{label:<36}{size:>12}")


def playwright_keep_dirs() -> set[str]:
    """Directory names in the browser cache the pinned Playwright needs."""
    script = (
        "const path=require('path');"
        "const t=require.resolve('@playwright/test/package.json');"
        "const core=require.resolve('playwright-core/package.json',{paths:[path.dirname(t)]});"
        "const b=require(path.join(path.dirname(core),'browsers.json'));"
        "console.log(b.browsers.map(x=>x.name+'-'+x.revision).join('\\n'))"
    )
    return set(run(["node", "-e", script], cwd=REPO_ROOT / "web").split())


def prune(dry_run: bool, playwright_cache: bool, registry: bool) -> None:
    deletions: list[tuple[str, Path]] = []
    for path in ARTIFACT_DIRS:
        if path.exists():
            deletions.append((f"regenerable test artifacts {path.relative_to(REPO_ROOT)}/", path))
    for path in (REPO_ROOT / "web" / name for name in ("test-results", "playwright-report", "blob-report")):
        if path.exists():
            deletions.append((f"regenerable playwright output {path.relative_to(REPO_ROOT)}/", path))
    for scratch in sorted(Path("/tmp").glob(f"{TMP_SCRATCH_PREFIX}*")) + sorted(Path("/tmp").glob(f".{TMP_SCRATCH_PREFIX}*")):
        if time.time() - scratch.stat().st_mtime > 3600:
            deletions.append((f"stale scratch {scratch}", scratch))
    deletions.append(("cargo build cache target/ (next build restores it)", REPO_ROOT / "target"))

    browser_cache = Path.home() / ".cache" / "ms-playwright"
    keep: set[str] = set()
    if playwright_cache:
        keep = playwright_keep_dirs()
        if browser_cache.exists():
            for entry in sorted(browser_cache.iterdir()):
                if entry.name not in keep:
                    deletions.append(
                        (f"unused Playwright browser build {entry.name}", entry)
                    )

    if registry:
        registry_dir = Path.home() / ".cargo" / "registry"
        for name in ("cache", "src"):
            path = registry_dir / name
            if path.exists():
                deletions.append((f"cargo registry {name}/ (re-fetched on demand)", path))

    if dry_run:
        for label, _ in deletions:
            print(f"[dry-run] would remove {label}")
        print("[dry-run] would run pnpm store prune (removes unreferenced packages only)")
        return

    before = measure([path for _, path in deletions])
    for label, path in deletions:
        if path.is_dir():
            shutil.rmtree(path)
        elif path.exists():
            # Scratch names may match a plain file as well as a directory.
            path.unlink()
        print(f"pruned: {label}")
    run(["pnpm", "store", "prune"])
    print("pruned: pnpm store (unreferenced packages only)")
    after = measure([path for _, path in deletions])
    print(f"freed {human(max(before - after, 0))} in the pruned paths (store prune not counted).")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="enforce gates (exit 1 on failure)")
    parser.add_argument("--report", action="store_true", help="print sizes of growth vectors")
    parser.add_argument("--prune", action="store_true", help="free regenerable space")
    parser.add_argument("--dry-run", action="store_true", help="with --prune: list actions only")
    parser.add_argument(
        "--playwright-cache",
        action="store_true",
        help="with --prune: also remove browser builds unused by the pinned version (machine-global)",
    )
    parser.add_argument(
        "--registry",
        action="store_true",
        help="with --prune: also remove cargo registry caches (machine-global, re-fetched on demand)",
    )
    parser.add_argument("--budgets", type=Path, help="override ci/disk-budgets.yaml (for testing)")
    args = parser.parse_args()

    if sum((args.check, args.report, args.prune)) != 1:
        parser.error("exactly one of --check, --report, --prune is required")
    if args.dry_run and not args.prune:
        parser.error("--dry-run requires --prune")
    if (args.playwright_cache or args.registry) and not args.prune:
        parser.error("--playwright-cache/--registry require --prune")

    try:
        if args.check:
            check(args.budgets)
        elif args.report:
            report()
        else:
            prune(args.dry_run, args.playwright_cache, args.registry)
    except VerificationError as exc:
        fail([str(exc)])


if __name__ == "__main__":
    main()
