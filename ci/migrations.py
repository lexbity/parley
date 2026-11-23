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
    "s07": ["0007_threads.sql"],
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
    ],
    "r01": ["0018_resource_board.sql"],
    "r02": [],
    "r03": [],
    "r04": ["0020_resource_workspace.sql"],
    "u01": ["0021_command_ledger.sql"],
    "u02": ["0022_agent_runs.sql"],
}

# Ownership entries removed one at a time by the negative self-test.
SELF_TEST_VICTIMS = ("0019_queue_sheet.sql", "0020_resource_workspace.sql")


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

    The map is copied per victim; the canonical SLICE_MIGRATIONS is never
    mutated. If the check ever passes with an unowned 0019 or 0020, the
    positive ownership gate would be vacuous and this self-test fails the run.
    """
    for victim in SELF_TEST_VICTIMS:
        if not any(victim in names for names in SLICE_MIGRATIONS.values()):
            raise VerificationError(
                f"negative ownership self-test cannot find {victim} in SLICE_MIGRATIONS"
            )
        mutated = {
            slice_id: [name for name in names if name != victim]
            for slice_id, names in SLICE_MIGRATIONS.items()
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
    verify_ownership(SLICE_MIGRATIONS, highest)
    self_test_ownership_gate()
    print(
        f"migration ownership verified: {len(required)} declared migration(s) through {last}, "
        f"highest required version {highest:04d}."
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


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--through", required=True, metavar="SLICE", help="highest slice ID")
    args = parser.parse_args()
    try:
        verify_through(args.through)
    except VerificationError as exc:
        fail([str(exc)])


if __name__ == "__main__":
    main()
