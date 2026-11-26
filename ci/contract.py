"""Contract verifier for Parley (RFC-0004 I-FR-4, "Build, verification and lint infrastructure").

For `--through SLICE --check-generated` this script:

1. requires a coverage row (contract/coverage.yaml) for every slice up to SLICE;
   a slice without rows is unimplemented and must fail, never pass vacuously,
2. validates each row's fields and that its fixture directory
   artifact exist,
3. requires contract/openapi.yaml once any covered slice declares operations,
   and that every row's operationId exists in the contract,
4. requires at least one valid and one invalid example per covered operation
   (I-FR-4), parses every fixture JSON, and validates fixtures that carry a
   `$schema` reference against the contract with jsonschema,
5. with `--check-generated`, regenerates web/src/api/generated.ts from the
   contract via openapi-typescript and requires it to be byte-identical to the
   committed file.
"""

from __future__ import annotations

import argparse
import json
import tempfile
from pathlib import Path
from typing import Any

import jsonschema
import yaml

from common import (
    REPO_ROOT,
    VerificationError,
    coverage_rows,
    fail,
    load_coverage,
    run,
    slices_through,
)

CONTRACT_DIR = REPO_ROOT / "contract"
FIXTURES_DIR = CONTRACT_DIR / "fixtures"
OPENAPI_PATH = CONTRACT_DIR / "openapi.yaml"
GENERATED_PATH = REPO_ROOT / "web" / "src" / "api" / "generated.ts"

REQUIRED_ROW_FIELDS = (
    "requirement",
    "operationId",
    "test_id",
    "fixture_id",
    "owner_role",
    "package_target",
)


def load_openapi() -> dict[str, Any]:
    if not OPENAPI_PATH.exists():
        raise VerificationError(
            "contract/openapi.yaml is missing; the owning slice must add the contract "
            "fragment it implements (RFC-0004 endpoint catalogue)"
        )
    doc = yaml.safe_load(OPENAPI_PATH.read_text(encoding="utf-8"))
    if not isinstance(doc, dict) or "paths" not in doc:
        raise VerificationError("contract/openapi.yaml is not an OpenAPI document with paths")
    return doc


def openapi_operation_ids(doc: dict[str, Any]) -> set[str]:
    ids: set[str] = set()
    for path, methods in doc["paths"].items():
        for method, operation in methods.items():
            if method.lower() in {"get", "post", "put", "patch", "delete"} and isinstance(
                operation, dict
            ):
                operation_id = operation.get("operationId")
                if operation_id:
                    ids.add(operation_id)
                else:
                    raise VerificationError(
                        f"operation {method.upper()} {path} has no operationId (RFC-0004 INT "
                        "contract requires operation IDs)"
                    )
    return ids


def resolve_schema_ref(doc: dict[str, Any], ref: str) -> dict[str, Any]:
    node: Any = doc
    for part in ref.lstrip("#/").split("/"):
        if not isinstance(node, dict) or part not in node:
            raise VerificationError(f"fixture $schema reference {ref!r} not found in contract")
        node = node[part]
    return node


def expand_schema_refs(
    schema: Any, doc: dict[str, Any], seen: frozenset[str] = frozenset()
) -> Any:
    """Expand local JSON Schema references while preserving sibling keywords.

    Fixture schemas are stored under components in the OpenAPI document, but
    jsonschema receives a detached component. Resolve those references against
    the complete contract before validating the fixture.
    """
    if isinstance(schema, list):
        return [expand_schema_refs(item, doc, seen) for item in schema]
    if not isinstance(schema, dict):
        return schema
    reference = schema.get("$ref")
    if reference is not None:
        reference = str(reference)
        if not reference.startswith("#/") or reference in seen:
            raise VerificationError(f"unsupported or recursive fixture schema reference: {reference}")
        target = expand_schema_refs(
            resolve_schema_ref(doc, reference), doc, seen | {reference}
        )
        siblings = {
            key: expand_schema_refs(value, doc, seen)
            for key, value in schema.items()
            if key != "$ref"
        }
        if isinstance(target, dict):
            return {**target, **siblings}
        if siblings:
            raise VerificationError(f"fixture schema reference {reference} has non-object siblings")
        return target
    return {key: expand_schema_refs(value, doc, seen) for key, value in schema.items()}


def validate_coverage(through: str) -> list[str]:
    """Return the slice IDs that declare coverage rows up to `through`."""
    coverage = load_coverage()
    declared: list[str] = []
    errors: list[str] = []
    for slice_id in slices_through(through):
        rows = coverage_rows(coverage, slice_id)
        if not rows:
            errors.append(
                f"slice {slice_id} declares no coverage rows; add one row per operation and "
                "requirement to contract/coverage.yaml (RFC-0004). A slice without rows is "
                "unimplemented and must fail verification."
            )
            continue
        declared.append(slice_id)
        for index, row in enumerate(rows):
            if not isinstance(row, dict):
                errors.append(f"coverage row {index} of {slice_id} is not a mapping")
                continue
            missing = [field for field in REQUIRED_ROW_FIELDS if field not in row]
            if missing:
                errors.append(
                    f"coverage row {index} of {slice_id} is missing fields: {', '.join(missing)}"
                )
                continue
            fixture_dir = FIXTURES_DIR / slice_id / str(row["fixture_id"])
            if not fixture_dir.is_dir():
                errors.append(
                    f"{slice_id} row {index}: fixture directory {fixture_dir.relative_to(REPO_ROOT)} "
                    "does not exist"
                )
    return declared, errors  # type: ignore[return-value]


def validate_fixtures(declared: list[str], operation_ids: set[str]) -> list[str]:
    errors: list[str] = []
    doc = load_openapi()
    for slice_id in declared:
        coverage = load_coverage()
        for row in coverage_rows(coverage, slice_id):
            op = str(row["operationId"])
            fixture_dir = FIXTURES_DIR / slice_id / str(row["fixture_id"])
            if op == "n/a" or not fixture_dir.is_dir():
                continue
            if op not in operation_ids:
                errors.append(
                    f"{slice_id} row: operationId {op!r} does not exist in contract/openapi.yaml"
                )
                continue
            valid = sorted(fixture_dir.glob(f"{op}.valid*.json"))
            invalid = sorted(fixture_dir.glob(f"{op}.invalid*.json"))
            if not valid:
                errors.append(
                    f"{fixture_dir.relative_to(REPO_ROOT)} has no {op}.valid*.json example "
                    "(I-FR-4: every catalog operation needs valid and invalid examples)"
                )
            if not invalid:
                errors.append(
                    f"{fixture_dir.relative_to(REPO_ROOT)} has no {op}.invalid*.json example "
                    "(I-FR-4)"
                )
            for fixture in valid + invalid:
                try:
                    payload = json.loads(fixture.read_text(encoding="utf-8"))
                except json.JSONDecodeError as exc:
                    errors.append(f"{fixture.relative_to(REPO_ROOT)} is not valid JSON: {exc}")
                    continue
                schema_ref = payload.get("$schema") if isinstance(payload, dict) else None
                if schema_ref:
                    schema = expand_schema_refs(
                        resolve_schema_ref(doc, str(schema_ref)), doc
                    )
                    try:
                        jsonschema.validate(payload.get("body", payload), schema)
                    except jsonschema.ValidationError as exc:
                        errors.append(
                            f"{fixture.relative_to(REPO_ROOT)} fails its declared schema: "
                            f"{exc.message}"
                        )
    return errors


def check_generated_client() -> None:
    if not GENERATED_PATH.exists():
        raise VerificationError(
            "web/src/api/generated.ts is missing; generate it from the contract with "
            "`pnpm --dir web exec openapi-typescript ../contract/openapi.yaml -o src/api/generated.ts`"
        )
    with tempfile.TemporaryDirectory(prefix="parley-contract-") as tmp:
        candidate = Path(tmp) / "generated.ts"
        run(
            [
                "pnpm",
                "--dir",
                "web",
                "exec",
                "openapi-typescript",
                str(OPENAPI_PATH),
                "-o",
                str(candidate),
            ]
        )
        committed = GENERATED_PATH.read_bytes()
        regenerated = candidate.read_bytes()
        if committed != regenerated:
            raise VerificationError(
                "web/src/api/generated.ts is stale; regenerate it from contract/openapi.yaml "
                "and commit the result (--check-generated requires byte equality)"
            )


def verify_through(through: str, check_generated: bool) -> None:
    declared, errors = validate_coverage(through)
    operation_ids: set[str] = set()
    if declared:
        operation_ids = openapi_operation_ids(load_openapi())
        errors.extend(validate_fixtures(declared, operation_ids))
    if check_generated and declared:
        check_generated_client()
    if errors:
        fail(errors)
    print(f"contract verified through {through}: {len(declared)} covered slice(s).")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--through", required=True, metavar="SLICE", help="highest slice ID")
    parser.add_argument(
        "--check-generated",
        action="store_true",
        help="require web/src/api/generated.ts to match the contract exactly",
    )
    args = parser.parse_args()
    try:
        verify_through(args.through, args.check_generated)
    except VerificationError as exc:
        fail([str(exc)])


if __name__ == "__main__":
    main()
