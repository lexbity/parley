S07 evidence is the named S07 integration cases in
`crates/parley-service/tests/s07.rs` and the browser journey in
`web/e2e/s07.spec.ts`.

The fixtures use synthetic opportunities, participants, released materials and
release grants. A valid send binds one primary and optional companions to
one-use releases with a response deadline; an invalid send pairs a companion
bound to a different recipient. Outcomes cover rung advancement, declines and
duplicate replay; resolution closes active timers; capacity provisioning
defines the ledger charged exactly once per send.
