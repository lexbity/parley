# Local HTTP API

**Audience:** Frontend and service contributors changing the request or response contract.
**Purpose:** Document the loopback API that the Vue client and development mock share.
**Prerequisites:** Read the [glossary](README.md#glossary) and [ARCHITECTURE](ARCHITECTURE.md#system-diagram).

## Base and conventions

The service binds to IPv4 loopback on an OS-allocated port.
The UI and API share one origin.
The base path is `/api/v1`.

- JSON is UTF-8.
- Timestamps are RFC 3339 in UTC.
- IDs are opaque strings.
- Byte offsets are zero-based and measured on the named representation.
- Hashes are lowercase hexadecimal SHA-256.
- Binary routes return bytes, MIME type, filename, and length.

The launcher prints the local address in the bootstrap URL.

## Authentication

There are no accounts.
The launcher prints a one-time bootstrap URL containing a 60-second secret.
The client strips the fragment before making requests.

1. `POST /session/bootstrap` with `{"secret":"<bootstrap-secret>"}`.
2. The response sets a `parley_session` cookie and returns `{"ok":true}`.
3. `GET /session` returns the `workspace_id`, `epoch`, wire versions, capabilities, `mutation_token`, and `cursor`.
4. Every private request sends the cookie, the `X-Parley-Token` header, and a matching `Host` and `Origin`.

The cookie is HttpOnly, SameSite=Strict, scoped to `/api/v1`, and valid for eight hours.
Bootstrap and session responses set `Cache-Control: no-store`.
Mutation token handling for replay is specified in RFC-0004 IQ2.

### Required headers

| Header | Used for | Applies to |
|---|---|---|
| `Host` | Must equal the service authority | All requests |
| `Origin` | Must match the local origin when present | All requests |
| `Cookie: parley_session=…` | Session identity | All private requests |
| `X-Parley-Token` | Per-session mutation token | Private reads and writes |
| `If-Match` | Strong ETag for the named write scope | Updates and actions |
| `Idempotency-Key` | Client-generated operation key | Mutations |
| `X-Parley-Epoch` | Workspace epoch for invalidation | Responses from the service |

A missing or weak `If-Match` is rejected where the route requires one.
Replaying a key with a different payload or target conflicts.

## Endpoint catalogue

Paths are relative to `/api/v1`.
Braces mark path parameters.

### Launch and session

| Method | Path | Purpose |
|---|---|---|
| GET | `/health` | Readiness and version |
| POST | `/session/bootstrap` | Redeem the one-time secret |
| GET | `/session` | Workspace identity, token, capabilities, cursor |

### Entities, graph, and evidence

| Method | Path | Purpose |
|---|---|---|
| GET, POST | `/entities` | List or create an Entity |
| GET, PATCH | `/entities/{id}` | Read or update an Entity |
| GET, PUT | `/entities/{id}/notes` | Read or replace canonical Markdown notes |
| GET | `/entities/{id}/dossier` | Party dossier |
| GET | `/entities/{id}/usage` | Where an Entity is used |
| POST | `/entities/{id}/retire` | Retire an Entity from ordinary use |
| POST | `/entities/{id}/clear-dnc` | Clear party do-not-contact with a reason |
| GET | `/graph` | Neighborhood snapshot |
| PUT | `/graph/layout` | Persist view coordinates |
| POST | `/graph/related` | Create a related Entity and Edge atomically |
| POST | `/edges` | Create an Edge |
| PATCH, DELETE | `/edges/{id}` | Correct or remove an Edge |
| POST | `/claims/{id}/revisions` | Add a Claim revision |
| POST | `/claims/{id}/hook` | Accept a hook on a Claim |
| GET | `/events` | Poll committed events |

### Captures and proposals

| Method | Path | Purpose |
|---|---|---|
| GET, POST | `/captures` | List or save Captures |
| GET | `/captures/{occurrence_id}/proposals` | Read staged proposals |
| POST | `/proposals/{batch_id}/resolve` | Apply an explicit decision list |
| GET | `/directory` | Filterable Party directory |

### Opportunities, board, and queue

| Method | Path | Purpose |
|---|---|---|
| GET, POST | `/opportunities` | List or create an Opportunity |
| GET | `/opportunities/{id}` | Read an Opportunity |
| POST | `/opportunities/{id}/participants` | Add a participant |
| PUT | `/opportunities/{id}/tags` | Replace tags |
| POST, DELETE | `/opportunities/{id}/sources/{source_id}` | Link or unlink a source |
| GET, POST | `/opportunities/{id}/interests` | Interests for both sides |
| GET, POST | `/opportunities/{id}/exchange` | Exchange versions |
| POST | `/opportunities/{id}/exchange/{recipient_id}/attest` | Attest fairness |
| POST, GET | `/opportunities/{id}/matches` | Ranked matches or no-match |
| GET, PATCH | `/opportunities/{id}/notes` | Read or update notes |
| POST | `/opportunities/{id}/notes/tasks/toggle` | Toggle one task by byte span |
| PATCH | `/opportunities/{id}/phase` | Set the planning phase |
| POST | `/opportunities/{id}/resolution` | Record a resolution |
| GET | `/board` | Board with ordered lists |
| POST | `/board/lists` | Create a list |
| POST | `/board/lists/reorder` | Reorder lists |
| PATCH | `/board/lists/{list_id}` | Rename a list |
| POST | `/board/lists/{list_id}/archive` | Archive a list |
| POST | `/opportunities/{id}/placement` | Move a card |
| GET | `/queue` | Ranked and filtered work |
| GET | `/insights` | Insight report |

### Resources and resource board

| Method | Path | Purpose |
|---|---|---|
| POST | `/resources/{id}/ready` | Mark a Resource ready |
| GET, POST | `/voice-samples` | Voice samples |
| POST | `/voice-samples/{id}/retire` | Retire a voice sample |
| POST | `/identity-tags` | Save an identity tag |
| GET | `/resource-board` | Resource board snapshot |
| GET | `/resource-cards/{id}` | Read a resource card |
| GET | `/resource-cards/{id}/deliveries` | Card deliveries |
| POST | `/resource-cards/{id}/retire` | Retire a card |
| POST | `/resource-card-commits` | Commit a card |
| POST | `/resource-board/moves` | Move a card |

### Materials, templates, and release

| Method | Path | Purpose |
|---|---|---|
| POST, GET | `/materials` | Create or list Materials |
| GET | `/materials/{id}` | Read a Material |
| POST | `/materials/{id}/versions` | Add an immutable version |
| POST | `/materials/{id}/gate` | Evaluate the Gate |
| POST | `/materials/{id}/lint` | Run the linter |
| POST | `/materials/{id}/approve` | Approve a version |
| POST | `/materials/{id}/release` | Issue a Release |
| GET | `/materials/{id}/download` | Retrieve issued bytes |
| POST | `/releases/{id}/ack` | Acknowledge clipboard result |
| GET | `/outputs` | Output registry |
| POST, GET | `/templates` | Template families |
| POST | `/templates/{family_id}/versions` | Add a template version |
| POST | `/templates/{id}/approve` | Approve a template |
| POST | `/templates/{id}/activate` | Activate a template |
| POST | `/templates/build` | Build a template candidate |
| GET, POST | `/template-families` | Template lineage |

### Threads, sends, and time

| Method | Path | Purpose |
|---|---|---|
| GET | `/threads` | List Threads |
| POST | `/threads/{id}/send` | Record Mark sent |
| POST | `/threads/{id}/reopen` | Reopen a Thread |
| GET | `/sends` | List Sends |
| POST | `/sends/{id}/outcome` | Record an outcome |
| POST | `/sends/{id}/outcome/correction` | Correct an outcome |
| GET, POST | `/capacity` | Capacity ledger |

### Files

| Method | Path | Purpose |
|---|---|---|
| POST | `/uploads` | Stream an attachment upload |
| POST | `/uploads/{id}/review` | Review an opaque upload |
| POST | `/entities/{id}/attachments` | Attach a file |
| GET | `/entities/{id}/attachments` | List attachments |
| DELETE | `/entities/{id}/attachments/{reference}` | Remove an attachment |
| GET | `/entities/{id}/attachments/{reference}/content` | Read attachment bytes |

### Agent and coaching

| Method | Path | Purpose |
|---|---|---|
| GET, PUT | `/agent/provider` | Local provider configuration |
| POST | `/agent/provider/health` | Probe the provider |
| GET, POST | `/agent/chats` | Chats |
| GET | `/agent/chats/{id}` | Read a chat |
| POST | `/agent/chats/{id}/turns` | Add a turn |
| POST | `/agent/chats/{id}/questions` | Act on a question |
| POST | `/agent/candidates/{id}/confirm` | Confirm a candidate |
| POST | `/agent/candidates/{id}/dismiss` | Dismiss a candidate |
| GET, PUT | `/agent/prompts/{role}` | Role prompt version |
| GET, PUT | `/agent/question-bank` | Interviewer question bank |
| GET, POST | `/jobs/{id}` | Read or cancel a job |
| GET, POST | `/segments` | Segments |
| POST | `/segments/{id}/parties` | Update segment membership |
| GET, POST | `/briefs/{scope_kind}/{scope_id}/items` | Brief items |
| POST | `/brief-items/{id}/decision` | Decide on a brief item |
| GET | `/fluency` | Glossary fluency states |
| PUT | `/fluency/{term}` | Update fluency |
| GET | `/coach/questions` | Coach questions |
| POST | `/coach/questions/{id}/answer` | Answer a question |
| POST | `/coach/questions/{id}/dismiss` | Dismiss a question |
| GET, POST | `/coach/weekly` | Weekly report |
| GET | `/embedding-model` | Active embedding model |
| PUT | `/embedding-model` | Set the embedding model |
| GET, POST | `/matching/calibrations` | Match thresholds |

### Party import and maintenance

| Method | Path | Purpose |
|---|---|---|
| POST | `/party-imports` | Stage a CSV import |
| POST | `/party-imports/{id}/preview` | Preview an import |
| POST | `/party-imports/{id}/commit` | Commit an import |
| GET, POST | `/backups` | List or create a backup |
| POST | `/exports` | Create an export |
| POST | `/imports/preview` | Preview an import plan |
| POST | `/imports/commit` | Commit an import plan |
| POST | `/restore/preview` | Preview a restore |
| POST | `/restore/commit` | Commit a restore |
| POST | `/parties/{id}/erase/preview` | Preview erasure |
| POST | `/parties/{id}/erase` | Commit erasure |
| GET | `/maintenance/operations/{id}` | Poll a maintenance job |
| GET, PUT | `/settings/maintenance` | Maintenance settings |

Operational detail for maintenance routes lives in [OPERATIONS](OPERATIONS.md).

## Response shape

A single-record read returns `{data, cursor}`.
A collection returns `{items, next_cursor, observed_cursor}`.
A successful mutation returns the committed aggregate or a `{data, event_cursor}` envelope.
An asynchronous command returns a job ID and status URL with status `202`.

The `cursor` is the last committed event sequence visible to the read, as a decimal string.
`observed_cursor` reports the fence for the collection read.

## Pagination, filter, and sort

Collection reads default to 50 items and cap at 200.
`next_cursor` is opaque and bound to filters and sort.
An expired or changed cursor is rejected; requery from the start.

- Filters combine with AND.
- Repeated values within one filter combine with OR.
- Sorts use a deterministic ID tiebreaker.
- Totals are computed after filters and before pagination.

`GET /events` takes an `after` cursor and returns ordered retained changes.
An event gap invalidates the client's replay position.

## Errors

Every error has a stable code, a human-readable message, and field details where relevant.
Error bodies never carry private content.

```json
{
  "error": {
    "code": "REVISION_CONFLICT",
    "message": "The resource changed while this edit was open.",
    "details": [{"path": "/revision", "expected_rev": 3, "current_rev": 4}],
    "retryable": false
  }
}
```

| Status | Meaning |
|---|---|
| 200 | Read or update succeeded |
| 201 | Resource created |
| 202 | Asynchronous work accepted |
| 204 | Empty success |
| 400 | Malformed input |
| 401 | Invalid or expired session |
| 403 | Token, origin, or permission failure |
| 404 | Unknown resource |
| 409 | Domain or key conflict |
| 410 | Expired capability, page cursor, or erased record |
| 412 | Stale version |
| 413 | Size limit exceeded |
| 422 | Schema, policy, or Gate failure |
| 428 | Missing precondition |
| 503 | Temporarily unavailable |

Observed codes include `BOOTSTRAP_INVALID` and `SESSION_EXPIRED`.

There is no public rate limit on the loopback API.
Request-size and queue limits are listed in [ARCHITECTURE](ARCHITECTURE.md#scaling-notes).

## Versioning

`GET /session` reports `wire_major` and `wire_minor`.
The URL carries the major version.
Additive response fields are allowed within a major version.
An incompatible command meaning requires a new major version.
The database schema version is separate from the wire version.

## Client examples

Health check with a discovered local address:

```sh
curl -sS "http://<host>:<port>/api/v1/health"
```

Bootstrap a session:

```sh
curl -sS -X POST "http://<host>:<port>/api/v1/session/bootstrap" \
  -H 'Content-Type: application/json' \
  -d '{"secret":"<bootstrap-secret>"}' \
  -c /tmp/parley-cookies.txt
```

Read the session:

```sh
curl -sS "http://<host>:<port>/api/v1/session" \
  -b /tmp/parley-cookies.txt \
  -H "X-Parley-Token: <mutation-token>"
```

Create an Entity:

```sh
curl -sS -X POST "http://<host>:<port>/api/v1/entities" \
  -b /tmp/parley-cookies.txt \
  -H "X-Parley-Token: <mutation-token>" \
  -H "Idempotency-Key: <uuid-v7>" \
  -H 'Content-Type: application/json' \
  -d '<typed-create-body>'
```

Replace `<typed-create-body>` with the create payload for the Entity kind.

The shipped client wraps these steps:

```ts
import { ParleyClient } from './web/src/api/client';

const client = new ParleyClient();
await client.connect();
const opportunities = await client.listOpportunities();
```

## Related docs

- [GETTING_STARTED](GETTING_STARTED.md#first-launch) — obtain the bootstrap URL.
- [ARCHITECTURE](ARCHITECTURE.md#adr-3--loopback-http-with-bootstrap-session) — why the API is local.
- [TESTING](TESTING.md) — contract fixtures and parity checks.
