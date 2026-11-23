"""Shared helpers for Parley's project-owned CI verifiers (RFC-0004).

These scripts are the project's own gates; they deliberately have no framework
dependencies beyond PyYAML/jsonschema where a format demands it, and they always
fail with a precise, actionable message instead of passing vacuously.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from typing import Any, NoReturn

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent

# Canonical cumulative order. Every slice up to and including U09 is a real
# gate target; verifiers and scripts/prove read this one list so the accepted
# ID set cannot drift between gates.
SLICE_ORDER = (
    [f"s{n:02d}" for n in range(1, 13)]
    + [f"p{n:02d}" for n in range(1, 5)]
    + [f"r{n:02d}" for n in range(1, 5)]
    + [f"u{n:02d}" for n in range(1, 10)]
)


def load_coverage() -> dict[str, Any]:
    """Load contract/coverage.yaml, the per-slice traceability manifest."""
    path = REPO_ROOT / "contract" / "coverage.yaml"
    if not path.exists():
        raise VerificationError(
            f"contract/coverage.yaml is missing; every slice must add rows per RFC-0004 "
            "(requirement, operationId, test ID, fixture ID, owner role, package target, evidence path)"
        )
    data = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not isinstance(data.get("slices", {}), dict):
        raise VerificationError("contract/coverage.yaml must contain a 'slices' mapping")
    return data


def coverage_rows(coverage: dict[str, Any], slice_id: str) -> list[dict[str, Any]]:
    rows = coverage["slices"].get(slice_id, [])
    if not isinstance(rows, list):
        raise VerificationError(f"coverage rows for {slice_id} must be a list")
    return rows


class VerificationError(Exception):
    """A gate failed; the message is shown to the author verbatim."""


def slices_through(last: str) -> list[str]:
    """Return every slice ID up to and including `last` in canonical order."""
    if last not in SLICE_ORDER:
        raise VerificationError(
            f"unknown slice ID {last!r}; expected one of {', '.join(SLICE_ORDER)}"
        )
    return SLICE_ORDER[: SLICE_ORDER.index(last) + 1]


def run(command: list[str], cwd: Path | None = None, env: dict[str, str] | None = None) -> str:
    """Run a subprocess, converting any failure into a VerificationError."""
    display = " ".join(command) + (f"  (cwd={cwd})" if cwd else "")
    try:
        proc = subprocess.run(
            command,
            cwd=cwd or REPO_ROOT,
            env=env,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError as exc:
        raise VerificationError(f"required tool not found for: {display}\n  {exc}") from exc
    if proc.returncode != 0:
        detail = (proc.stdout + proc.stderr).strip()
        raise VerificationError(f"command failed ({proc.returncode}): {display}\n{detail}")
    return proc.stdout


def run_rg(pattern: str, paths: list[Path]) -> list[str]:
    """Run ripgrep exactly once; return matching lines.

    rg exit 1 (no matches) is clean. Any other nonzero status — including a
    missing binary or a bad pattern — is a hard failure, never silently
    tolerated (this mirrors scripts/absent).
    """
    proc = subprocess.run(
        ["rg", "-n", "--no-heading", "--", pattern, *[str(p) for p in paths]],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if proc.returncode == 1:
        return []
    if proc.returncode != 0:
        raise VerificationError(
            f"ripgrep failed with status {proc.returncode} for pattern {pattern!r}:\n"
            f"{proc.stderr.strip()}"
        )
    return [line for line in proc.stdout.splitlines() if line]


def gate(name: str, condition: bool, message: str) -> None:
    if not condition:
        raise VerificationError(f"[{name}] {message}")


def fail(errors: list[str]) -> NoReturn:
    for error in errors:
        print(f"FAIL: {error}", file=sys.stderr)
    sys.exit(1)
