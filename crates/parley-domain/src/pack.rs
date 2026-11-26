//! Pack documents: the owner-authored vocabulary, stages, ask ladder and
//! limits that configure one mode of outreach (job search, lead generation,
//! a later collab pack). Parsing and validation are pure so the queue sheet,
//! the gate and the settings surface share one definition of a valid pack.
//!
//! The document grammar is the flat YAML subset the owner edits in Settings:
//! top-level `key: value` scalars, two-space-indented maps (`vocabulary`,
//! `cadence`, `limits`) and two-space-indented `- { key: value, ... }` flow
//! lists (`stages`, `ask_ladder`). Unknown top-level fields are rejected, not
//! silently dropped: a typo such as `limit:` for `limits:` must fail loudly.

use serde_json::json;

/// The closed set of stage classes. `closed` is the only terminal class; every
/// other class plans future work.
pub const STAGE_CLASSES: [&str; 5] = ["identified", "prepared", "responded", "touched", "closed"];

/// One validation problem with its source line (1-based) and field name when
/// known. Validation collects every problem before failing.
#[derive(Debug, Clone, PartialEq)]
pub struct PackIssue {
    pub line: Option<usize>,
    pub field: Option<&'static str>,
    pub message: String,
}

impl PackIssue {
    fn new(line: Option<usize>, field: Option<&'static str>, message: String) -> Self {
        Self {
            line,
            field,
            message,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackStage {
    pub name: String,
    pub class: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackRung {
    pub rung: u32,
    pub weight: u32,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    pub id: String,
    pub name: String,
    pub version: String,
    pub party_noun: Option<String>,
    pub win_noun: Option<String>,
    pub stages: Vec<PackStage>,
    pub ask_ladder: Vec<PackRung>,
    pub max_touches: u32,
    pub daily_send_cap: u32,
}

impl Pack {
    /// `pack_stage` rows for this pack: (stage_id, label, class, kind,
    /// ordinal). Slugs are lowercase snake_case and unique within the pack;
    /// ordinals step by ten so owners can reason about ordering.
    pub fn stage_rows(&self) -> Vec<(String, String, String, &'static str, i64)> {
        let mut rows = Vec::with_capacity(self.stages.len());
        let mut used: Vec<String> = Vec::with_capacity(self.stages.len());
        for (index, stage) in self.stages.iter().enumerate() {
            let base = slug(&stage.name);
            let mut slug = base.clone();
            let mut suffix = 2;
            while used.contains(&slug) {
                slug = format!("{base}_{suffix}");
                suffix += 1;
            }
            used.push(slug.clone());
            let kind = if stage.class == "closed" {
                "terminal"
            } else {
                "planning"
            };
            rows.push((
                slug,
                stage.name.clone(),
                stage.class.clone(),
                kind,
                ((index + 1) * 10) as i64,
            ));
        }
        rows
    }

    /// The policy fragment stored under `$.pack_policy` in `owner_profile`.
    /// Derived from the document so activation can never disagree with the
    /// policy the queue and the gate read.
    pub fn policy_json(&self) -> serde_json::Value {
        // The fragment stored under `$.pack_policy` — no wrapper: the queue
        // reads `settings_json` at `$.pack_policy.limits.daily_send_cap`.
        json!({
            "limits": { "daily_send_cap": self.daily_send_cap },
            "cadence": { "max_touches": self.max_touches },
            "ask_ladder": {
                "max_rung": self.ask_ladder.iter().map(|rung| rung.rung).max().unwrap_or(0)
            },
        })
    }
}

fn slugify_base(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut previous_underscore = false;
    for character in name.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            previous_underscore = false;
        } else if !slug.is_empty() && !previous_underscore {
            slug.push('_');
            previous_underscore = true;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    slug
}

fn slug(name: &str) -> String {
    let base = slugify_base(name);
    if base.is_empty() {
        String::from("stage")
    } else {
        base
    }
}

/// CRLF to LF, trailing whitespace trimmed, exactly one trailing newline.
pub fn normalize_pack_document(document: &str) -> String {
    let unified = document.replace("\r\n", "\n");
    let trimmed = unified.trim_end();
    if trimmed.is_empty() {
        String::from("\n")
    } else {
        format!("{trimmed}\n")
    }
}

struct RawEntry {
    line: usize,
    key: String,
    value: String,
}

fn scalar(value: &str) -> &str {
    // Values end at an unquoted ` #` comment marker; quotes are not grammar.
    let hash = value.find(" #").unwrap_or(value.len());
    value[..hash].trim()
}

/// Parse and validate a pack document. Returns every problem found, so an
/// owner fixes one editor pass instead of one error per attempt.
pub fn parse_pack(document: &str) -> Result<Pack, Vec<PackIssue>> {
    let normalized = normalize_pack_document(document);
    let mut issues: Vec<PackIssue> = Vec::new();
    let mut scalars: Vec<RawEntry> = Vec::new();
    let mut maps: Vec<(String, RawEntry)> = Vec::new();
    let mut lists: Vec<(String, RawEntry)> = Vec::new();
    let mut current_map: Option<&'static str> = None;
    let mut current_list: Option<&'static str> = None;

    for (index, raw) in normalized.lines().enumerate() {
        let line = index + 1;
        let trimmed_start = raw.len() - raw.trim_start().len();
        let content = raw.trim();
        if content.is_empty() {
            continue;
        }
        if trimmed_start == 0 {
            let Some((key, value)) = content.split_once(':') else {
                issues.push(PackIssue::new(
                    Some(line),
                    None,
                    format!("Expected `field: value`, found `{content}`."),
                ));
                continue;
            };
            let key = key.trim();
            let known = [
                "id",
                "name",
                "version",
                "vocabulary",
                "stages",
                "ask_ladder",
                "cadence",
                "limits",
            ];
            if !known.contains(&key) {
                issues.push(PackIssue::new(
                    Some(line),
                    None,
                    format!("Unknown pack field `{key}`."),
                ));
            }
            match key {
                "vocabulary" | "cadence" | "limits" => {
                    current_map = Some(match key {
                        "vocabulary" => "vocabulary",
                        "cadence" => "cadence",
                        _ => "limits",
                    });
                    current_list = None;
                    if !scalar(value).is_empty() {
                        issues.push(PackIssue::new(
                            Some(line),
                            None,
                            format!("`{key}:` takes indented keys, not a value."),
                        ));
                    }
                }
                "stages" | "ask_ladder" => {
                    current_list = Some(if key == "stages" {
                        "stages"
                    } else {
                        "ask_ladder"
                    });
                    current_map = None;
                    if !scalar(value).is_empty() {
                        issues.push(PackIssue::new(
                            Some(line),
                            None,
                            format!("`{key}:` takes list entries, not a value."),
                        ));
                    }
                }
                other => {
                    current_map = None;
                    current_list = None;
                    scalars.push(RawEntry {
                        line,
                        key: other.to_owned(),
                        value: scalar(value).to_owned(),
                    });
                }
            }
            continue;
        }
        if trimmed_start == 2 && content.starts_with("- {") && content.ends_with('}') {
            let Some(section) = current_list else {
                issues.push(PackIssue::new(
                    Some(line),
                    None,
                    "List entry outside `stages:` or `ask_ladder:`.".to_owned(),
                ));
                continue;
            };
            lists.push((
                section.to_owned(),
                RawEntry {
                    line,
                    key: String::new(),
                    value: content[3..content.len() - 1].to_owned(),
                },
            ));
            continue;
        }
        if trimmed_start == 2 {
            let Some((key, value)) = content.split_once(':') else {
                issues.push(PackIssue::new(
                    Some(line),
                    None,
                    format!("Expected `key: value`, found `{content}`."),
                ));
                continue;
            };
            let Some(section) = current_map else {
                issues.push(PackIssue::new(
                    Some(line),
                    None,
                    "Indented key outside `vocabulary:`, `cadence:` or `limits:`.".to_owned(),
                ));
                continue;
            };
            maps.push((
                section.to_owned(),
                RawEntry {
                    line,
                    key: key.trim().to_owned(),
                    value: scalar(value).to_owned(),
                },
            ));
            continue;
        }
        issues.push(PackIssue::new(
            Some(line),
            None,
            "Indent must be exactly two spaces.".to_owned(),
        ));
    }

    let read = |key: &str| scalars.iter().find(|entry| entry.key == key);
    let mut pack = Pack {
        id: String::new(),
        name: String::new(),
        version: String::new(),
        party_noun: None,
        win_noun: None,
        stages: Vec::new(),
        ask_ladder: Vec::new(),
        max_touches: 0,
        daily_send_cap: 0,
    };

    for field in ["id", "name", "version"] {
        if read(field).is_none() {
            issues.push(PackIssue::new(
                None,
                Some(match field {
                    "id" => "id",
                    "name" => "name",
                    _ => "version",
                }),
                format!("Required pack field `{field}` is missing."),
            ));
        }
    }
    for field in ["stages", "ask_ladder", "cadence", "limits"] {
        let present = match field {
            "stages" | "ask_ladder" => lists.iter().any(|(section, _)| section == field),
            _ => maps.iter().any(|(section, _)| section == field),
        };
        if !present {
            issues.push(PackIssue::new(
                None,
                Some(match field {
                    "stages" => "stages",
                    "ask_ladder" => "ask_ladder",
                    "cadence" => "cadence",
                    _ => "limits",
                }),
                format!("Required pack field `{field}` is missing."),
            ));
        }
    }

    if let Some(entry) = read("id") {
        let valid = !entry.value.is_empty()
            && entry.value.len() <= 64
            && entry
                .value
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphanumeric())
            && entry
                .value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'));
        if valid {
            pack.id = entry.value.clone();
        } else {
            issues.push(PackIssue::new(Some(entry.line), Some("id"), "Pack id must be 1-64 characters of letters, digits, `_`, `.`, `:` or `-`, starting with a letter or digit.".to_owned()));
        }
    }
    if let Some(entry) = read("name") {
        if !entry.value.is_empty() && entry.value.chars().count() <= 80 {
            pack.name = entry.value.clone();
        } else {
            issues.push(PackIssue::new(
                Some(entry.line),
                Some("name"),
                "Pack name must be 1-80 characters.".to_owned(),
            ));
        }
    }
    if let Some(entry) = read("version") {
        let parts: Vec<&str> = entry.value.split('.').collect();
        let well_formed = parts.len() == 3
            && parts.iter().all(|part| {
                !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) && part.len() <= 6
            });
        if well_formed {
            pack.version = entry.value.clone();
        } else {
            issues.push(PackIssue::new(
                Some(entry.line),
                Some("version"),
                "Pack version must be three numeric parts like `1.0.0`.".to_owned(),
            ));
        }
    }
    for entry in maps
        .iter()
        .filter(|(section, _)| section == "vocabulary")
        .map(|(_, entry)| entry)
    {
        match entry.key.as_str() {
            "party_noun" => pack.party_noun = Some(entry.value.clone()),
            "win_noun" => pack.win_noun = Some(entry.value.clone()),
            other => issues.push(PackIssue::new(
                Some(entry.line),
                None,
                format!("Unknown vocabulary key `{other}`."),
            )),
        }
    }

    let stage_entries: Vec<&RawEntry> = lists
        .iter()
        .filter(|(section, _)| section == "stages")
        .map(|(_, entry)| entry)
        .collect();
    if stage_entries.is_empty() {
        issues.push(PackIssue::new(
            None,
            Some("stages"),
            "At least one stage is required.".to_owned(),
        ));
    }
    let mut seen_names: Vec<String> = Vec::new();
    for entry in stage_entries {
        let fields = match flow_fields(&entry.value) {
            Ok(fields) => fields,
            Err(message) => {
                issues.push(PackIssue::new(Some(entry.line), Some("stages"), message));
                continue;
            }
        };
        let name = fields.get("name").cloned().unwrap_or_default();
        let class = fields.get("class").cloned().unwrap_or_default();
        if name.is_empty() || name.chars().count() > 80 {
            issues.push(PackIssue::new(
                Some(entry.line),
                Some("stages"),
                "Stage name must be 1-80 characters.".to_owned(),
            ));
            continue;
        }
        if seen_names
            .iter()
            .any(|seen| seen.eq_ignore_ascii_case(&name))
        {
            issues.push(PackIssue::new(
                Some(entry.line),
                Some("stages"),
                format!("Duplicate stage name `{name}`."),
            ));
            continue;
        }
        if !STAGE_CLASSES.contains(&class.as_str()) {
            issues.push(PackIssue::new(
                Some(entry.line),
                Some("stages"),
                format!("Stage class must be one of {}.", STAGE_CLASSES.join(", ")),
            ));
            continue;
        }
        seen_names.push(name.clone());
        pack.stages.push(PackStage { name, class });
    }

    let ladder_entries: Vec<&RawEntry> = lists
        .iter()
        .filter(|(section, _)| section == "ask_ladder")
        .map(|(_, entry)| entry)
        .collect();
    if ladder_entries.is_empty() {
        issues.push(PackIssue::new(
            None,
            Some("ask_ladder"),
            "At least one ask rung is required.".to_owned(),
        ));
    }
    for (index, entry) in ladder_entries.iter().enumerate() {
        let fields = match flow_fields(&entry.value) {
            Ok(fields) => fields,
            Err(message) => {
                issues.push(PackIssue::new(
                    Some(entry.line),
                    Some("ask_ladder"),
                    message,
                ));
                continue;
            }
        };
        let rung = fields
            .get("rung")
            .and_then(|value| value.parse::<u32>().ok());
        let weight = fields
            .get("weight")
            .and_then(|value| value.parse::<u32>().ok());
        let label = fields.get("label").cloned().unwrap_or_default();
        if rung != Some((index + 1) as u32) {
            issues.push(PackIssue::new(
                Some(entry.line),
                Some("ask_ladder"),
                format!("Ask rung {} must be numbered {}.", index + 1, index + 1),
            ));
            continue;
        }
        if !weight.is_some_and(|weight| (1..=9).contains(&weight)) {
            issues.push(PackIssue::new(
                Some(entry.line),
                Some("ask_ladder"),
                "Ask weight must be 1-9.".to_owned(),
            ));
            continue;
        }
        if label.is_empty() || label.chars().count() > 80 {
            issues.push(PackIssue::new(
                Some(entry.line),
                Some("ask_ladder"),
                "Ask label must be 1-80 characters.".to_owned(),
            ));
            continue;
        }
        pack.ask_ladder.push(PackRung {
            rung: rung.unwrap(),
            weight: weight.unwrap(),
            label,
        });
    }

    if let Some(entry) = maps
        .iter()
        .rev()
        .find(|(section, entry)| section == "cadence" && entry.key == "max_touches")
        .map(|(_, entry)| entry)
    {
        match entry.value.parse::<u32>() {
            Ok(value) if (1..=50).contains(&value) => pack.max_touches = value,
            _ => issues.push(PackIssue::new(
                Some(entry.line),
                Some("cadence"),
                "Cadence max_touches must be 1-50.".into(),
            )),
        }
    } else {
        issues.push(PackIssue::new(
            None,
            Some("cadence"),
            "Cadence `max_touches` is missing.".to_owned(),
        ));
    }
    if let Some(entry) = maps
        .iter()
        .rev()
        .find(|(section, entry)| section == "limits" && entry.key == "daily_send_cap")
        .map(|(_, entry)| entry)
    {
        match entry.value.parse::<u32>() {
            Ok(value) if (1..=200).contains(&value) => pack.daily_send_cap = value,
            _ => issues.push(PackIssue::new(
                Some(entry.line),
                Some("limits"),
                "Limits daily_send_cap must be 1-200.".into(),
            )),
        }
    } else {
        issues.push(PackIssue::new(
            None,
            Some("limits"),
            "Limits `daily_send_cap` is missing.".to_owned(),
        ));
    }

    if issues.is_empty() {
        Ok(pack)
    } else {
        Err(issues)
    }
}

fn flow_fields(content: &str) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut fields = std::collections::BTreeMap::new();
    for part in content.split(',') {
        let Some((key, value)) = part.split_once(':') else {
            return Err(format!("Flow entry `{part}` is not `key: value`."));
        };
        fields.insert(key.trim().to_owned(), value.trim().to_owned());
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: &str = concat!(
        "id: job_search\n",
        "name: Job search\n",
        "version: 1.0.0\n",
        "vocabulary:\n",
        "  party_noun: hiring contact\n",
        "  win_noun: role\n",
        "stages:\n",
        "  - { name: Exploring, class: identified }\n",
        "  - { name: Preparing, class: prepared }\n",
        "  - { name: In conversation, class: responded }\n",
        "  - { name: Closed, class: closed }\n",
        "ask_ladder:\n",
        "  - { rung: 1, weight: 1, label: one-line question }\n",
        "  - { rung: 2, weight: 2, label: short review }\n",
        "cadence:\n",
        "  max_touches: 3\n",
        "limits:\n",
        "  daily_send_cap: 12\n",
    );

    #[test]
    fn parses_the_reference_document() {
        let pack = parse_pack(REFERENCE).expect("reference document must parse");
        assert_eq!(pack.id, "job_search");
        assert_eq!(pack.stages.len(), 4);
        assert_eq!(
            pack.stage_rows()[3],
            (
                "closed".into(),
                "Closed".into(),
                "closed".into(),
                "terminal",
                40
            )
        );
        assert_eq!(pack.ask_ladder.len(), 2);
        assert_eq!(pack.max_touches, 3);
        assert_eq!(pack.daily_send_cap, 12);
        assert_eq!(pack.policy_json()["limits"]["daily_send_cap"], 12);
        assert_eq!(pack.policy_json()["ask_ladder"]["max_rung"], 2);
    }

    #[test]
    fn collects_every_problem_and_reports_lines() {
        let broken = "id: job_search\nname: Job search\nversion: one\nstages:\nask_ladder:\nlimits:\n  daily_send_cap: 9999\n";
        let issues = parse_pack(broken).expect_err("must fail");
        let fields: Vec<Option<&str>> = issues.iter().map(|issue| issue.field).collect();
        assert!(fields.contains(&Some("version")));
        assert!(fields.contains(&Some("ask_ladder")));
        assert!(fields.contains(&Some("cadence")));
        assert!(fields.contains(&Some("limits")));
        assert!(issues.iter().any(|issue| issue.line == Some(3)));
    }

    #[test]
    fn rejects_unknown_top_level_fields_and_sluggifies_stage_names() {
        let document = REFERENCE.replace("cadence:", "cadence2:\n  max_touches: 3\ncadence:");
        let issues = parse_pack(&document).expect_err("unknown field must fail");
        assert!(
            issues
                .iter()
                .any(|issue| issue.message.contains("Unknown pack field"))
        );
        let pack = parse_pack(REFERENCE).expect("reference parses");
        let rows = pack.stage_rows();
        assert_eq!(rows[2].0, "in_conversation");
    }

    #[test]
    fn normalizes_line_endings_and_trailing_whitespace() {
        assert_eq!(
            normalize_pack_document("id: x\r\nversion: 1.0.0\r\n\r\n"),
            "id: x\nversion: 1.0.0\n"
        );
    }
}
