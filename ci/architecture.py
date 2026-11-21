"""Architecture gate: enforce the crate dependency-graph policy (RFC-0002
module ownership) over the actual Cargo manifest graph.

`python3 ci/architecture.py --check` reads `cargo metadata --no-deps`, extracts
the workspace member graph, and enforces ci/architecture.toml:

1. every workspace member has an [edges.<crate>] policy entry;
2. a member's declared internal dependencies (any kind) are a subset of its
   allowed list — undeclared edges are violations, so depending into the
   launcher binary is impossible and new members fail until reviewed;
3. crates under [forbidden_external] declare none of the listed external
   crates in any dependency kind — parley-domain cannot grow an HTTP, SQL,
   async-runtime or CLI-parsing dependency even one that is never used.

This is the manifest-level complement to the source-level import gates in
ci/trace.py: `use sqlx;` in a domain module trips the import gate, while
declaring the dependency without using it trips this one.

stdlib only, so the Rust CI lane can run it without pip installs.
"""

from __future__ import annotations

import argparse
import json
import tomllib
from pathlib import Path
from typing import Any

from common import REPO_ROOT, VerificationError, fail, run

POLICY_PATH = REPO_ROOT / "ci" / "architecture.toml"


def load_policy() -> dict[str, Any]:
    return tomllib.loads(POLICY_PATH.read_text(encoding="utf-8"))


def workspace_graph() -> tuple[set[str], dict[str, list[tuple[str, str]]]]:
    """Return (member names, member -> [(dep name, dep kind)]) from cargo metadata.

    Membership is matched by package ID (workspace_members entries are package
    IDs like `path+file:///…#0.1.0`), not by parsing the ID string.
    """
    meta = json.loads(run(["cargo", "metadata", "--no-deps", "--format-version", "1"]))
    member_ids = set(meta.get("workspace_members", []))
    members: set[str] = set()
    graph: dict[str, list[tuple[str, str]]] = {}
    for package in meta.get("packages", []):
        if package.get("id") not in member_ids:
            continue
        name = package["name"]
        members.add(name)
        graph[name] = [
            (dep["name"], dep.get("kind") or "normal")
            for dep in package.get("dependencies", [])
        ]
    return members, graph


def check() -> None:
    policy = load_policy()
    edges_policy: dict[str, list[str]] = {
        crate.replace("edges.", ""): entry.get("allowed", [])
        for crate, entry in policy.get("edges", {}).items()
    }
    forbidden: dict[str, list[str]] = {
        crate.replace("forbidden_external.", ""): entry.get("crates", [])
        for crate, entry in policy.get("forbidden_external", {}).items()
    }

    members, graph = workspace_graph()
    errors: list[str] = []

    for member in sorted(members - set(edges_policy)):
        errors.append(
            f"workspace member {member!r} has no [edges.{member}] entry in "
            "ci/architecture.toml; declare its allowed internal dependencies first"
        )

    for member in sorted(members):
        for dep_name, kind in graph.get(member, []):
            if dep_name in members:
                allowed = edges_policy.get(member, [])
                if dep_name not in allowed:
                    errors.append(
                        f"{member} ({kind} dependency) depends on {dep_name}, which is not in "
                        f"its allowed internal dependencies {allowed} — amend ci/architecture.toml "
                        "or remove the edge (RFC-0002 module ownership)"
                    )
            elif dep_name in forbidden.get(member, []):
                errors.append(
                    f"{member} ({kind} dependency) declares forbidden external crate "
                    f"{dep_name!r} — {member} must stay within its RFC-0002 boundary"
                )

    if errors:
        fail(errors)
    internal = sorted(
        f"{member} -> {dep}"
        for member, deps in graph.items()
        for dep, _kind in deps
        if dep in members
    )
    print(
        "architecture gate passed: "
        f"{len(members)} member(s), internal edges [{', '.join(internal) if internal else 'none'}], "
        "policy ci/architecture.toml enforced."
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="enforce the policy (default)")
    args = parser.parse_args()
    if not args.check:
        parser.error("--check is required")
    try:
        check()
    except VerificationError as exc:
        fail([str(exc)])


if __name__ == "__main__":
    main()
