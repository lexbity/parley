# Fuzz targets (RFC-0002/0003/0004)

This is the `cargo fuzz` crate for Parley. Targets run 60 s on PR and
30 min nightly with bounded allocation (RFC-0004 release evidence).

Implemented targets:

- `evidence_spans` (S02) — quote span boundaries and renormalization identity.
- `notes_spans` (S05) — toggle byte preservation outside the addressed marker.
- `template_slots` (S03) — typed slot parsing from the registered resolver set.

Mandated target that is not yet present, and why:

- `command_json` (S01) and `archive_manifest` (S11) fuzz request/archive
  decoding of the typed command entry point (`crates/parley-service/src/command.rs`)
  and the archive manifest module (`crates/parley-service/src/archive.rs`).
  Those modules do not exist yet (RFC-0004 QA P-2/P-3/part of P-16), so a
  fuzz target over them would be a stub. They are added when the module lands.

Run (nightly pinned by `fuzz/rust-toolchain.toml`):

    cd fuzz && cargo fuzz run evidence_spans -- -max_total_time=60
    cd fuzz && cargo fuzz run notes_spans -- -max_total_time=60
    cd fuzz && cargo fuzz run template_slots -- -max_total_time=60
