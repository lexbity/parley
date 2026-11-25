"""TD-10 route inventory generator and gate.

Emits a machine-readable inventory of every axum route registered by the
service: path, HTTP methods, defining module, dispatch class today, assigned
wave and exemption. The committed artifact is the migration checklist and the
oracle for the registry-completeness gate.

Usage:
  python3 ci/routes.py --write     regenerate contract/fixtures/td10/route-inventory.json
  python3 ci/routes.py --check     fail if the committed artifact is stale
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

from common import REPO_ROOT, VerificationError, fail

SRC = REPO_ROOT / "crates" / "parley-service" / "src"
INVENTORY = REPO_ROOT / "contract" / "fixtures" / "td10" / "route-inventory.json"
DESTRUCTIVE_SET = REPO_ROOT / "contract" / "fixtures" / "td10" / "destructive-set.json"

ROUTE_RE = re.compile(r'\.route\(\s*"([^"]+)"')
METHOD_RE = re.compile(r"\b(get|post|put|patch|delete|head)\s*\(")

# Exempt classes are enumerated, not open-ended (TD-001 Q11).
EXEMPT_RULES = (
    (re.compile(r"^/api/v1/command-notifications$"), "sse-stream"),
    (re.compile(r"^/api/v1/materials/\{id\}/download$"), "media-bytes"),
    (re.compile(r"^/api/v1/session(/bootstrap)?$"), "session-bootstrap"),
    (re.compile(r"^/api/v1/health$"), "session-bootstrap"),
    (re.compile(r"^/(?!api/v1/)"), "embedded-ui"),
)

# TD-10 W3: conservative destructive set. These routes change or replace
# durable owner facts and must stop at the approval inbox with a frozen intent
# digest once their transport converges. Previews and reads are not destructive.
DESTRUCTIVE = (
    (re.compile(r"^/api/v1/parties/\{id\}/erase$"), "POST"),
    (re.compile(r"^/api/v1/restore/commit$"), "POST"),
    (re.compile(r"^/api/v1/settings/maintenance$"), "PUT"),
    (re.compile(r"^/api/v1/resource-cards/\{id\}/retire$"), "POST"),
    (re.compile(r"^/api/v1/imports/commit$"), "POST"),
)

# Ownership of the non-registry transport. The wave assignment is the TD-10
# migration plan: reads first, core mutations, then writing/release/admin.
WAVE_RULES = (
    (re.compile(r"^commands/http/"), "registry", "0"),
    (
        re.compile(r"^(agent_runtime|assistant|inference|jobs)(\.rs|/)"),
        "legacy-axum",
        "3",
    ),
    (
        re.compile(r"^(maintenance|materials|release|blob|writing|queueing)(\.rs|/)"),
        "legacy-axum",
        "3",
    ),
    (
        re.compile(r"^(parties|board|relations|queue_sheet)(\.rs|/)"),
        "legacy-axum",
        "2",
    ),
    (
        re.compile(r"^resource_board(\.rs|/)"),
        "legacy-axum",
        "2",
    ),
    (re.compile(r"^lib\.rs$"), "legacy-axum", "1"),
)


def module_of(path: Path) -> str:
    return path.relative_to(SRC).as_posix()


def route_methods(call: str) -> list[str]:
    """Methods named on a `.route(path, ...)` handler expression."""
    methods = [name.upper() for name in METHOD_RE.findall(call)]
    return list(dict.fromkeys(methods)) or ["GET"]


def is_destructive(path: str, method: str) -> bool:
    return any(pattern.fullmatch(path) and expected == method for pattern, expected in DESTRUCTIVE)


def classify(path: str, module: str) -> tuple[str, str, str]:
    for pattern, reason in EXEMPT_RULES:
        if pattern.search(path):
            return "exempt", "—", reason
    for pattern, dispatch, wave in WAVE_RULES:
        if pattern.search(module):
            return dispatch, wave, "—"
    return "legacy-axum", "2", "—"


def build_inventory() -> dict:
    routes: list[dict] = []
    for source in sorted(SRC.rglob("*.rs")):
        text = source.read_text(encoding="utf-8")
        module = module_of(source)
        for match in ROUTE_RE.finditer(text):
            start = match.start()
            depth = 0
            end = start
            for index in range(start, len(text)):
                if text[index] == "(":
                    depth += 1
                elif text[index] == ")":
                    depth -= 1
                    if depth == 0:
                        end = index + 1
                        break
            call = text[start:end]
            path = match.group(1)
            dispatch, wave, exemption = classify(path, module)
            for method in route_methods(call):
                routes.append(
                    {
                        "path": path,
                        "method": method,
                        "module": module,
                        "dispatch": dispatch,
                        "wave": wave,
                        "exempt": exemption,
                        "destructive": is_destructive(path, method),
                    }
                )
    routes.sort(key=lambda item: (item["path"], item["method"], item["module"]))
    non_exempt = [item for item in routes if item["exempt"] == "—"]
    registry = [item for item in non_exempt if item["dispatch"] == "registry"]
    return {
        "schema_version": 1,
        "slices": ["TD-10"],
        "summary": {
            "total": len(routes),
            "exempt": len(routes) - len(non_exempt),
            "non_exempt": len(non_exempt),
            "registry_today": len(registry),
        },
        "routes": routes,
    }


def verify_destructive_set(inventory: dict) -> None:
    """FR-14: the enumerated destructive set and the inventory must agree."""
    if not DESTRUCTIVE_SET.exists():
        raise VerificationError("destructive set is missing; add contract/fixtures/td10/destructive-set.json")
    declared = {
        (item["path"], item["method"])
        for item in json.loads(DESTRUCTIVE_SET.read_text(encoding="utf-8"))["routes"]
    }
    marked = {
        (item["path"], item["method"])
        for item in inventory["routes"]
        if item["destructive"]
    }
    if declared != marked:
        raise VerificationError(
            "destructive set disagrees with the route inventory: "
            f"only in artifact={sorted(declared - marked)}, only in inventory={sorted(marked - declared)}"
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--write", action="store_true", help="regenerate the committed inventory")
    group.add_argument("--check", action="store_true", help="require the committed inventory to be current")
    args = parser.parse_args()
    generated = json.dumps(build_inventory(), indent=2, sort_keys=True) + "\n"
    if args.write:
        INVENTORY.parent.mkdir(parents=True, exist_ok=True)
        INVENTORY.write_text(generated, encoding="utf-8")
        print(f"wrote {INVENTORY.relative_to(REPO_ROOT)}")
        return
    if not INVENTORY.exists():
        raise VerificationError("route inventory is missing; run `python3 ci/routes.py --write`")
    if INVENTORY.read_text(encoding="utf-8") != generated:
        raise VerificationError(
            "route inventory is stale; run `python3 ci/routes.py --write` and commit the artifact"
        )
    verify_destructive_set(json.loads(generated))
    print("route inventory and destructive set verified.")


if __name__ == "__main__":
    try:
        main()
    except VerificationError as exc:
        fail([str(exc)])
