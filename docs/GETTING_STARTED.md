# Getting started

**Audience:** Contributors setting up this repository for the first time.
**Purpose:** Install the pinned tools, launch the local app, and verify the setup.
**Prerequisites:** Read the [glossary](README.md#glossary) and the root [README](../README.md).

## Toolchain versions

Versions are pinned.
Do not substitute newer versions without updating the pinned files.

| Tool | Version | Source of truth |
|---|---|---|
| Rust and Cargo | 1.98.1 | `rust-toolchain.toml` |
| Node | 26.12.0 | `.nvmrc`, `package.json` |
| pnpm | 12.21.0 | `package.json` |
| Python | 3.14.10 | `ci/toolchain-versions.yaml` |
| SQLx CLI | 0.9.1 | `ci/toolchain-versions.yaml` |
| Playwright | 1.65.1, Chromium build 1255 | `web/package.json`, `ci/toolchain-versions.yaml` |
| `pi` agent | 1.6.0 | `agent/package.json` |

Check the active toolchain after installation:

```sh
scripts/check-toolchain
```

## Install

Run the commands below from the repository root.

### Rust

Install the toolchain with `rustup`.
`rust-toolchain.toml` selects the channel and components.

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component rustfmt,clippy,llvm-tools-preview
```

### Node and pnpm

Install Node 26.12.0 with your version manager.
With `nvm`, run:

```sh
nvm install 26.12.0
nvm use
```

Install the pinned pnpm version:

```sh
npm install --global pnpm@12.21.0
```

### Python and SQLx CLI

Python runs the verifiers in `ci/`.
Install version 3.14.10 with your version manager.
Use your version manager's standard install command.

```sh
python3 -m pip install -r ci/requirements.txt
```

SQLx CLI must match the SQLx library version.
The Rust CI lane installs it with `cargo binstall`:

```sh
cargo binstall -y sqlx-cli@0.9.1
```

### Project dependencies

Install both workspaces with frozen locks:

```sh
pnpm install --frozen-lockfile
cargo fetch --locked
```

## Environment variables

Set these before launch if the defaults do not fit.

| Name | Required | Default | Description |
|---|---|---|---|
| `PARLEY_WORKSPACE` | No | Unset | Path to the Workspace file. Overrides the defaults below. |
| `XDG_DATA_HOME` | No | Unset | Base directory for the default Workspace path. |
| `HOME` | No on Linux | Unset | Fallback base when `XDG_DATA_HOME` is unset. |

Without an override, the launcher uses
`$XDG_DATA_HOME/parley/workspace.sqlite`, or
`$HOME/.local/share/parley/workspace.sqlite`.
The `--workspace PATH` flag takes precedence over all three variables.
If no base directory can be found, the launcher asks for `PARLEY_WORKSPACE`.

Never commit a Workspace file.
`*.sqlite`, `*.sqlite-wal`, and `*.sqlite-shm` are gitignored.

## First launch

Build and start the launcher in headless mode:

```sh
cargo build --locked --release --bin parley
./target/release/parley --workspace /tmp/parley-demo --headless
```

Headless mode prints a one-time local bootstrap URL.
Open that URL in a browser to reach the app.
The URL secret is single-use and expires.

Omit `--headless` to let the launcher open the browser for you:

```sh
./target/release/parley --workspace /tmp/parley-demo
```

A second launch against the same Workspace reuses the running service.
It opens a fresh bootstrap URL through the local control socket.
Do not delete a live Workspace lock.

Check the binary version:

```sh
./target/release/parley --version
```

The app starts without a model installed.
Manual entry and review remain available.
Agent-backed features need a local provider.

## Frontend-only setup

You can work on the client without Rust, SQLite, or a model.

Start Vite against the real service on loopback:

```sh
pnpm --dir web run dev
```

Start Vite with the development mock:

```sh
pnpm --dir web run dev:mock
```

The mock is a separate development entry point.
Production never selects it through a URL or fallback.

## Verification

Run these checks before opening a pull request:

```sh
pnpm --dir web run typecheck
pnpm --dir web run lint
pnpm --dir web run test
cargo test --locked --workspace
scripts/check-slice s01
```

For a full per-slice proof, run:

```sh
scripts/prove s01
```

Replace `s01` with the Slice you are changing.
Test layers, coverage gates, and failure examples live in [TESTING](TESTING.md).

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `cannot determine the owner data directory; set PARLEY_WORKSPACE` | No `XDG_DATA_HOME` or `HOME` | Set `PARLEY_WORKSPACE` to a writable path. |
| `workspace is active but its control channel is unavailable` | Another launcher owns the lock, but its socket is missing | Stop stale processes and relaunch. Never delete a live lock. |
| Toolchain manifest drift reported | Active versions do not match the pinned files | Run `scripts/check-toolchain`, then align the installed versions. |
| Migration check fails | SQLx CLI does not match the library | Install `sqlx-cli@0.9.1`, then rerun the migration verifier. |
| Browser tests report zero executed tests | The requested Slice has no spec yet | Check `web/e2e/` and [TESTING](TESTING.md). Missing tests must not pass vacuously. |
| Agent features unavailable | No local model provider configured | Use manual paths. |

## Next steps

- [ARCHITECTURE](ARCHITECTURE.md) — understand the boundaries before changing code.
- [API](API.md) — learn the request contract the client uses.
- [CONTRIBUTING](CONTRIBUTING.md) — read the pull request checklist.
- [OPERATIONS](OPERATIONS.md) — back up, restore, or erase a Workspace.
