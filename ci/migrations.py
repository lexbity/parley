"""Migration verifier for Parley (RFC-0002 BE-01/BE-04, RFC-0004 S01, UCR-001 U01/U02).

For `--through SLICE` this script:

1. requires every migration owned by slices up to SLICE to exist,
2. verifies sqlx-cli is installed and matches the sqlx library (major.minor),
3. applies every migration file whose numeric prefix is at or below the
   highest required version — in numeric order, not slice order — to a real,
   private SQLite file with `sqlx migrate run`, then checks integrity, foreign
   keys, applied row counts and that stored checksums match SHA-384 of the
   files,
4. runs `cargo sqlx prepare --workspace --check` against the fully migrated
   database so query metadata cannot drift from the schema.

Every migration file in that numeric range must have exactly one owning
slice. The ownership check is exercised by a negative self-test that removes
the 0019 and 0020 owners in turn and requires the check to fail. Temporary
databases live in a private temp directory and are removed after the run. No
migration is ever applied to a real workspace from here.

Migration allocation (TD-001/TD-09): plan slices own sequential versions below
0900; out-of-plan work (debt, performance, tooling) allocates from the reserved
band [0900,1000). `--check-bands` verifies that policy on its own; `--through`
runs it too. Both negative self-tests run on every `--through` gate so an
unowned migration or a mis-banded one cannot pass vacuously; `--check-bands`
runs the band self-test. No file is planted on disk by a self-test, so the
gates leave no artifact behind.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import shutil
import sqlite3
import tempfile
from pathlib import Path

from common import REPO_ROOT, VerificationError, fail, run, slices_through

MIGRATIONS_DIR = REPO_ROOT / "migrations"
MIGRATION_PREFIX = re.compile(r"^(\d{4})_")

# Slice -> owned migration file (RFC-0004 implementation plan, UCR-001 U01/U02).
SLICE_MIGRATIONS: dict[str, list[str]] = {
    "s01": ["0001_workspace.sql"],
    "s02": ["0002_graph.sql"],
    "s03": ["0003_materials.sql"],
    "s04": ["0004_jobs.sql"],
    "s05": ["0005_board.sql"],
    "s06": ["0006_blobs.sql"],
    "s07": ["0007_threads.sql", "0035_thread_read_index.sql"],
    "s08": ["0008_assistant.sql"],
    "s09": ["0009_writing.sql"],
    "s10": ["0010_coach.sql"],
    "s11": ["0011_legacy.sql"],
    "s12": ["0019_queue_sheet.sql"],
    "p01": ["0012_party_sheet.sql"],
    "p02": ["0013_graph_cards.sql"],
    "p03": [],
    "p04": [
        "0014_party_intake.sql",
        "0015_resource_cards.sql",
        "0016_party_private_staging.sql",
        "0017_card_retirement.sql",
        "0033_opportunity_updated_index.sql",
    ],
    "r01": ["0018_resource_board.sql", "0023_resource_board_controls.sql"],
    "r02": [],
    "r03": [],
    "r04": ["0020_resource_workspace.sql"],
    "u01": ["0021_command_ledger.sql"],
    "u02": ["0022_agent_runs.sql"],
    "u03": [],
    "u04": [],
    "u05": ["0024_scoped_command_jobs.sql"],
    "u06": ["0025_canonical_resources.sql"],
    "u07": [],
    "ar01": ["0027_inference_boundary.sql"],
    "ar02": ["0028_coach_question_context.sql"],
    "ar03": ["0029_template_candidates.sql"],
    "ar04": ["0030_chat_job_bindings.sql"],
    "ar05": ["0031_delegated_profiles.sql"],
    "ar06": [],
    "ar07": ["0032_qualified_embeddings.sql"],
    "ar08": ["0034_remove_direct_provider_config.sql"],
    "ar09": ["0036_inference_activity.sql"],
    "u08": ["0026_command_approvals.sql"],
    "u09": [],
}

# Ownership entries removed one at a time by the negative self-test.
SELF_TEST_VICTIMS = ("0019_queue_sheet.sql", "0020_resource_workspace.sql")

# TD-001/TD-09: plan slices own the next sequential version below 0900.
# Out-of-plan work (debt, performance, tooling) allocates from a reserved band
# so two branches can no longer choose the same "next" number. A band entry may
# be a reservation for a file that lands with a later slice; verify_ownership
# only inspects declarations at or below the requested slice's highest version,
# so a pending reservation does not disturb a --through gate.
RESERVED_BAND = (900, 1000)
PLAN_SLICE_RE = re.compile(r"^(?:s|p|r|u|ar)\d{2}$")
DEBT_MIGRATIONS: dict[str, str] = {
    # Reserved for TD-04's aggregate-read indexes; the file lands with TD-04.
    "0900_debt_read_indexes.sql": "TD-001/TD-04",
}
# Planting victims is done in memory, never on disk, so a self-test leaves no
# file for ci/disk.py to flag and never races a concurrent migration run.
BAND_SELF_TEST_PLAN_VICTIM = "0999_band_violation.sql"
BAND_SELF_TEST_DEBT_VICTIM = "0899_debt_below_band.sql"


def numeric_prefix(name: str) -> int:
    """Return the four-digit sqlx version prefix of a migration filename."""
    match = MIGRATION_PREFIX.match(name)
    if not match:
        raise VerificationError(
            f"migration {name!r} has no four-digit numeric prefix; sqlx cannot order it"
        )
    return int(match.group(1))


def migration_files() -> list[Path]:
    """Return every migration file in numeric prefix order (ties by name)."""
    files = [path for path in MIGRATIONS_DIR.glob("*.sql") if MIGRATION_PREFIX.match(path.name)]
    return sorted(files, key=lambda path: (numeric_prefix(path.name), path.name))


def in_reserved_band(version: int) -> bool:
    """True when `version` belongs to the out-of-plan allocation band."""
    low, high = RESERVED_BAND
    return low <= version < high


def ownership_registry() -> dict[str, list[str]]:
    """Return plan and debt ownership as one owner-id -> filenames mapping.

    Debt reservations are included so a present band file is never unowned,
    but verify_ownership's version filter keeps a not-yet-landed reservation
    out of --through gates until the file exists.
    """
    merged = {slice_id: list(names) for slice_id, names in SLICE_MIGRATIONS.items()}
    for name, debt_id in DEBT_MIGRATIONS.items():
        merged.setdefault(debt_id, []).append(name)
    return merged


def verify_bands(
    plan: dict[str, list[str]] | None = None,
    debt: dict[str, str] | None = None,
    present: set[str] | None = None,
) -> None:
    """Enforce TD-001/TD-09 allocation policy across declarations and files.

    Plan slices (the keys of SLICE_MIGRATIONS) own contiguous versions <0900.
    Out-of-plan debt owners own versions in [0900,1000). No file may have two
    owners of different kinds, and no two files may share a numeric prefix.

    `plan`, `debt` and `present` are injectable so the negative self-test can
    plant a violating allocation without touching the canonical registries or
    the working tree.
    """
    plan_map = SLICE_MIGRATIONS if plan is None else plan
    debt_map = DEBT_MIGRATIONS if debt is None else debt
    present_names = set() if present is None else present

    errors: list[str] = []
    for owner in plan_map:
        if not PLAN_SLICE_RE.fullmatch(owner):
            errors.append(
                f"{owner!r} is not a plan slice ID (sNN/pNN/rNN/uNN/arNN); "
                "only plan slices may own migrations below 0900"
            )

    plan_by_name: dict[str, list[str]] = {}
    for owner, names in plan_map.items():
        for name in names:
            plan_by_name.setdefault(name, []).append(owner)
    debt_by_name: dict[str, list[str]] = {}
    for name, owner in debt_map.items():
        debt_by_name.setdefault(name, []).append(owner)

    by_prefix: dict[int, list[str]] = {}
    for name in sorted(set(plan_by_name) | set(debt_by_name) | present_names):
        version = numeric_prefix(name)
        by_prefix.setdefault(version, []).append(name)
        plan_owners = plan_by_name.get(name, [])
        debt_owners = debt_by_name.get(name, [])
        if plan_owners and debt_owners:
            errors.append(
                f"band violation: migrations/{name} is owned by both plan slice(s) "
                f"{plan_owners} and debt owner(s) {debt_owners}"
            )
        if name in present_names and not (plan_owners or debt_owners):
            errors.append(
                f"migrations/{name} (version {version:04d}) is present but has no owner in "
                "SLICE_MIGRATIONS or DEBT_MIGRATIONS"
            )
        if in_reserved_band(version):
            if plan_owners:
                errors.append(
                    f"band violation: migrations/{name} (version {version:04d}) is owned by "
                    f"plan slice(s) {plan_owners}; versions [0900,1000) are reserved for "
                    "out-of-plan debt work"
                )
        else:
            if debt_owners:
                errors.append(
                    f"band violation: migrations/{name} (version {version:04d}) is owned by "
                    f"debt owner(s) {debt_owners}; out-of-plan migrations must use [0900,1000)"
                )
            if version >= RESERVED_BAND[1]:
                errors.append(
                    f"band violation: migrations/{name} (version {version:04d}) is outside "
                    "both the plan range and the reserved band [0900,1000)"
                )

    plan_versions = sorted(numeric_prefix(name) for name in plan_by_name)
    if plan_versions and plan_versions != list(range(1, len(plan_versions) + 1)):
        errors.append(
            "plan migration versions are not contiguous from 0001: "
            f"{[f'{v:04d}' for v in plan_versions]}; plan slices own the next sequential number"
        )

    for version, names in sorted(by_prefix.items()):
        if len(names) != 1:
            errors.append(
                f"duplicate migration version {version:04d}: {sorted(names)}; "
                "sqlx needs one file per numeric prefix"
            )

    if errors:
        raise VerificationError("\n".join(errors))


def self_test_band_gate() -> None:
    """Negative gate: mis-banded allocations must be rejected by verify_bands.

    Mirrors self_test_ownership_gate: the canonical registries are never
    mutated, and the victims exist only in copied dictionaries.
    """
    plan = {slice_id: list(names) for slice_id, names in SLICE_MIGRATIONS.items()}
    plan["ar08"] = plan.get("ar08", []) + [BAND_SELF_TEST_PLAN_VICTIM]
    try:
        verify_bands(plan=plan)
    except VerificationError as exc:
        if "band violation" not in str(exc) or BAND_SELF_TEST_PLAN_VICTIM not in str(exc):
            raise VerificationError(
                "negative band self-test rejected the planted plan migration for the wrong "
                f"reason (expected band violation for {BAND_SELF_TEST_PLAN_VICTIM}): {exc}"
            ) from exc
    else:
        raise VerificationError(
            f"negative band self-test failed: plan-slice migration {BAND_SELF_TEST_PLAN_VICTIM} "
            "in the reserved band still passed"
        )

    debt = dict(DEBT_MIGRATIONS)
    debt[BAND_SELF_TEST_DEBT_VICTIM] = "TD-001/TD-99"
    try:
        verify_bands(debt=debt)
    except VerificationError as exc:
        if "band violation" not in str(exc) or BAND_SELF_TEST_DEBT_VICTIM not in str(exc):
            raise VerificationError(
                "negative band self-test rejected the planted debt migration for the wrong "
                f"reason (expected band violation for {BAND_SELF_TEST_DEBT_VICTIM}): {exc}"
            ) from exc
    else:
        raise VerificationError(
            f"negative band self-test failed: out-of-plan migration {BAND_SELF_TEST_DEBT_VICTIM} "
            "below the reserved band still passed"
        )
    print("negative band self-test passed (plan >=0900 and debt <0900 are rejected).")


def verify_ownership(migrations: dict[str, list[str]], highest: int) -> None:
    """Require exactly one slice to own every migration file up to `highest`.

    This is the internal function the negative self-test mutates. It raises
    VerificationError with every ambiguity, missing owner or missing file; it
    never filters the slice set, so an unowned migration cannot hide behind a
    later slice's ownership entry.
    """
    errors: list[str] = []
    owners: dict[str, list[str]] = {}
    for slice_id, names in migrations.items():
        for name in names:
            if numeric_prefix(name) <= highest:
                owners.setdefault(name, []).append(slice_id)

    present = {
        path.name: path for path in migration_files() if numeric_prefix(path.name) <= highest
    }
    for name in sorted(present):
        declared = owners.get(name, [])
        if len(declared) != 1:
            errors.append(
                f"migrations/{name} (version {numeric_prefix(name)}) is owned by "
                f"{len(declared)} slice(s): {declared or 'none'}; exactly one owner is required"
            )
    for name in sorted(set(owners) - set(present)):
        errors.append(
            f"{', '.join(owners[name])} own(s) migrations/{name}, which is missing from "
            f"{MIGRATIONS_DIR.relative_to(REPO_ROOT)}"
        )

    by_prefix: dict[int, list[str]] = {}
    for name in present:
        by_prefix.setdefault(numeric_prefix(name), []).append(name)
    for version, names in sorted(by_prefix.items()):
        if len(names) != 1:
            errors.append(
                f"migration version {version} is ambiguous: {sorted(names)}; sqlx needs one file "
                "per numeric prefix"
            )
    if errors:
        raise VerificationError("\n".join(errors))


def self_test_ownership_gate() -> None:
    """Negative gate: removing a real owner must make verify_ownership fail.

    The map is copied per victim; the canonical registries are never
    mutated. If the check ever passes with an unowned 0019 or 0020, the
    positive ownership gate would be vacuous and this self-test fails the run.
    """
    registry = ownership_registry()
    for victim in SELF_TEST_VICTIMS:
        if not any(victim in names for names in registry.values()):
            raise VerificationError(
                f"negative ownership self-test cannot find {victim} in the migration registries"
            )
        mutated = {
            owner: [name for name in names if name != victim]
            for owner, names in registry.items()
        }
        try:
            verify_ownership(mutated, numeric_prefix(victim))
        except VerificationError as exc:
            if victim in str(exc):
                continue
            raise VerificationError(
                f"negative ownership self-test rejected the mutated map for the wrong reason "
                f"(expected {victim}): {exc}"
            ) from exc
        raise VerificationError(
            f"negative ownership self-test failed: removing the owner of {victim} still passed "
            "the ownership check"
        )
    print("negative ownership self-test passed (0019 and 0020 owners are required).")


def sqlx_cli_version() -> str:
    argv = ["sqlx", "--version"] if shutil.which("sqlx") else ["cargo", "sqlx", "--version"]
    return run(argv).strip()


def sqlx_library_version() -> str:
    lock = (REPO_ROOT / "Cargo.lock").read_text(encoding="utf-8")
    match = re.search(r'name = "sqlx"\nversion = "(\d+\.\d+\.\d+)"', lock)
    if not match:
        raise VerificationError("sqlx not found in Cargo.lock")
    return match.group(1)


def check_sqlx_cli() -> str:
    out = sqlx_cli_version()
    match = re.search(r"sqlx-cli (\d+\.\d+\.\d+)", out)
    if not match:
        raise VerificationError(f"could not parse sqlx-cli version from: {out!r}")
    cli = match.group(1)
    lib = sqlx_library_version()
    if cli.rsplit(".", 1)[0] != lib.rsplit(".", 1)[0]:
        raise VerificationError(
            f"sqlx-cli {cli} does not match sqlx library {lib}; "
            "RFC-0004 requires the CLI and library versions to match"
        )
    return cli


def sqlite_url(db_path: Path) -> str:
    # mode=rwc lets sqlx create the fresh private database file it migrates.
    return f"sqlite://{db_path}?mode=rwc"


def verify_applied_database(db_path: Path, expected: list[Path]) -> None:
    conn = sqlite3.connect(db_path)
    try:
        result = conn.execute("PRAGMA integrity_check").fetchone()
        if not result or result[0] != "ok":
            raise VerificationError(f"integrity_check failed on {db_path}: {result}")
        violations = conn.execute("PRAGMA foreign_key_check").fetchall()
        if violations:
            raise VerificationError(
                f"foreign_key_check reported violations on {db_path}: {violations[:5]}"
            )
        rows = conn.execute(
            "SELECT version, checksum FROM _sqlx_migrations ORDER BY version"
        ).fetchall()
        if len(rows) != len(expected):
            raise VerificationError(
                f"_sqlx_migrations has {len(rows)} rows, expected {len(expected)}"
            )
        for (version, checksum), path in zip(rows, expected):
            digest = hashlib.sha384(path.read_bytes()).digest()
            if bytes(checksum) != digest:
                raise VerificationError(
                    f"checksum mismatch for migration {path.name} (version {version}); "
                    "migration files are immutable once applied"
                )
    finally:
        conn.close()


def apply_prefix(prefix: list[Path], workdir: Path) -> Path:
    migrations_copy = workdir / "migrations"
    migrations_copy.mkdir()
    for migration in prefix:
        shutil.copy2(migration, migrations_copy / migration.name)
    db_path = workdir / "workspace.sqlite"
    env = {"DATABASE_URL": sqlite_url(db_path), "PATH": os.environ["PATH"]}
    run(["sqlx", "migrate", "run"], cwd=workdir, env=env)
    verify_applied_database(db_path, prefix)
    return db_path


def verify_through(last: str) -> None:
    cli = check_sqlx_cli()
    print(f"sqlx-cli {cli} matches the sqlx library.")

    required: list[tuple[str, str]] = []
    for slice_id in slices_through(last):
        for name in SLICE_MIGRATIONS[slice_id]:
            required.append((slice_id, name))
    if not required:
        raise VerificationError(
            f"no migrations are declared for any slice through {last}; a verifier that applies "
            "zero migrations cannot qualify the schema"
        )
    highest = max(numeric_prefix(name) for _, name in required)
    verify_ownership(ownership_registry(), highest)
    verify_bands(present={path.name for path in migration_files()})
    self_test_ownership_gate()
    self_test_band_gate()
    print(
        f"migration ownership and band allocation verified: {len(required)} declared "
        f"migration(s) through {last}, highest required version {highest:04d}, "
        f"reserved band [{RESERVED_BAND[0]:04d},{RESERVED_BAND[1]:04d})."
    )

    applied = [path for path in migration_files() if numeric_prefix(path.name) <= highest]
    with tempfile.TemporaryDirectory(prefix="parley-migrations-") as tmp:
        workdir = Path(tmp)
        db_path = Path(workdir / "workspace.sqlite")
        for count in range(1, len(applied) + 1):
            prefix_dir = workdir / f"prefix-{count:02d}"
            prefix_dir.mkdir()
            prefix_db = apply_prefix(applied[:count], prefix_dir)
            if count == len(applied):
                shutil.copy2(prefix_db, db_path)
        env = {
            "DATABASE_URL": sqlite_url(db_path),
            "PATH": os.environ["PATH"],
            "HOME": os.environ.get("HOME", ""),
        }
        print(
            f"applied {len(applied)} cumulative prefixes to private SQLite files; "
            "running cargo sqlx prepare --workspace --check"
        )
        run(["cargo", "sqlx", "prepare", "--workspace", "--check"], cwd=REPO_ROOT, env=env)
    print(f"migrations verified through {last}.")


def verify_bands_only() -> None:
    """Run the allocation gate alone, without sqlx apply or query-metadata checks."""
    verify_bands(present={path.name for path in migration_files()})
    self_test_band_gate()
    print(
        "migration band allocation verified: plan versions are contiguous below 0900, "
        f"out-of-plan versions use [{RESERVED_BAND[0]:04d},{RESERVED_BAND[1]:04d}), "
        f"{len(migration_files())} file(s) present."
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--through", metavar="SLICE", help="highest slice ID")
    parser.add_argument(
        "--check-bands",
        action="store_true",
        help="verify the allocation policy without applying migrations "
        "(included in --through runs)",
    )
    args = parser.parse_args()
    if not args.through and not args.check_bands:
        parser.error("provide --through SLICE or --check-bands")
    try:
        if args.through:
            verify_through(args.through)
        else:
            verify_bands_only()
    except VerificationError as exc:
        fail([str(exc)])


if __name__ == "__main__":
    main()
