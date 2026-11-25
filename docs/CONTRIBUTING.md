# Contributing

**Audience:** Contributors preparing a change for review.
**Purpose:** Define the local workflow, commit format, and pull request gates.
**Prerequisites:** Complete [GETTING_STARTED](GETTING_STARTED.md) and read the [glossary](README.md#glossary).

## Development setup

Set up the pinned toolchains and both workspaces as described in
[GETTING_STARTED](GETTING_STARTED.md#install).
Run `scripts/check-toolchain` before your first change.
Use a scratch Workspace for development; never point the app at Owner data.

## Branching

[ASSUMED] Work on short-lived branches from the default branch.
Merge through pull requests.

## Commit convention

[ASSUMED] Use Conventional Commits.
Write the type, optional scope, and imperative subject:

```text
feat(queue): add phase filter to the sheet
fix(entities): preserve notes bytes on conflict
docs: document the local session headers
```

Keep the subject under 72 characters.
Use `feat`, `fix`, `docs`, `test`, `refactor`, `chore`, or `ci`.
Add a body when the reason is not obvious from the subject.
## Style and lint

Run these from the repository root before pushing:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
pnpm --dir web exec vue-tsc --noEmit
pnpm --dir web exec eslint . --max-warnings 0
```

Rust warnings fail CI.
Frontend lint warnings fail CI.
Do not add a suppression without a reason in the same change.

The domain crate cannot gain HTTP, SQL, IO, or async-runtime dependencies.
`ci/architecture.py` and `ci/trace.py` enforce ownership boundaries.
See [ARCHITECTURE](ARCHITECTURE.md#boundary-rule).

## Pull request checklist

- [ ] The change has a clear problem statement and a test.
- [ ] Tests pass locally; see [TESTING](TESTING.md).
- [ ] `scripts/check-slice <slice>` passes for affected Slices.
- [ ] `scripts/prove <slice>` passes with fresh evidence.
- [ ] New or changed endpoints have contract fixtures.
- [ ] Migrations are ordered, checksummed, and reversible through a verified backup. Plan slices allocate below `0900`; out-of-plan work uses the reserved band `0900`–`0999` and is registered in `ci/migrations.py`.
- [ ] No Workspace file, fixture secret, or private text is committed.
- [ ] Docs that own an affected topic are updated in the same PR.
- [ ] Commit subjects follow the convention above.

## CI gates

Two independent pull request lanes run in `.github/workflows/`.

| Lane | Gates |
|---|---|
| `rust` | rustfmt, Clippy with `-D warnings`, `cargo test`, disk, architecture, coverage, migrations |
| `web-mock` | frozen install, `vue-tsc`, ESLint, Vitest, Vite build, coverage, disk, contract, trace, Playwright mock |

The full local gate set is:

```sh
scripts/check-slice s01
scripts/prove s01
```

Replace `s01` with the Slice you changed.
Both scripts fail when evidence is missing or reports show zero executed tests.

Coverage never regresses against `ci/coverage-baseline.toml`.
The committed floors rise toward the 70% minimum as Slices land.
See [TESTING](TESTING.md#coverage).

## Reporting issues

Include the following in a bug report:

- The Slice and tab where you saw the behavior.
- The exact command or UI path you ran.
- Expected result and observed result.
- The local build identifier and `wire_major`.
- A redacted diagnostic export, if available.

Never attach a Workspace file or raw Owner text.
Use synthetic fixtures for reproductions.

Feature proposals should name the Owner task, the affected components, and the test that would prove it.

## Releases

Use `scripts/release` to qualify an internal Linux launcher archive locally. A version tag runs the same checks and stages a draft with checksums and workspace recovery evidence. [Release process](RELEASING.md)

## Community

By participating you agree to the [CODE_OF_CONDUCT](../CODE_OF_CONDUCT.md).

## Related docs

- [TESTING](TESTING.md) — test layers and commands.
- [OPERATIONS](OPERATIONS.md) — maintenance and recovery behavior.
