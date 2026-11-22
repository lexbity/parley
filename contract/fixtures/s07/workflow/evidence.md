S07 evidence is the named S07 integration cases in
`crates/parley-service/tests/s07.rs` and the browser journey in
`web/e2e/s07.spec.ts`.

The fixtures use synthetic opportunities, participants, released materials and
release grants. A valid send binds one primary and optional companions to
one-use releases with a response deadline; an invalid send pairs a companion
bound to a different recipient. Outcomes cover rung advancement, declines and
duplicate replay; resolution closes active timers; capacity provisioning
defines the ledger charged exactly once per send. Thread listing exposes the
per-touch send history with outcome records so a reloaded client can attach a
response to a historical send. Do-not-contact clearance is reason-bearing:
missing or short reasons are rejected, cleared policy is recorded as an event,
and a fresh version becomes releasable again after clearance.
