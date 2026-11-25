s10 evidence is the named s10 integration cases in `crates/parley-service/tests/s10.rs`.

The queue fixtures are schema-pinned: `getQueue.valid.json` validates against
`QueueSnapshot` (including the required `total_actionable` and `total_blocked`
fields and the 200-item array caps), and `getQueue.invalid.json` validates
against the `CommandErrorEnvelope` error shape. Behavior is proven by the
executed `s10_queue_5000_parties_meets_render_budget` and
`s10_daily_queue_bounded_and_counts_at_scale` cases; the longer TD-03 record
lives in `devdocs/evidence/td-03.md`.
