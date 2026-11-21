# S04 evaluation harness

The CLI unit test `s04_extractor_eval_is_machine_readable` reads the checked-in `extractor-v1` gold fixture and asserts a machine-readable JSON result with all four deterministic quote-validation cases passing. This initial evaluator validates deterministic quote handling; it does not invoke a provider.
