# Code-review fixes · migration collision, trigger guard, god-file splits

Evidence for `devdocs/CODE_REVIEW_god_files_and_triggers.md` (2026-11-25).
All moves are verbatim relocations with visibility adjustments only; no
behavior, paths, payloads or wire shapes changed.

## 1. Duplicate 0035 — not present in this tree

The write-up's blocking defect does not reproduce on the reviewed branch:

- `migrations/` contains exactly one version-35 file:
  `0035_thread_read_index.sql`, owned by `s07`.
- `python3 -B ci/migrations.py --through s11` passes (12 declared migrations,
  highest 0035, 35 cumulative prefixes applied, `cargo sqlx prepare --check`
  green). `--check-bands` also passes with 37 files present.
- The colliding `0035_material_updated_index.sql` exists only on
  `origin/frontend` (commit `2dab9ef`). On this branch its index is already
  provided by `0900_debt_read_indexes.sql` (TD-04, owned by
  `DEBT_MIGRATIONS`), so no renumber or data-only follow-up is needed.

No migration was changed or reverted.

## 2. Trigger inventory guard

- `crates/parley-service/tests/fixtures/expected_triggers.json` pins the final
  inventory: **38 triggers**, name plus whitespace-normalised body, generated
  by applying every migration in numeric order.
- `crates/parley-service/tests/td02_triggers.rs` compares the live
  `sqlite_master` inventory against that fixture and fails on a missing,
  extra or drifted trigger. A future table rebuild that forgets a recreation
  is now a red gate.
- Negative probes: event log append-only (update and delete refused),
  template-version content immutability and no-delete, and the erasure payload
  rewrite refused **without** a committed `erasure_authorization`.
- The 0011 `event_no_update` widening is consistent with
  `devdocs/done/RFC-0002-Rust-Backend.md` ("the only permitted historical
  mutation is the specified erasure redaction plus its separate tombstone").
  `docs/OPERATIONS.md` does not spell out the SQL-level exception, so the
  checked-in inventory plus the negative probe now document it executable.

`cargo test --locked -p parley-service --test td02_triggers -- --nocapture`:
4 passed, "verified 38 final triggers".

## 3. God-file splits (pure moves)

| File | Before | After |
| --- | --- | --- |
| `src/resource_board.rs` | 2,926 | `resource_board/mod.rs` 540 + `card` 427, `column` 168, `http` 327, `placement` 500, `relation` 454, `support` 550 |
| `src/lib.rs` | 3,979 | `lib.rs` 1,562 + `http.rs` 1,046, `quarantine.rs` 193, `entity.rs` ~840, `graph.rs` ~194, `source.rs` ~175 |
| `src/commands/http.rs` | 3,489 | `commands/http/mod.rs` 1,379 + `pipeline.rs` 922, `destructive_pipeline.rs` 731, `approval_endpoints.rs` 394, `notifications.rs` 97 |

Details:

- `resource_board`: types/constants/helpers in `mod.rs`; the two `impl
  Workspace` blocks split across `card`, `placement`, `relation`, `column`,
  `support`; handlers in `http`. `routes()` remains the module facade.
  Cross-module private helpers became `pub(super)`; crate-root calls became
  `pub(crate)`. No `pub use` compatibility re-exports.
- `lib.rs`: crate root keeps mod declarations, DTOs, shared helpers, auth and
  `start_workspace`. Extracted quarantine, the owner HTTP handler set, and
  the `impl Workspace` groups for entity CRUD, graph and source/capture. The
  router calls `http::*`.
- `commands/http`: shared types and budget/denial helpers in `mod.rs`; the two
  pipelines, approval endpoints and notification stream in their own modules.
  `commands::approval` (the domain module) and the new `http::approval_endpoints`
  are distinct names; no alias was introduced.

## 4. Verification

| Gate | Result |
| --- | --- |
| `cargo fmt --all -- --check` / `cargo clippy --locked --workspace --all-targets -- -D warnings` | PASS |
| `cargo test --locked --workspace` | PASS on the rerun; one earlier run hit the pre-existing timing-sensitive `ar05_partial_batch_waits_releases_lane_and_resumes`, which passes in isolation and on the rerun (no agent-runtime code was touched) |
| Targeted suites during the moves | `td02_triggers` 4/4, `r01` 10/10, `r02` 3/3, `r03` 4/4, `r04` 2/2, `p04` 1/1, `s01` 7/7, `s02` 4/4, `s11` 9/9, `p01` 8/8, `u01` 14/14, `u05` 7/7, `u08` 14/14 |
| `python3 -B ci/migrations.py --through s11` / `--check-bands` | PASS |

## 5. Deferred

- **SQL extraction** (review §6): the 1,633 `sqlx::query*` call sites remain
  inline. No `include_str!`/`query!` conversion was attempted in this pass;
  doing it during a file move would double the review surface, as the review
  itself recommends. `cargo sqlx prepare --check` remains the schema gate.
- Fine-grained `lib.rs` extraction of auth/bootstrap and the remaining
  `AppState`/`start_workspace` block was not attempted: sibling modules depend
  on those crate-root items via `super::`, so that move needs its own pass.
