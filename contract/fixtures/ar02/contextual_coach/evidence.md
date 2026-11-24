# AR02 contextual Coach fixtures

The packaged-service tests use a current owner session and a disposable SQLite workspace.

| Case | Selector | Expected boundary |
| --- | --- | --- |
| positive party scope | Current party ID and revision | Read the scoped question projection and bind dispositions to its revision and semantic key |
| stale party | Existing party with a prior card revision | Reject with `STALE_INPUT` before creating a job or receipt |
| foreign opportunity recipient | Non-member recipient or absent opportunity | Hide the selector as `TARGET_UNAVAILABLE` |
| stale disposition | Current question ID with wrong question revision | Reject with `QUESTION_STALE` and preserve the open question |
| owner disposition | Current question ID, key and revision | Commit one disposition and advance its revision once; same operation key replays it |

The mock browser project models explicit scoped start, scope filters and question dispositions. The Rust browser project uses the production service and verifies the unavailable-model path without treating the mock as worker evidence.
