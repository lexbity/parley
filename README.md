# Parley

**Audience:** New contributors and evaluators who have never seen this codebase.
**Purpose:** Explain what Parley does and how to reach a running local app.
**Prerequisites:** None for reading. Setup steps link to [GETTING_STARTED](docs/GETTING_STARTED.md).

[![Rust CI](https://img.shields.io/badge/Rust_CI-pending-lightgrey)](.github/workflows/rust.yml)
[![Web CI](https://img.shields.io/badge/Web_CI-pending-lightgrey)](.github/workflows/web-mock.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

## Contents

- [What Parley is](#what-parley-is)
- [Features](#features)
- [Prerequisites](#prerequisites)
- [Quick start](#quick-start)
- [Usage examples](#usage-examples)
- [Documentation](#documentation)
- [Screenshots](#screenshots)
- [License](#license)

## What Parley is

Parley helps one person decide whom they can help, prepare an honest offer, and remember what happened.
The Owner is the single human user.
The Owner starts from either end.
They record a Resource (something they can offer) or capture a Party (a person or organization they may engage).
An Opportunity joins those records.
An Opportunity is the Owner's pursuit of a possible exchange.
Parley prepares materials locally.
The Owner copies approved text or downloads an approved file, then delivers it outside the application.
Parley never sends anything.

## Features

- Start from either end.
  The Owner records a Resource first or captures a Party first.
- Keep facts traceable.
  A Claim is a factual statement with provenance, so each draft can show where a fact came from.
- Decide releases with code.
  The Gate is deterministic code that checks a Material before any bytes are issued.
- Work from one local Workspace.
  A Workspace is the Owner's SQLite database, blob store, and control directory.
  All application data stays there.
- Review Captures before they become facts.
  A Capture is raw, untrusted pasted text.
  The Owner accepts, corrects, or dismisses each proposal.
- Track work across five tabs.
  Resources, Parties, Queue, Output, and Settings address the same records.
- Keep output history immutable.
  A Material is a working output bound to one Opportunity and one recipient, held in numbered versions.
- Remember every touch.
  A Thread is the conversation for one Opportunity and recipient pair, with a reply deadline.
- Recover locally.
  Backup, restore, export, import, and erasure operate on the Workspace.

## Prerequisites

| Requirement | Version | Source of truth |
|---|---|---|
| Rust | 1.98.1 | `rust-toolchain.toml` |
| Node | 26.12.0 | `.nvmrc` |
| pnpm | 12.21.0 | `package.json` |
| Python | 3.14.10 | `ci/toolchain-versions.yaml` |
| SQLx CLI | 0.9.1 | `ci/toolchain-versions.yaml` |

Python and SQLx CLI are needed for CI verifiers and migration checks.
Frontend-only work needs Node and pnpm.

## Quick start

1. Install the pinned toolchains in [GETTING_STARTED](docs/GETTING_STARTED.md#toolchain-versions).
2. Install frontend dependencies: `pnpm install --frozen-lockfile`.
3. Start the development UI: `pnpm --dir web run dev`.
4. Run the mock browser tests: `pnpm --dir web run e2e:mock`.
5. To run the full local app, build the launcher and start it headless:
   `cargo build --locked --release --bin parley`
   `./target/release/parley --workspace /tmp/parley-demo --headless`

The headless launch prints a one-time local bootstrap URL.
Open that URL to reach the app.
See [GETTING_STARTED](docs/GETTING_STARTED.md#first-launch) for the full sequence.

## Usage examples

Start the full app with a chosen Workspace path:

```sh
cargo build --locked --release --bin parley
./target/release/parley --workspace /tmp/parley-demo --headless
```

Start the frontend against the development mock, with no Rust service:

```sh
pnpm install --frozen-lockfile
pnpm --dir web run dev:mock
```

Verify one capability slice end to end:

```sh
scripts/check-slice s01
scripts/prove s01
```

Run the offline Extractor evaluation:

```sh
cargo run --locked -p parley-cli -- eval --role extractor
```

## Documentation

Start at the [documentation index](docs/README.md).
It owns the glossary and the topic map.

| Doc | What it covers |
|---|---|
| [ARCHITECTURE](docs/ARCHITECTURE.md) | Components, data model, trust boundaries, decisions |
| [GETTING_STARTED](docs/GETTING_STARTED.md) | Install, configure, launch, verify, troubleshoot |
| [API](docs/API.md) | Local HTTP contract, auth, endpoints, errors |
| [CONTRIBUTING](docs/CONTRIBUTING.md) | Branching, commits, PRs, CI gates |
| [TESTING](docs/TESTING.md) | Test layers and commands |
| [OPERATIONS](docs/OPERATIONS.md) | Backup, restore, erasure, migrations, disk hygiene |
| [CHANGELOG](CHANGELOG.md) | Version history |
| [CODE_OF_CONDUCT](CODE_OF_CONDUCT.md) | Community expectations |

## Screenshots

No screenshots are included in this doc set.

## License

Apache-2.0. See [LICENSE](LICENSE).
