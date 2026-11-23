# Operations and maintenance

**Audience:** Contributors changing persistence, recovery, or local diagnostics.
**Purpose:** Document maintenance of one local Workspace after launch.
**Prerequisites:** Read the [glossary](README.md#glossary) and [ARCHITECTURE](ARCHITECTURE.md#key-invariants).

## Scope

Parley has no hosted environment, on-call rotation, or remote infrastructure.
This doc covers local maintenance of one Workspace.
Hosted monitoring, alerting, and scaling are out of scope.
Code-level limits live in [ARCHITECTURE](ARCHITECTURE.md#scaling-notes).

Maintenance actions are explicit Owner commands.
Destructive actions use a preview followed by a separate commit.

## Backup

Backups use a SQLite online snapshot.
The service pins referenced blobs until the manifest is durable.
A successful backup is published under a generation directory.
The last verified generation remains available.

| Endpoint | Purpose |
|---|---|
| `GET /api/v1/backups` | List backup generations |
| `POST /api/v1/backups` | Create a backup |
| `GET /api/v1/maintenance/operations/{id}` | Poll a maintenance job |

The specified policy is nightly backups with 14 retained generations.
See [API](API.md#party-import-and-maintenance) for request contract details.

## Restore

Restore prepares a sibling generation, verifies it, then publishes a selector under an exclusive lock.
A restore mints a new epoch and revokes existing sessions and cursors.
Clients must rediscover the session after a restore.

1. `POST /api/v1/restore/preview` with a chosen snapshot.
2. Review the reported rows, bytes, format versions, and snapshot date.
3. `POST /api/v1/restore/commit` with the fresh preview ID and explicit confirmation.
4. Poll `GET /api/v1/maintenance/operations/{id}` until the operation ends.
5. Reopen the app and complete the bootstrap flow again.

A damaged backup never replaces a verified one.

## Export and import

An export is an Owner administrative action.
It is distinct from a Release to a recipient.
Import and restore previews are short-lived and single-use.

| Endpoint | Purpose |
|---|---|
| `POST /api/v1/exports` | Create an export |
| `POST /api/v1/imports/preview` | Build an import plan |
| `POST /api/v1/imports/commit` | Commit an import plan |
| `POST /api/v1/party-imports` | Stage a CSV Party import |
| `POST /api/v1/party-imports/{id}/preview` | Preview the CSV import |
| `POST /api/v1/party-imports/{id}/commit` | Commit the CSV import |

An import rejects absolute paths, traversal components, symlinks, and duplicate normalized filenames.
The default archive budget is 16 GiB expanded, 100,000 entries, and 64 MiB per attachment.
These are specified budgets, not measured behavior.

## Erasure

Erasure removes a Party's data from the live Workspace and derived content.
It previews impact before it commits.
It invalidates affected approvals.

1. `POST /api/v1/parties/{id}/erase/preview`.
2. Review the affected rows, shared captures, and history.
3. `POST /api/v1/parties/{id}/erase` with explicit confirmation.
4. Rebuild the clean generation and remove old working generations.

Erasure cannot recall bytes already issued through a Release.
Backups and Owner-managed copies require separate disposal.
The service reports copies that remain outside its control.

## Migrations

Migrations are ordered, checksummed SQL files in `migrations/`.
The current sequence runs from `0001_workspace.sql` to `0019_queue_sheet.sql`.
The schema version is separate from the wire version.

- Take a backup before migrating a populated Workspace.
- Migrate on a working copy, then verify counts, foreign keys, and hashes.
- Keep the previous generation recoverable.
- Forward migration only; rollback uses a verified prior generation.
- A failed migration leaves the prior Workspace recoverable.

Check migrations from the repository root:

```sh
python3 ci/migrations.py --through s11
```

Replace `s11` with the Slice under verification.

## Disk hygiene

Development and testing must not grow disk usage without bound.
Test Workspaces and scratch databases live in OS temporary directories.
Browser fixtures dispose of their server and Workspace after each test.
`ci/disk.py --check` runs in both CI lanes and in `scripts/check-slice`.

```sh
python3 ci/disk.py --report
./scripts/prune --dry-run
./scripts/prune
./scripts/prune --playwright-cache --registry
```

`scripts/prune` clears cargo build output, test artifacts, stale `/tmp/parley-*` directories, and unreferenced pnpm packages.
The two opt-in flags also remove machine-global caches.
Other projects on the machine may need a browser reinstall after `--playwright-cache`.

Database artifacts must not appear in the working tree outside `contract/fixtures`.
`*.sqlite`, `*.sqlite-wal`, `*.sqlite-shm`, and `*.db` are gitignored.

## Logs and diagnostics

Logs are content-free.
They record wall time, a build or epoch pseudonym, a correlation ID, the operation, elapsed milliseconds, the result class, and queue wait.
They do not record bodies, prompts, URL fragments, tokens, private filenames, or raw SQL bind parameters.
Rotation is set at 5 MiB with three files.
## Common incidents

| Symptom | Likely cause | Action |
|---|---|---|
| `workspace is active but its control channel is unavailable` | Another launcher owns the lock; its socket is missing | Stop stale processes and relaunch. Never delete a live lock. |
| Migration verifier fails | SQLx CLI does not match the library | Install `sqlx-cli@0.9.1` and rerun the verifier. |
| Cursor rejected as invalid or gapped | Filters changed or retained events were pruned | Reset the client query and refetch snapshots. |
| Release capability expired | The one-use grant was spent or timed out | Request a new grant with a new operation key. |
| Release disabled after a correction | A dependency revision or Gate result changed | Create a new Material version and review it. |
| Deadline backlog reported as recovering | Due threads are being reconciled | Wait for reconciliation to finish. Do not mark expired threads actionable. |
| Attachment unavailable | Missing or corrupt blob disables release | Inspect blob metadata and restore from a verified backup. |
| Agent features unavailable | No local model provider | Use manual paths. |

## Related docs

- [GETTING_STARTED](GETTING_STARTED.md#troubleshooting) — setup failures.
- [API](API.md#party-import-and-maintenance) — maintenance routes.
- [ARCHITECTURE](ARCHITECTURE.md#adr-5--immutable-bytes-and-retained-history) — why history is retained.
