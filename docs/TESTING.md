# Testing

**Audience:** Contributors adding or changing tests.
**Purpose:** Describe the test layers, commands, layout, and quality gates.
**Prerequisites:** Complete [GETTING_STARTED](GETTING_STARTED.md#install) and read the [glossary](README.md#glossary).

## Test pyramid

The suite is layered from pure rules to full journeys.

| Layer | Location | What it proves |
|---|---|---|
| Domain unit | `crates/parley-domain/src/*.rs` | Gate, lifecycle, evidence, matching, and queue rules with no IO |
| Service integration | `crates/parley-service/tests/*.rs` | Real Axum and SQLite against a temporary Workspace |
| Fuzz | `fuzz/fuzz_targets/` | Malformed and oversized command input is rejected before writes |
| Frontend component | `web/src/**/*.spec.ts` | Client decoding, state, and rendering |
| Browser mock | `web/e2e/*.spec.ts`, `--project=mock` | Interactive journeys against the development mock |
| Browser integrated | `web/e2e/*.spec.ts`, `--project=rust` | The same journeys against the real service |
| Accessibility | `web/e2e/accessibility.spec.ts` | Keyboard reachability and audit findings |
| Contract and trace | `ci/contract.py`, `ci/trace.py` | Fixtures, coverage manifest, and executed evidence |

Each Slice carries its own tests and evidence.
A missing test must never pass vacuously.

## Run commands

From the repository root:

```sh
cargo test --locked --workspace
cargo test --locked -p parley-service --test s01
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
pnpm install --frozen-lockfile
pnpm --dir web run typecheck
pnpm --dir web run lint
pnpm --dir web run test
pnpm --dir web run build
pnpm --dir web run e2e:mock
pnpm --dir web run e2e:integrated
pnpm --dir web run e2e:a11y
```

For a single Slice, use the verification wrappers:

```sh
scripts/check-slice s01
scripts/prove s01
```

`scripts/prove` runs the slice's Rust test, both browser projects, and a trace check that requires fresh reports.
Reports are written under `artifacts/prove-<slice>.*`.
See [GETTING_STARTED](GETTING_STARTED.md#verification) for the full local sequence.

## Layout and naming

- Rust integration tests use the Slice ID: `s01.rs`, `s12_queue_sheet.rs`, `p01.rs`, `r01.rs`, `ar09.rs`.
- Frontend component tests sit beside the module as `<name>.spec.ts`.
- Browser specs use the Slice ID in `web/e2e/`, for example `s01.spec.ts`.
- Fixtures live under `contract/fixtures/<slice>/<area>/`.
- Each fixture has a `.valid.json` and an `.invalid.json` pair.

Test workspaces and scratch databases go to OS temporary directories.
Browser fixtures dispose of the server and Workspace after each test.
Never create a database artifact in the working tree.
`ci/disk.py --check` enforces this.

## Example tests

A service integration test starts a real service on a loopback port with a temporary Workspace.
The helper below comes from `crates/parley-service/tests/s01.rs`:

```rust
fn workspace_path() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "parley-s01-{}-{}.sqlite",
        std::process::id(),
        rand::random::<u64>()
    ));
    let directory = path.with_extension("workspace");
    std::fs::create_dir_all(&directory).expect("create private test workspace directory");
    directory.join("workspace.sqlite")
}
```

A component test mocks the transport and asserts client decoding.
The shape below comes from `web/src/api/client.spec.ts`:

```ts
it('rejects malformed opportunity responses', async () => {
  vi.spyOn(globalThis, 'fetch')
    .mockResolvedValueOnce(sessionResponse())
    .mockResolvedValueOnce(response([{ id: 'op', participant_ids: [7] }]));
  const client = new ParleyClient();
  await client.connect();
  await expect(client.listOpportunities()).rejects.toThrow('malformed');
});
```

Both examples use synthetic data only.
Do not copy Owner text into fixtures.

## Coverage

Coverage never regresses against `ci/coverage-baseline.toml`.
Per-crate floors rise toward a 70% minimum as Slices land.
Rust uses `cargo llvm-cov --json` over each crate's `src`.
Web uses Vitest v8 coverage, excluding `src/main.ts`.
A crate with no measurable regions is exempt until code lands.

```sh
python3 ci/coverage.py --check --rust --web
python3 ci/coverage.py --update-baseline --rust --web
```

Update the baseline deliberately and commit it with the change that caused it.
A baseline bump without a test change is a review finding.

## Negative controls

Several gates assert that a forbidden behavior is absent.
`scripts/absent PATTERN PATH...` succeeds only when the pattern is not found.
Source-pattern gates are paired with behavioral negative controls.
Do not satisfy a gate by adding an empty or stub file.

[ASSUMED] New behavior gets a failing test before the implementation.

## Test data

- Use the private test clock for deadline cases.
- Pin timestamps and IDs in fixtures.
- Normalize only allocated IDs when comparing observations.
- Compare refusals and event counts, not just final state.
- Keep canary strings out of ordinary logs.

See [API](API.md) for the routes the fixtures exercise.
See [ARCHITECTURE](ARCHITECTURE.md#decisions) for the mock parity decision.

## Related docs

- [CONTRIBUTING](CONTRIBUTING.md#pull-request-checklist) — required checks before a PR.
- [OPERATIONS](OPERATIONS.md) — migration and backup verification.
