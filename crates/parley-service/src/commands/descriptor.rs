//! Command descriptors, boot validation and manifest hashing (UCR-001
//! FR-4, §4.1, §4.2).
//!
//! Descriptors are closed, self-contained records: IDs follow the §4.2
//! shape, args/result schemas are closed Draft 2020-12 JSON schemas with no
//! `$ref`, and context expressions are parsed to a bounded AST at boot.
//! The manifest is the ordered JCS of descriptors (Tier 1 ordered by the
//! §4.2 context rule: conditioned entries first, ID tie-break) hashed with
//! SHA-256. Zero descriptors are published until boot validation passes.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::policy::{Effect, Permission, Surface};

pub(crate) const MAX_DESCRIPTOR_ID_BYTES: usize = 48;
pub(crate) const MAX_TITLE_SCALARS: usize = 60;
pub(crate) const MAX_SUMMARY_SCALARS: usize = 140;
pub(crate) const MAX_DESCRIPTION_BYTES: usize = 1024;
pub(crate) const MAX_PALETTE_CONTEXT_FIELDS: usize = 16;
pub(crate) const MANIFEST_MAX_COMMANDS: usize = 256;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PaletteSpec {
    /// Flat context keys read from the palette context (closed allowlist).
    pub context_fields: &'static [&'static str],
    /// Ordered prompts in declaration order.
    pub prompts: &'static [PromptSpec],
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PromptSpec {
    pub field: &'static str,
    #[serde(rename = "picker")]
    pub picker: &'static str,
    pub title: &'static str,
}

pub(crate) const PICKER_ALLOWLIST: &[&str] = &[
    "text",
    "party",
    "planning_stage",
    "opportunity_task",
    "resource_subtype",
];

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CommandDescriptor {
    pub id: &'static str,
    pub version: u32,
    pub title: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
    pub keywords: &'static [&'static str],
    pub effect: Effect,
    pub requires_permission: Permission,
    pub surfaces: &'static [Surface],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_tier: Option<u8>,
    pub args_schema: &'static serde_json::Value,
    pub result_schema: &'static serde_json::Value,
    pub supports_dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_schema: Option<&'static serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<&'static str>,
    pub palette: PaletteSpec,
}

impl CommandDescriptor {
    pub fn is_owner_visible(&self) -> bool {
        self.surfaces.contains(&Surface::Palette)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Expr {
    /// A context key, resolved against the request context.
    Ident(String),
    /// A quoted literal, always evaluates to its string value.
    Lit(String),
    Eq(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExprError {
    UnexpectedEnd,
    UnexpectedToken(Token),
    TooDeep,
}

/// Parse a bounded context expression.
pub(crate) fn parse_expr(source: &str) -> Result<Expr, ExprError> {
    let mut parser = Parser {
        tokens: tokenize(source),
        pos: 0,
        depth: 0,
    };
    let expr = parser.parse_or()?;
    if parser.pos != parser.tokens.len() {
        return Err(ExprError::UnexpectedToken(
            parser.tokens[parser.pos].clone(),
        ));
    }
    Ok(expr)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    Ident(String),
    String(String),
    Eq,
    Ne,
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn tokenize(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            ' ' | '\t' | '\n' => i += 1,
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '!' if chars.get(i + 1) == Some(&'=') => {
                tokens.push(Token::Ne);
                i += 2;
            }
            '!' => {
                tokens.push(Token::Not);
                i += 1;
            }
            '&' if chars.get(i + 1) == Some(&'&') => {
                tokens.push(Token::And);
                i += 2;
            }
            '|' if chars.get(i + 1) == Some(&'|') => {
                tokens.push(Token::Or);
                i += 2;
            }
            '=' if chars.get(i + 1) == Some(&'=') => {
                tokens.push(Token::Eq);
                i += 2;
            }
            '"' => {
                let mut value = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    value.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
                tokens.push(Token::String(value));
            }
            c if c.is_ascii_alphanumeric() || c == '_' || c == '.' => {
                let mut ident = String::new();
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                {
                    ident.push(chars[i]);
                    i += 1;
                }
                tokens.push(Token::Ident(ident));
            }
            _ => {
                tokens.push(Token::String(ch.to_string()));
                i += 1;
            }
        }
    }
    tokens
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn parse_or(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_and()?;
        while self.peek_is(&Token::Or) {
            self.pos += 1;
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_unary()?;
        while self.peek_is(&Token::And) {
            self.pos += 1;
            let right = self.parse_unary()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ExprError> {
        if self.peek_is(&Token::Not) {
            self.pos += 1;
            let inner = self.parse_unary()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        self.depth += 1;
        if self.depth > 8 {
            return Err(ExprError::TooDeep);
        }
        let result = if self.peek_is(&Token::LParen) {
            self.pos += 1;
            let inner = self.parse_or()?;
            if !self.peek_is(&Token::RParen) {
                return Err(ExprError::UnexpectedEnd);
            }
            self.pos += 1;
            inner
        } else {
            let left = match self.next() {
                Some(Token::Ident(ident)) => Expr::Ident(ident),
                Some(Token::String(value)) => Expr::Lit(value),
                _ => return Err(ExprError::UnexpectedEnd),
            };
            if self.peek_is(&Token::Eq) || self.peek_is(&Token::Ne) {
                let op = self.next().unwrap();
                let right = match self.next() {
                    Some(Token::Ident(ident)) => Expr::Ident(ident),
                    Some(Token::String(value)) => Expr::Lit(value),
                    _ => return Err(ExprError::UnexpectedEnd),
                };
                if op == Token::Eq {
                    Expr::Eq(Box::new(left), Box::new(right))
                } else {
                    Expr::Not(Box::new(Expr::Eq(Box::new(left), Box::new(right))))
                }
            } else {
                left
            }
        };
        self.depth -= 1;
        Ok(result)
    }

    fn peek_is(&self, token: &Token) -> bool {
        self.tokens.get(self.pos) == Some(token)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }
}

/// Evaluate a parsed expression against a bounded context map. Missing
/// boolean values are false; other missing values compare as null.
pub(crate) fn eval_expr(expr: &Expr, context: &BTreeMap<String, serde_json::Value>) -> bool {
    match expr {
        Expr::Ident(path) => match context.get(path) {
            Some(serde_json::Value::Bool(value)) => *value,
            _ => false,
        },
        Expr::Lit(_) => false,
        Expr::Eq(left, right) => {
            let l = value_of(left, context);
            let r = value_of(right, context);
            l == r
        }
        Expr::Not(inner) => !eval_expr(inner, context),
        Expr::And(left, right) => eval_expr(left, context) && eval_expr(right, context),
        Expr::Or(left, right) => eval_expr(left, context) || eval_expr(right, context),
    }
}

fn value_of(expr: &Expr, context: &BTreeMap<String, serde_json::Value>) -> serde_json::Value {
    match expr {
        Expr::Ident(path) => context
            .get(path)
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        Expr::Lit(value) => serde_json::Value::String(value.clone()),
        _ => serde_json::Value::Null,
    }
}

/// Validate one owned descriptor identity; returns its mapped tool name.
/// Shared by boot registration and the catalogue verification probe.
pub(crate) fn validate_id(id: &str) -> Result<String, String> {
    let segments: Vec<&str> = id.split('.').collect();
    if segments.len() < 2
        || segments.iter().any(|segment| segment.is_empty())
        || segments.iter().any(|segment| segment.contains("__"))
    {
        return Err(format!(
            "descriptor id {id:?} must use at least two lowercase dot-separated snake-case segments with no double underscore"
        ));
    }
    if !id.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
    }) || id.len() > MAX_DESCRIPTOR_ID_BYTES
    {
        return Err(format!(
            "descriptor id {id:?} must be lowercase ASCII snake-case, at most 48 bytes"
        ));
    }
    if id.starts_with("ui.") || id.starts_with("commands.") {
        return Err(format!(
            "descriptor id {id:?} reserves the ui./commands. prefix"
        ));
    }
    let mapped_name = id.replace('.', "__");
    if mapped_name.len() > 64 {
        return Err(format!(
            "descriptor id {id:?} exceeds 64 bytes after dot-to-double-underscore replacement"
        ));
    }
    Ok(mapped_name)
}

/// Validate the owned shape of one descriptor without publishing it. Boot
/// runs the same checks over the static registrations; this entry point lets
/// the release catalogue gate prove that a deliberately invalid registration
/// is rejected (FR-4).
pub(crate) fn validate_owned_descriptor(
    id: &str,
    args_schema: &serde_json::Value,
    result_schema: &serde_json::Value,
    when: Option<&str>,
) -> Result<(), String> {
    validate_id(id)?;
    validate_schema(id, args_schema)?;
    validate_schema(id, result_schema)?;
    if let Some(when) = when {
        parse_expr(when).map_err(|error| {
            format!("descriptor {id:?} has a malformed context expression: {error:?}")
        })?;
    }
    Ok(())
}

/// Boot validation for a descriptor set. Returns actions error on the first
/// invalid registration (FR-4).
pub(crate) fn validate_registry(descriptors: &[CommandDescriptor]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    let mut mapped = BTreeSet::new();
    for descriptor in descriptors {
        let id = descriptor.id;
        let mapped_name = validate_id(id)?;
        if !seen.insert(id) {
            return Err(format!("duplicate descriptor id {id:?}"));
        }
        if !mapped.insert(mapped_name.clone()) {
            return Err(format!(
                "mapped-name collision for {id:?} (-> {mapped_name})"
            ));
        }
        if !super::policy::PERMISSION_ALLOWLIST.contains(&descriptor.requires_permission.as_str()) {
            return Err(format!(
                "descriptor {id:?} declares an undeclared permission {}",
                descriptor.requires_permission.as_str()
            ));
        }
        if descriptor.title.chars().count() > MAX_TITLE_SCALARS {
            return Err(format!("descriptor {id:?} title exceeds 60 scalar values"));
        }
        if descriptor.summary.chars().count() > MAX_SUMMARY_SCALARS {
            return Err(format!(
                "descriptor {id:?} summary exceeds 140 scalar values"
            ));
        }
        if descriptor.description.len() > MAX_DESCRIPTION_BYTES {
            return Err(format!(
                "descriptor {id:?} description exceeds 1024 UTF-8 bytes"
            ));
        }
        if descriptor.surfaces.is_empty() {
            return Err(format!("descriptor {id:?} declares no surfaces"));
        }
        for surface in descriptor.surfaces {
            let name = match surface {
                Surface::Palette => "palette",
                Surface::Agent => "agent",
            };
            if !super::policy::SURFACE_ALLOWLIST.contains(&name) {
                return Err(format!("descriptor {id:?} declares unknown surface {name}"));
            }
        }
        if let Some(tier) = descriptor.agent_tier {
            if !(1..=2).contains(&tier) {
                return Err(format!("descriptor {id:?} has invalid agent tier {tier}"));
            }
            if !descriptor.surfaces.contains(&Surface::Agent) {
                return Err(format!(
                    "descriptor {id:?} declares an agent tier without the agent surface"
                ));
            }
        }
        validate_schema(id, descriptor.args_schema)?;
        validate_schema(id, descriptor.result_schema)?;
        if descriptor.supports_dry_run {
            // A dry-run command must expose a generated preview schema and its
            // result must be an object (FR-4). Preview results are validated
            // against the preview schema before any dry-run receipt is stored.
            let preview = descriptor.preview_schema.ok_or_else(|| {
                format!("descriptor {id:?} supports dry run without a preview schema")
            })?;
            validate_schema(id, preview)?;
            if descriptor
                .result_schema
                .get("type")
                .and_then(|v| v.as_str())
                != Some("object")
            {
                return Err(format!(
                    "descriptor {id:?} supports dry run without an object result schema"
                ));
            }
        } else if descriptor.preview_schema.is_some() {
            return Err(format!(
                "descriptor {id:?} declares a preview schema without supporting dry run"
            ));
        }
        if let Some(when) = descriptor.when {
            parse_expr(when).map_err(|error| {
                format!("descriptor {id:?} has a malformed context expression: {error:?}")
            })?;
        }
        if descriptor.palette.context_fields.len() > MAX_PALETTE_CONTEXT_FIELDS {
            return Err(format!(
                "descriptor {id:?} declares too many palette context fields"
            ));
        }
        for prompt in descriptor.palette.prompts {
            if !PICKER_ALLOWLIST.contains(&prompt.picker) {
                return Err(format!(
                    "descriptor {id:?} prompt field {} uses unallowlisted picker {}",
                    prompt.field, prompt.picker
                ));
            }
            let properties = descriptor
                .args_schema
                .get("properties")
                .and_then(|v| v.as_object());
            match properties.and_then(|map| map.get(prompt.field)) {
                Some(_) => {}
                None => {
                    return Err(format!(
                        "descriptor {id:?} prompt field {} is not declared in args_schema",
                        prompt.field
                    ));
                }
            }
        }
        for field in descriptor.palette.context_fields {
            if !field
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_')
            {
                return Err(format!(
                    "descriptor {id:?} has a malformed context field name {field:?}"
                ));
            }
        }
    }
    Ok(())
}

fn validate_schema(id: &str, schema: &serde_json::Value) -> Result<(), String> {
    if !schema.is_object() {
        return Err(format!("descriptor {id:?} schema must be an object"));
    }
    if schema.get("$ref").is_some() || schema.get("$schema").is_some() {
        return Err(format!(
            "descriptor {id:?} schema must be closed (no $ref/$schema)"
        ));
    }
    if schema.get("type").and_then(|v| v.as_str()).is_none() {
        return Err(format!("descriptor {id:?} schema must declare a type"));
    }
    if schema.get("additionalProperties").and_then(|v| v.as_bool()) != Some(false) {
        return Err(format!(
            "descriptor {id:?} schema must set additionalProperties: false"
        ));
    }
    // Reject unsigned scalar overflow and unrepresentable bounds.
    let check_bound = |name: &str| -> Result<(), String> {
        if let Some(bound) = schema.get(name) {
            if let Some(number) = bound.as_i64() {
                if number > 9_007_199_254_740_991 {
                    return Err(format!(
                        "descriptor {id:?} schema {name} exceeds the safe-integer range"
                    ));
                }
            } else if bound.as_f64().is_none() {
                return Err(format!(
                    "descriptor {id:?} schema {name} must be an integer"
                ));
            }
        }
        Ok(())
    };
    check_bound("minimum")?;
    check_bound("maximum")?;
    if let Some(properties) = schema.get("properties") {
        if !properties.is_object() {
            return Err(format!(
                "descriptor {id:?} schema properties must be an object"
            ));
        }
        for (name, property) in properties.as_object().unwrap() {
            if !property.is_object() {
                return Err(format!(
                    "descriptor {id:?} schema property {name} must be an object"
                ));
            }
            let type_ok = match property.get("type") {
                Some(serde_json::Value::String(kind)) => !kind.is_empty(),
                Some(serde_json::Value::Array(kinds)) => {
                    !kinds.is_empty() && kinds.iter().all(|kind| kind.is_string())
                }
                _ => false,
            };
            if !type_ok {
                return Err(format!(
                    "descriptor {id:?} schema property {name} must declare a type"
                ));
            }
        }
    }
    Ok(())
}

/// Tier 1 manifest order: context-conditioned entries first, then ID.
pub(crate) fn tier1_order(descriptors: &[CommandDescriptor]) -> Vec<&CommandDescriptor> {
    let mut ordered: Vec<&CommandDescriptor> = descriptors.iter().collect();
    ordered.sort_by(|a, b| {
        let a_conditioned = a.when.is_some();
        let b_conditioned = b.when.is_some();
        b_conditioned
            .cmp(&a_conditioned)
            .then_with(|| a.id.cmp(b.id))
    });
    ordered
}

/// Deterministic manifest hash over ordered descriptors (JCS, including
/// versions).
pub(crate) fn manifest_hash(ordered: &[&CommandDescriptor]) -> String {
    let mut canonical = String::new();
    for descriptor in ordered {
        let value = serde_json::to_value(descriptor).unwrap_or_default();
        if let Ok(part) = parley_domain::jcs::canonicalize_value(&value) {
            canonical.push_str(&part);
        }
        canonical.push('\n');
    }
    crate::digest(canonical.as_bytes())
}
