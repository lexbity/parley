//! UCR-001 command registry: erased dispatch, validation, output projection,
//! policy, ledger, and HTTP transport.

pub(crate) mod descriptor;
pub(crate) mod http;
pub(crate) mod jobs;
pub(crate) mod ledger;
pub(crate) mod opportunity;
pub(crate) mod party;
pub(crate) mod phase;
pub(crate) mod policy;
pub(crate) mod read;
pub(crate) mod resource;
pub(crate) mod task;

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

pub(crate) fn validator_for(schema: &serde_json::Value) -> Result<jsonschema::Validator, String> {
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
        descriptors.extend(opportunity::descriptors().into_iter().cloned());
        descriptors.extend(read::descriptors().into_iter().cloned());
        descriptors.extend(party::descriptors().into_iter().cloned());
        descriptors.extend(task::descriptors().into_iter().cloned());
        descriptors.extend(jobs::descriptors().into_iter().cloned());
        descriptors.extend(resource::descriptors().into_iter().cloned());
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

    /// Every registered command ID, for validating mapped tool names.
    pub fn descriptor_ids(&self) -> Vec<&'static str> {
        self.descriptors
            .iter()
            .map(|descriptor| descriptor.id)
            .collect()
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

    /// Agent Tier 1 descriptors visible under a run's immutable permission
    /// subset. Discovery and invocation use the same predicate (FR-3/FR-6).
    pub fn agent_tier1(&self, permissions: &[String]) -> Vec<&CommandDescriptor> {
        let mut items: Vec<&CommandDescriptor> = descriptor::tier1_order(&self.descriptors)
            .into_iter()
            .filter(|descriptor| {
                descriptor.surfaces.contains(&policy::Surface::Agent)
                    && descriptor.agent_tier == Some(1)
                    && permissions
                        .iter()
                        .any(|value| value == descriptor.requires_permission.as_str())
            })
            .collect();
        items.truncate(http::QUERY_MAX_COMMANDS);
        items
    }

    /// Agent search uses the same permission filter as discovery and never
    /// exposes schemas or private record data (FR-6).
    pub fn agent_search(
        &self,
        query: &str,
        limit: usize,
        permissions: &[String],
    ) -> Vec<serde_json::Value> {
        let needle = query.trim().to_ascii_lowercase();
        let mut hits: Vec<&CommandDescriptor> = self
            .descriptors
            .iter()
            .filter(|descriptor| {
                descriptor.surfaces.contains(&policy::Surface::Agent)
                    && permissions
                        .iter()
                        .any(|value| value == descriptor.requires_permission.as_str())
            })
            .collect();
        hits.sort_by(|a, b| {
            fn rank<'a>(descriptor: &'a CommandDescriptor, needle: &str) -> (u8, &'a str) {
                if needle.is_empty() {
                    return (5, descriptor.id);
                }
                if descriptor.id == needle {
                    return (0, descriptor.id);
                }
                if descriptor.id.starts_with(needle) {
                    return (1, descriptor.id);
                }
                let title = descriptor.title.to_ascii_lowercase();
                if title.split_whitespace().any(|token| token == needle) {
                    return (2, descriptor.id);
                }
                if title.starts_with(needle) {
                    return (3, descriptor.id);
                }
                let tokens = needle
                    .split_whitespace()
                    .filter(|token| {
                        descriptor.id.contains(token)
                            || title.contains(token)
                            || descriptor.summary.to_ascii_lowercase().contains(token)
                            || descriptor.keywords.iter().any(|key| key.contains(token))
                    })
                    .count();
                (4 + (10 - tokens.min(10)) as u8, descriptor.id)
            }
            rank(a, &needle).cmp(&rank(b, &needle))
        });
        hits.truncate(limit.min(http::SEARCH_MAX_RESULTS));
        hits.into_iter()
            .map(|descriptor| {
                serde_json::json!({
                    "id": descriptor.id.replace('.', "__"),
                    "version": descriptor.version,
                    "title": descriptor.title,
                    "summary": descriptor.summary,
                    "effect": descriptor.effect,
                })
            })
            .collect()
    }

    /// A descriptor is concealed from an agent unless its permission is in
    /// the grant. The same check authorizes invocation (FR-3).
    pub fn agent_visible(&self, id: &str, permissions: &[String]) -> Option<&CommandDescriptor> {
        let descriptor = self.command(id)?;
        if !descriptor.surfaces.contains(&policy::Surface::Agent) {
            return None;
        }
        if !permissions
            .iter()
            .any(|value| value == descriptor.requires_permission.as_str())
        {
            return None;
        }
        Some(descriptor)
    }

    /// Sample-args agreement gate: every valid sample must both validate
    /// against the closed schema and deserialize through the typed serde
    /// struct; every invalid sample must fail schema validation.
    fn validate_samples(&self) -> Result<(), String> {
        let (phase_valid, phase_invalid) = phase::validation_samples();
        let (opportunity_valid, opportunity_invalid) = opportunity::validation_samples();
        let (read_valid, read_invalid) = read::validation_samples();
        let (party_valid, party_invalid) = party::validation_samples();
        let (task_valid, task_invalid) = task::validation_samples();
        let (jobs_valid, jobs_invalid) = jobs::validation_samples();
        let (resource_valid, resource_invalid) = resource::validation_samples();
        let valid = [
            phase_valid,
            opportunity_valid,
            read_valid,
            party_valid,
            task_valid,
            jobs_valid,
            resource_valid,
        ]
        .concat();
        let invalid = [
            phase_invalid,
            opportunity_invalid,
            read_invalid,
            party_invalid,
            task_invalid,
            jobs_invalid,
            resource_invalid,
        ]
        .concat();
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
            if let Some(command_kind) = phase::PhaseCommand::for_id(&sample.command) {
                phase::parse_wire_args(command_kind, &sample.args)
                    .map_err(|error| format!("{}: {error}", sample.command))?;
            } else if sample.command == opportunity::CREATE_ID {
                opportunity::parse_wire_args(&sample.args)
                    .map_err(|error| format!("{}: {error}", sample.command))?;
            } else if let Some(command_kind) = read::ReadCommand::for_id(&sample.command) {
                read::parse_wire_args(command_kind, &sample.args)
                    .map_err(|error| format!("{}: {error}", sample.command))?;
            } else if let Some(command_kind) = party::PartyCommand::for_id(&sample.command) {
                party::parse_wire_args(command_kind, &sample.args)
                    .map_err(|error| format!("{}: {error}", sample.command))?;
            } else if sample.command == task::TASK_DONE_ID {
                task::parse_wire_args(&sample.args)
                    .map_err(|error| format!("{}: {error}", sample.command))?;
            } else if sample.command == jobs::BRIEF_START_ID {
                jobs::parse_wire_args(&sample.args)
                    .map_err(|error| format!("{}: {error}", sample.command))?;
            } else if let Some(command_kind) = resource::ResourceCommand::for_id(&sample.command) {
                resource::parse_wire_args(command_kind, &sample.args)
                    .map_err(|error| format!("{}: {error}", sample.command))?;
            } else {
                return Err(format!(
                    "sample command {} has no serde agreement check",
                    sample.command
                ));
            }
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

/// Positive audit allowlist for one registered command. A registration that
/// forgets to declare its fields records no args instead of leaking arbitrary
/// request content into the ledger (UCR-001 §5).
pub(crate) fn audit_args(
    command_id: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let (allow, sensitive): (&[&str], &[&str]) =
        if phase::PhaseCommand::for_id(command_id).is_some() {
            (phase::args_allowlist(), phase::args_sensitive())
        } else if command_id == opportunity::CREATE_ID {
            (opportunity::args_allowlist(), opportunity::args_sensitive())
        } else if read::ReadCommand::for_id(command_id).is_some() {
            (read::args_allowlist(), read::args_sensitive())
        } else if party::PartyCommand::for_id(command_id).is_some() {
            (party::args_allowlist(), party::args_sensitive())
        } else if command_id == task::TASK_DONE_ID {
            (task::args_allowlist(), task::args_sensitive())
        } else if command_id == jobs::BRIEF_START_ID {
            (jobs::args_allowlist(), jobs::args_sensitive())
        } else if resource::ResourceCommand::for_id(command_id).is_some() {
            (resource::args_allowlist(), resource::args_sensitive())
        } else {
            (&[], &[])
        };
    ledger::redact_args(args, allow, sensitive)
}

#[derive(Clone)]
pub(crate) struct SampleArgs {
    pub command: String,
    pub args: serde_json::Value,
}

/// The serialized outcome of a registered write operation plus whether a real
/// effect (a domain event or fact mutation) occurred. A true no-op returns
/// the current permitted state and consumes zero executed-write allowance
/// (UCR-001 FR-2).
pub(crate) struct RunOutcome {
    pub result: serde_json::Value,
    pub effected: bool,
}

/// The resolved actor behind an invocation. Owner authority is unrestricted;
/// an agent actor carries only the immutable permission/target subset of its
/// run. Instances are built by the owner auth boundary or by the private
/// principal constructor in `agent_runtime::grants`, never from tool input.
#[derive(Clone, Debug)]
pub(crate) struct Actor {
    pub kind: ActorKind,
    pub pseudonym: String,
    pub epoch: String,
    pub run_id: Option<String>,
    pub lease_token: Option<String>,
    pub permissions: Vec<String>,
    pub targets: Vec<ActorTarget>,
    /// Immutable run grant flag. Only the private principal constructor may
    /// set this from a durable run row; tool input can never assert it.
    pub allow_created_opportunities: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActorKind {
    Owner,
    Agent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActorTarget {
    pub kind: String,
    pub id: String,
}

impl Actor {
    pub(crate) fn owner(pseudonym: String) -> Self {
        Self {
            kind: ActorKind::Owner,
            pseudonym,
            epoch: String::new(),
            run_id: None,
            lease_token: None,
            permissions: Vec::new(),
            targets: Vec::new(),
            allow_created_opportunities: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn agent(
        pseudonym: String,
        epoch: String,
        run_id: String,
        lease_token: String,
        permissions: Vec<String>,
        targets: Vec<ActorTarget>,
        allow_created_opportunities: bool,
    ) -> Self {
        Self {
            kind: ActorKind::Agent,
            pseudonym,
            epoch,
            run_id: Some(run_id),
            lease_token: Some(lease_token),
            permissions,
            targets,
            allow_created_opportunities,
        }
    }

    pub(crate) fn actor_kind(&self) -> &'static str {
        match self.kind {
            ActorKind::Owner => "owner",
            ActorKind::Agent => "agent",
        }
    }

    /// The target field named by a command's args, if any. Selectors and
    /// relationships inside args never widen scope.
    pub(crate) fn target_field<'a>(
        &self,
        args: &'a serde_json::Value,
    ) -> Option<(&'static str, &'a str)> {
        for field in [
            "party_id",
            "primary_party_id",
            "opportunity_id",
            "resource_id",
            "material_id",
        ] {
            if let Some(value) = args.get(field).and_then(serde_json::Value::as_str) {
                return Some((field, value));
            }
        }
        None
    }

    pub(crate) fn party_ids(&self) -> Vec<String> {
        self.targets
            .iter()
            .filter(|target| target.kind == "party")
            .map(|target| target.id.clone())
            .collect()
    }
}
