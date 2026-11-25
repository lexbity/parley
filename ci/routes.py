"""TD-10 route inventory generator and gate.

Emits a machine-readable inventory of every axum route registered by the
service: path, HTTP methods, defining module, dispatch class today, assigned
wave and exemption. The committed artifact is the migration checklist and the
oracle for the registry-completeness gate.

Usage:
  python3 ci/routes.py --write     regenerate contract/fixtures/td10/route-inventory.json
  python3 ci/routes.py --check     fail if the committed artifact is stale
  python3 ci/routes.py --self-test-negative    prove a planted route fails the gate
"""

from __future__ import annotations

import argparse
import json
import re
import tempfile
from pathlib import Path

import yaml

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
    (re.compile(r"^(lib|http)\.rs$"), "legacy-axum", "1"),
)


def module_of(path: Path) -> str:
    return path.relative_to(SRC).as_posix()


# TD-10 W4: handler-owner map used to keep the inventory's `module` field
# truthful now that every registration lives in `commands/http/`. The handler
# prefix in the registration names the owning module (the "descriptor table
# beside the owning module" rule), so the wave plan and review units stay
# intact even though the route table itself is centralized.
HANDLER_OWNER = {
    "board": "board.rs",
    "materials": "materials.rs",
    "relations": "relations.rs",
    "parties": "parties.rs",
    "thread": "thread.rs",
    "jobs": "jobs.rs",
    "queueing": "queueing.rs",
    "queue_sheet": "queue_sheet.rs",
    "assistant": "assistant.rs",
    "writing": "writing.rs",
    "maintenance": "maintenance.rs",
    "blob": "blob.rs",
    "release": "release.rs",
    "agent_runtime::http": "agent_runtime/http.rs",
    "resource_board::http": "resource_board/http.rs",
    "activity": "inference/activity.rs",
    "http": "http.rs",
}


def owner_module_of(call: str, fallback: str) -> str:
    """Map a handler qualifier in a route call back to its owning module."""
    match = re.search(r"\b(\w+(?:::\w+)?)::(\w+)", call)
    if match:
        prefix = match.group(1)
        if prefix in HANDLER_OWNER:
            return HANDLER_OWNER[prefix]
        if prefix.startswith("resource_board::"):
            return "resource_board/http.rs"
        if prefix.startswith("agent_runtime::"):
            return "agent_runtime/http.rs"
    return fallback


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
            source_file = module
            owner = owner_module_of(call, module)
            dispatch, wave, exemption = classify(path, owner)
            for method in route_methods(call):
                routes.append(
                    {
                        "path": path,
                        "method": method,
                        "module": owner,
                        "source": source_file,
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


def verify_placement(inventory: dict) -> None:
    """AC-11 / FR-15: every `/api/v1` registration lives in `commands/http/`.

    The consolidated transport table is the only place route strings may
    appear; any `/api/v1` `.route(` anywhere else in the tree is a legacy
    transport resurrection and fails the gate.
    """
    offenders = sorted(
        {
            item["source"]
            for item in inventory["routes"]
            if item["path"].startswith("/api/v1") and not item["source"].startswith("commands/http/")
        }
    )
    if offenders:
        raise VerificationError(
            "legacy transport placement violation: /api/v1 registrations found outside "
            f"commands/http/ in {offenders}"
        )


def verify_openapi_mapping(inventory: dict) -> None:
    """FR-15: every `/api/v1` route maps to an OpenAPI operation.

    Path-parameter names are not wire-significant (`{id}` vs `{list_id}`),
    so comparison strips parameter names after normalizing the `/api/v1`
    prefix the OpenAPI document omits.
    """
    openapi = REPO_ROOT / "contract" / "openapi.yaml"
    if not openapi.exists():
        raise VerificationError("contract/openapi.yaml is missing; cannot verify route mapping")
    spec = yaml.safe_load(openapi.read_text(encoding="utf-8"))
    paths = spec.get("paths", {})
    strip = lambda p: re.sub(r"\{[^}]+\}", "{}", p)
    missing = []
    for item in inventory["routes"]:
        path = item["path"]
        if not path.startswith("/api/v1"):
            continue
        rel = path[len("/api/v1"):]
        normalized = strip(rel)
        found = any(
            strip(existing) == normalized and item["method"].lower() in operations
            for existing, operations in paths.items()
        )
        if not found:
            missing.append((item["method"], path))
    if missing:
        raise VerificationError(
            "route/openapi mapping incomplete; these operations are absent from "
            f"contract/openapi.yaml: {sorted(missing)}"
        )


def negative_self_test() -> None:
    """FR-15 negative control: a planted unmapped route must fail the gate."""
    planted = SRC / "td10_planted_probe.rs"
    planted.write_text(
        'use axum::routing::post;\n'
        'pub fn planted() -> axum::Router<()> { axum::Router::new().route("/api/v1/planted", post(|| async {})) }\n',
        encoding="utf-8",
    )
    try:
        inventory = build_inventory()
        ok = True
        try:
            verify_placement(inventory)
        except VerificationError:
            ok = False
        if ok:
            raise VerificationError("negative self-test failed: planted route outside commands/http/ passed placement")
        print("negative self-test passed: planted route fails the placement gate.")
    finally:
        planted.unlink(missing_ok=True)


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
    group.add_argument(
        "--self-test-negative",
        action="store_true",
        help="prove a planted route outside commands/http/ fails the placement gate",
    )
    args = parser.parse_args()
    generated = json.dumps(build_inventory(), indent=2, sort_keys=True) + "\n"
    if args.write:
        INVENTORY.parent.mkdir(parents=True, exist_ok=True)
        INVENTORY.write_text(generated, encoding="utf-8")
        print(f"wrote {INVENTORY.relative_to(REPO_ROOT)}")
        return
    if args.self_test_negative:
        negative_self_test()
        return
    if not INVENTORY.exists():
        raise VerificationError("route inventory is missing; run `python3 ci/routes.py --write`")
    if INVENTORY.read_text(encoding="utf-8") != generated:
        raise VerificationError(
            "route inventory is stale; run `python3 ci/routes.py --write` and commit the artifact"
        )
    inventory = json.loads(generated)
    verify_placement(inventory)
    verify_openapi_mapping(inventory)
    verify_destructive_set(inventory)
    print("route inventory, placement, openapi mapping and destructive set verified.")


if __name__ == "__main__":
    try:
        main()
    except VerificationError as exc:
        fail([str(exc)])
