S06 evidence is the named S06 integration cases in
`crates/parley-service/tests/s06.rs` and the browser journey in
`web/e2e/s06.spec.ts`.

The fixtures use synthetic uploads, recipients and release grants. A valid
review binds share permission; a material only binds a reviewed, shareable
upload; a download serves the exact approved bytes exactly once and refuses a
stale or consumed grant.
