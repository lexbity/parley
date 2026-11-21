# Parley

Single-user, local-only outreach system. The normative specifications live in
`devdocs/` (RFC-0001 application, RFC-0002 Rust authority, RFC-0003 Vue owner
interface, RFC-0004 integration and qualification). `devdocs/` stays
untracked per `AGENTS.md`.

## Status

Toolchain, lint and verification harness (RFC-0004 S01 build infrastructure):
**complete**. Feature slices S01–S12: **not started** — `scripts/check-slice s01`
correctly reports the missing slice artifacts (contract fixtures, migration
`0001_workspace.sql`, coverage rows, `tests/s01.rs`, `e2e/s01.spec.ts`) until
slice S01 lands. There are no stubs in between: gates fail loudly, toolchain
steps pass.

## Toolchain pins (RFC-0002 BE-01, RFC-0004 S01)

| Tool | Pin | Enforced by |
|---|---|---|
| Rust | 1.98.1 | `rust-toolchain.toml`, `ci/toolchain.py` |
| SQLx CLI | 0.9.1 (matches sqlx crate 0.9.1) | `ci/migrations.py`, `ci/toolchain.py` |
| Node | 26.12.0 | `.nvmrc`, `engines` |
| pnpm | 12.21.0 | `packageManager` field |
| Playwright | 1.65.1, Chromium build 1255 | `web/package.json`, `ci/toolchain-versions.yaml` |
| pi agent | `@earendil-works/pi-coding-agent` 1.6.0 | `agent/package.json`, `ci/toolchain-versions.yaml` |

Run `scripts/check-toolchain` to verify the environment still matches
`ci/toolchain-versions.yaml`.

## Agent runner

**pi is the first supported agent runner** (owner decision; RFC-0002 BE-09
permits a pi-compatible runner behind the bounded adapter). The runner is
pinned as a project dependency in `agent/`
(`@earendil-works/pi-coding-agent`); Ollama remains the default *local
provider* behind the adapter boundary per RFC-0001 §2 — pi drives the model,
the adapter port (typed run/cancel/health) stays identical. The adapter
integration lands in RFC-0004 slice S04; until then the dependency is pinned
and version-tracked only. Its transitive postinstall scripts are denied in
`pnpm-workspace.yaml`, matching upstream's documented
`--ignore-scripts` install guidance.

## Commands

```sh
scripts/check-slice s01            # cumulative verification through slice s01
scripts/prove s01                  # per-slice proof: cargo + playwright mock/rust + trace
scripts/absent PATTERN PATH...     # forbidden-pattern grep gate
scripts/check-toolchain            # toolchain manifest drift check
scripts/prune                      # disk hygiene (see below)
```

Frontend-only development (no Rust, no model required except `e2e:integrated`):

```sh
pnpm install --frozen-lockfile
pnpm --dir web run dev            # vite dev server
pnpm --dir web run typecheck      # vue-tsc --noEmit
pnpm --dir web run lint           # eslint, zero warnings
pnpm --dir web run test           # vitest
pnpm --dir web run build          # vite build (emits manifest for asset proofs)
pnpm --dir web run e2e:mock       # playwright against the dev mock (S01+)
pnpm --dir web run e2e:integrated # playwright against the Rust service (S01+)
```

## Quality gates

**Coverage ratchet** (`ci/coverage.py`, state in `ci/coverage-baseline.toml`):
coverage never regresses against the committed baseline, and per-crate floors
rise toward the **70% minimum** (`floor_target`) as slice code lands. Rust is
measured with `cargo llvm-cov --json` per `crates/<name>/src`; web with vitest
v8 coverage (`src/main.ts` excluded — entry wiring is e2e territory). A crate
with no measurable regions (doc-only scaffold) is exempt until code lands.

```sh
python3 ci/coverage.py --check --rust --web       # enforce (used by check-slice and CI)
python3 ci/coverage.py --update-baseline --rust --web   # re-measure; commit deliberately
```

**Architecture gate** (`ci/architecture.py`, policy in `ci/architecture.toml`):
enforces the RFC-0002 module-ownership boundaries over the *declared* Cargo
dependency graph (`cargo metadata`), complementing the source-level import
gates in `ci/trace.py`. Every workspace member needs a policy entry; internal
edges are an allowlist (nothing may depend into `parley-cli`;
`parley-service → parley-domain → ∅` only); `parley-domain` cannot declare
forbidden external crates (HTTP, SQL, async runtime, CLI parsing) even when
unused. A new crate fails until its policy entry is reviewed and committed.

## Disk hygiene

Development and testing must not grow disk usage unboundedly. The guarantee
has two halves:

**Bounded by construction.** Test workspaces and scratch databases are created
in OS temp directories, never in the repo. Per-slice prove artifacts
(`artifacts/cargo-test-*.log`, `artifacts/playwright-*.json`) are overwritten
on every run, and Playwright cleans its output dir at the start of each run —
so artifacts stay bounded by slice count and per-run size. The only vector
expected to reach multiple GiB is Rust's `target/` build cache.

**Bounded by gate.** `ci/disk.py --check` runs inside `scripts/check-slice`
(placed before the slice-gated verifiers, so it executes even while slice work
is red) and in both CI lanes. It fails if any database artifact
(`*.sqlite`, `*.db`, `*-wal`, `*-shm`) lands in the working tree outside
`contract/fixtures`, and it enforces the warn/fail budgets in
`ci/disk-budgets.yaml` for `target/`, test artifacts and the repo tree.

```sh
python3 ci/disk.py --report      # sizes of every growth vector, incl. global caches
./scripts/prune --dry-run        # list what would be freed
./scripts/prune                  # cargo clean + test artifacts + stale /tmp/parley-*
                                 # + pnpm store prune (unreferenced packages only)
./scripts/prune --playwright-cache --registry   # opt-in machine-global caches
```

`--playwright-cache` removes browser builds unused by the pinned Playwright
version; other projects on this machine may need `playwright install` again.
`--registry` removes cargo registry downloads and extracted sources; they are
re-fetched on demand. Product-level retention (raw-text retention, backup
generations, erasure) is specified by RFC-0002 BE-13 and lands with slice S11.

## Layout

| Path | Owns |
|---|---|
| `crates/parley-domain` | Pure decision core: gates, lifecycle, matching, pack policy (no HTTP/SQL/IO — enforced by `ci/trace.py`) |
| `crates/parley-service` | Commands, SQLite persistence, blob store, agent worker, Axum transport |
| `crates/parley-cli` | `parley` launcher binary |
| `web/` | Vue 3 + TypeScript owner interface (single pnpm package) |
| `contract/` | OpenAPI, coverage manifest, per-slice fixtures |
| `migrations/` | SQLx migrations (owned per slice) |
| `ci/` | Project-owned verifiers: contract, migrations, trace, toolchain |
| `scripts/` | check-slice, prove, absent, check-toolchain |
| `.github/workflows/` | Rust-only and mock-only PR lanes |
