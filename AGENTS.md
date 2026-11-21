no minimum viable product
no shims
no aliases
no compat
no stubs

./devdocs stays in .gitignore

## Agent ownership

- **Backend agent on `backend`** owns Rust crates, migrations and provider tests. Run the active Rust slice test while iterating.
- **Frontend agent on `frontend`** owns the Vue client, HTTP mock behavior and frontend tests. Use Vitest watch and the mock browser project while iterating.
- **Integration agent on `merge`** coordinates OpenAPI, fixtures, generated API types and the shared test harness. Integrate both branches and run `scripts/check-slice` and `scripts/prove` before qualifying a slice.

Agree on the active slice's contract and fixture cases before parallel implementation. Route shared-file changes to their owner. Check the current branch before editing and coordinate checkout changes with any agent using that working directory.
