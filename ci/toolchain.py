"""Toolchain version manifest for Parley (RFC-0002 BE-01, RFC-0004 S01).

Records the exact installed versions of Rust, SQLx CLI, Node, pnpm, Python,
Playwright and the Playwright Chromium build into ci/toolchain-versions.yaml.
`--write` records the current environment; `--check` verifies the environment
still matches the recorded manifest and fails on any drift.

The Rust channel must additionally match rust-toolchain.toml, and SQLx CLI must
match the sqlx library version resolved in Cargo.lock (major.minor).
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
import tomllib
from pathlib import Path
from typing import Any, Callable

import yaml

from common import REPO_ROOT, VerificationError, fail, run

MANIFEST_PATH = REPO_ROOT / "ci" / "toolchain-versions.yaml"
RUST_TOOLCHAIN_PATH = REPO_ROOT / "rust-toolchain.toml"
WEB_DIR = REPO_ROOT / "web"


def _version_probe(argv: list[str], pattern: str, cwd: Path | None = None) -> str:
    out = run(argv, cwd=cwd).strip()
    match = re.search(pattern, out)
    if not match:
        raise VerificationError(f"could not parse {argv[0]} version from: {out!r}")
    return match.group(1)


def probe_rustc() -> str:
    return _version_probe(["rustc", "--version"], r"rustc (\S+)")


def probe_cargo() -> str:
    return _version_probe(["cargo", "--version"], r"cargo (\S+)")


def probe_node() -> str:
    return _version_probe(["node", "--version"], r"v(\S+)")


def probe_pnpm() -> str:
    return _version_probe(["pnpm", "--version"], r"(\S+)")


def probe_python() -> str:
    return ".".join(str(part) for part in sys.version_info[:3])


def probe_sqlx_cli() -> str:
    argv = ["sqlx", "--version"] if shutil.which("sqlx") else ["cargo", "sqlx", "--version"]
    return _version_probe(argv, r"sqlx-cli (\S+)")


def probe_playwright() -> str:
    return _version_probe(
        ["pnpm", "--dir", "web", "exec", "playwright", "--version"], r"Version (\S+)"
    )


def probe_agent_pi() -> str:
    # Read the installed SDK pin without launching the coding CLI.
    path = REPO_ROOT / "agent/node_modules/@earendil-works/pi-ai/package.json"
    try:
        version = json.loads(path.read_text(encoding="utf-8"))["version"]
    except (OSError, ValueError, KeyError) as error:
        raise VerificationError("install the locked Pi SDK before checking the toolchain") from error
    if not isinstance(version, str):
        raise VerificationError("installed Pi SDK has no version")
    return version


def probe_playwright_chromium() -> str:
    # playwright-core is a transitive dependency of @playwright/test; under
    # pnpm's isolated layout it resolves relative to @playwright/test's own
    # node_modules, not the package root.
    script = (
        "const path=require('path');"
        "const t=require.resolve('@playwright/test/package.json');"
        "const core=require.resolve('playwright-core/package.json',{paths:[path.dirname(t)]});"
        "const b=require(path.join(path.dirname(core),'browsers.json'));"
        "console.log(b.browsers.find(x=>x.name==='chromium').revision)"
    )
    out = run(["node", "-e", script], cwd=WEB_DIR)
    return out.strip()


def sqlx_library_version() -> str:
    lock = (REPO_ROOT / "Cargo.lock").read_text(encoding="utf-8")
    match = re.search(r'name = "sqlx"\nversion = "(\d+\.\d+\.\d+)"', lock)
    if not match:
        raise VerificationError("sqlx not found in Cargo.lock")
    return match.group(1)


def probe_all() -> dict[str, str]:
    values = {
        "rustc": probe_rustc(),
        "cargo": probe_cargo(),
        "node": probe_node(),
        "pnpm": probe_pnpm(),
        "python": probe_python(),
        "sqlx_cli": probe_sqlx_cli(),
        "playwright": probe_playwright(),
        "playwright_chromium": probe_playwright_chromium(),
        "agent_pi": probe_agent_pi(),
    }
    return values


def _rust_channel() -> str:
    data = tomllib.loads(RUST_TOOLCHAIN_PATH.read_text(encoding="utf-8"))
    channel = data.get("toolchain", {}).get("channel")
    if not channel:
        raise VerificationError(f"{RUST_TOOLCHAIN_PATH} does not pin a channel")
    return str(channel)


def write_manifest() -> None:
    values = probe_all()
    channel = _rust_channel()
    if values["rustc"] != channel:
        raise VerificationError(
            f"active rustc {values['rustc']} does not match pinned channel {channel}; "
            "fix rust-toolchain.toml or the active toolchain before recording the manifest"
        )
    lib = sqlx_library_version()
    cli = values["sqlx_cli"]
    if lib.rsplit(".", 1)[0] != cli.rsplit(".", 1)[0]:
        raise VerificationError(
            f"sqlx-cli {cli} does not match sqlx library {lib} (major.minor must match)"
        )
    MANIFEST_PATH.write_text(
        yaml.safe_dump(
            {"toolchain": values, "sqlx_library": lib, "rust_channel": channel},
            sort_keys=False,
        ),
        encoding="utf-8",
    )
    print(f"wrote {MANIFEST_PATH.relative_to(REPO_ROOT)}:")
    print(MANIFEST_PATH.read_text(encoding="utf-8"))


def check_manifest() -> None:
    if not MANIFEST_PATH.exists():
        raise VerificationError(
            f"{MANIFEST_PATH.relative_to(REPO_ROOT)} is missing; run "
            "`python3 ci/toolchain.py --write` to record the pinned toolchain"
        )
    recorded: dict[str, Any] = yaml.safe_load(MANIFEST_PATH.read_text(encoding="utf-8"))
    toolchain: dict[str, str] = recorded["toolchain"]
    errors: list[str] = []

    probes: dict[str, Callable[[], str]] = {
        "rustc": probe_rustc,
        "cargo": probe_cargo,
        "node": probe_node,
        "pnpm": probe_pnpm,
        "python": probe_python,
        "sqlx_cli": probe_sqlx_cli,
        "playwright": probe_playwright,
        "playwright_chromium": probe_playwright_chromium,
        "agent_pi": probe_agent_pi,
    }
    for key, probe in probes.items():
        if key not in toolchain:
            errors.append(f"manifest is missing {key!r}")
            continue
        actual = probe()
        if actual != toolchain[key]:
            errors.append(f"{key}: manifest pins {toolchain[key]}, environment has {actual}")

    if _rust_channel() != recorded.get("rust_channel"):
        errors.append(
            f"rust-toolchain.toml channel {_rust_channel()} differs from recorded "
            f"{recorded.get('rust_channel')}"
        )
    lib = sqlx_library_version()
    cli = toolchain.get("sqlx_cli", "")
    if lib.rsplit(".", 1)[0] != cli.rsplit(".", 1)[0]:
        errors.append(f"sqlx-cli {cli} does not match sqlx library {lib}")

    if errors:
        fail(errors)
    print("toolchain manifest matches the environment.")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--write", action="store_true", help="record current versions")
    group.add_argument("--check", action="store_true", help="verify against recorded versions")
    args = parser.parse_args()
    try:
        if args.write:
            write_manifest()
        else:
            check_manifest()
    except VerificationError as exc:
        fail([str(exc)])


if __name__ == "__main__":
    main()
