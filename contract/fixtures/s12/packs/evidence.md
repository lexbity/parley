# s12 · pack settings evidence

- Operation: getPackSettings / validatePack / activatePack (`contract/fixtures/s12/packs/`).
- Rust proof: `s12_pack_settings_validate_activate_and_remap` (tests/s12_queue_sheet.rs) exercises
  the default GET, a refused stale activation (412 SETTINGS_STALE), an unvalidated-pack refusal
  (422 PACK_NOT_VALIDATED via server-side re-validation), and a successful activation with an
  explicit stage mapping that remaps the opportunity's stage and derives `$.pack_policy`.
- Mock parity: web/e2e/mock-api.ts implements the same three routes (pre-existing).
- 2026-11-25: routes were consumed by the UI and the mock but absent from the service and the
  contract (architecture-review finding G14, TD-001 rev 1.1); this slice closes the gap.
