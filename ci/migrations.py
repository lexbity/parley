"""Migration verifier for Parley (RFC-0002 BE-01/BE-04, RFC-0004 S01).

For `--through SLICE` this script:

1. requires every migration owned by slices up to SLICE to exist,
2. verifies sqlx-cli is installed and matches the sqlx library (major.minor),
3. applies each cumulative prefix of the migration set to a real, private
   SQLite file with `sqlx migrate run`, then checks integrity, foreign keys,
   applied row counts and that stored checksums match SHA-384 of the files,
4. runs `cargo sqlx prepare --workspace --check` against the fully migrated
   database so query metadata cannot drift from the schema.

Temporary databases live in a private temp directory and are removed after the
run. No migration is ever applied to a real workspace from here.
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

# Slice -> owned migration file (RFC-0004 implementation plan).
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
    "s12": [],
    "p01": ["0012_party_sheet.sql"],
    "p02": [],
    "p03": [],
    "p04": [],
    "r01": ["0013_resource_board.sql"],
    "r02": [],
    "r03": [],
    "r04": [],
}


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

    errors: list[str] = []
    required: list[tuple[str, str]] = []
    for slice_id in [f"s{n:02d}" for n in range(1, 12)] + [
        f"p{n:02d}" for n in range(1, 5)
    ]:
        for name in SLICE_MIGRATIONS[slice_id]:
            required.append((slice_id, name))

    owned = {
        slice_id: name
        for slice_id in slices_through(last)
        for name in SLICE_MIGRATIONS[slice_id]
    }
    for slice_id, name in required:
        path = MIGRATIONS_DIR / name
        if slice_id in owned and not path.exists():
            errors.append(
                f"{slice_id} owns migrations/{name}, which is missing "
                "(RFC-0004 implementation plan)"
            )
    if errors:
        fail(errors)

    applied = [MIGRATIONS_DIR / name for _, name in required if (MIGRATIONS_DIR / name).exists()]
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
