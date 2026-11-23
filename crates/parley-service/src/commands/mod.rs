//! UCR-001 command registry: erased dispatch, validation, output projection,
//! policy, ledger, and HTTP transport.

pub(crate) mod descriptor;
pub(crate) mod http;
pub(crate) mod ledger;
pub(crate) mod phase;
pub(crate) mod policy;

use std::collections::HashMap;

use descriptor::CommandDescriptor;

/// A retriever that refuses every external reference (FR-4: prohibit remote
/// reference resolution). Boot validation additionally rejects any schema
/// containing `$ref`, so retrieval can never be triggered.
struct NoRetriever;

impl jsonschema::Retrieve for NoRetriever {
    fn retrieve(
        &self,
        _uri: &jsonschema::Uri<&str>,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("remote reference resolution is disabled".into())
    }
}

fn validator_for(schema: &serde_json::Value) -> Result<jsonschema::Validator, String> {
    jsonschema::Validator::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_retriever(NoRetriever)
        .build(schema)
        .map_err(|error| format!("schema build failed: {error}"))
}

pub(crate) struct Registry {
    descriptors: Vec<CommandDescriptor>,
    by_id: HashMap<String, usize>,
    tier1_manifest: String,
}

impl Registry {
    pub fn build() -> Result<Self, String> {
        let mut descriptors = Vec::new();
        descriptors.extend(phase::descriptors().into_iter().cloned());
        descriptor::validate_registry(&descriptors)?;
        let mut by_id = HashMap::with_capacity(descriptors.len());
        for (index, item) in descriptors.iter().enumerate() {
            by_id.insert(item.id.to_string(), index);
        }
        let ordered = descriptor::tier1_order(&descriptors);
        let tier1_manifest = descriptor::manifest_hash(&ordered);
        let registry = Registry {
            descriptors,
            by_id,
            tier1_manifest,
        };
        registry.validate_samples()?;
        Ok(registry)
    }

    pub fn tier1_manifest(&self) -> &str {
        &self.tier1_manifest
    }

    pub fn contract_major(&self) -> i64 {
        1
    }

    pub fn command(&self, id: &str) -> Option<&CommandDescriptor> {
        self.by_id.get(id).map(|index| &self.descriptors[*index])
    }

    /// Owner-visible Tier 1 descriptors in manifest order.
    pub fn owner_tier1(&self) -> Vec<&CommandDescriptor> {
        descriptor::tier1_order(&self.descriptors)
            .into_iter()
            .filter(|descriptor| descriptor.is_owner_visible())
            .collect()
    }

    /// Sample-args agreement gate: every valid sample must both validate
    /// against the closed schema and deserialize through the typed serde
    /// struct; every invalid sample must fail schema validation.
    fn validate_samples(&self) -> Result<(), String> {
        let (valid, invalid) = phase::validation_samples();
        for sample in valid {
            let command = self
                .command(&sample.command)
                .ok_or_else(|| format!("sample command {} is not registered", sample.command))?;
            validator_for(command.args_schema)
                .map_err(|error| format!("{}: {error}", sample.command))?
                .validate(&sample.args)
                .map_err(|error| {
                    format!(
                        "valid sample args for {} failed schema validation: {}",
                        sample.command, error
                    )
                })?;
            let command_kind = phase::PhaseCommand::for_id(&sample.command).ok_or_else(|| {
                format!(
                    "sample command {} has no serde agreement check",
                    sample.command
                )
            })?;
            phase::parse_wire_args(command_kind, &sample.args)
                .map_err(|error| format!("{}: {error}", sample.command))?;
        }
        for sample in invalid {
            let command = self
                .command(&sample.command)
                .ok_or_else(|| format!("sample command {} is not registered", sample.command))?;
            let validator = validator_for(command.args_schema)
                .map_err(|error| format!("{}: {error}", sample.command))?;
            if validator.validate(&sample.args).is_ok() {
                return Err(format!(
                    "invalid sample args for {} unexpectedly passed schema validation",
                    sample.command
                ));
            }
        }
        Ok(())
    }
}

pub(crate) struct SampleArgs {
    pub command: String,
    pub args: serde_json::Value,
}
