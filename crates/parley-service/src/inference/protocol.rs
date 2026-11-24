use std::{collections::BTreeSet, fmt, sync::LazyLock};

use serde::{
    Deserialize, Serialize,
    de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _};

pub const SCHEMA: &[u8] = include_bytes!("../../../../contract/agent/runtime-v1.schema.json");
pub const MAX_FRAME: usize = 512 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

static VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    let schema = serde_json::from_slice(SCHEMA).expect("runtime frame schema JSON");
    jsonschema::validator_for(&schema).expect("runtime frame schema")
});

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub revision: u32,
    pub provider_version: String,
    pub model_tag: String,
    pub model_digest: String,
    pub tokenizer_digest: String,
    pub context_tokens: u32,
    pub template_tokens: u32,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Coach,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Execute {
    Probe {
        profile: Profile,
        deadline_ms: u32,
    },
    StructuredRole {
        profile: Profile,
        deadline_ms: u32,
        role: Role,
        prompt_version: String,
        schema_digest: String,
        system: String,
        input: Value,
    },
}

impl Execute {
    pub fn profile(&self) -> &Profile {
        match self {
            Self::Probe { profile, .. } | Self::StructuredRole { profile, .. } => profile,
        }
    }

    pub fn deadline_ms(&self) -> u32 {
        match self {
            Self::Probe { deadline_ms, .. } | Self::StructuredRole { deadline_ms, .. } => {
                *deadline_ms
            }
        }
    }

    pub fn matches(&self, completion: &Completion) -> bool {
        matches!(
            (self, completion),
            (Self::Probe { .. }, Completion::Probe { .. })
                | (
                    Self::StructuredRole { .. },
                    Completion::StructuredRole { .. }
                )
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedModel {
    pub provider_version: String,
    pub model_tag: String,
    pub model_digest: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Completion {
    Probe {
        observed: ObservedModel,
        usage: Usage,
    },
    StructuredRole {
        observed: ObservedModel,
        usage: Usage,
        candidate: Value,
    },
}

impl Completion {
    pub fn observation(&self) -> (&ObservedModel, &Usage) {
        match self {
            Self::Probe { observed, usage }
            | Self::StructuredRole {
                observed, usage, ..
            } => (observed, usage),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Failure {
    ModelUnavailable,
    ProfileChanged,
    ContextBudget,
    WorkerProtocol,
    ResultInvalid,
    ProviderFailed,
    Cancelled,
    IsolationUnavailable,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Namespaces {
    pub user: String,
    pub net: String,
    pub pid: String,
    pub ipc: String,
    pub uts: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub build: String,
    pub schema_digest: String,
    pub capabilities: [Capability; 1],
    pub namespaces: Namespaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    StructuredRole,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerFrame {
    Hello {
        seq: u32,
        body: Hello,
    },
    Complete {
        seq: u32,
        execution_id: String,
        lease_id: String,
        body: Completion,
    },
    Failed {
        seq: u32,
        execution_id: String,
        lease_id: String,
        body: Failed,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failed {
    pub code: Failure,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostFrame<'a> {
    HelloAck {
        v: u8,
        seq: u32,
        body: Ack,
    },
    Execute {
        v: u8,
        seq: u32,
        execution_id: &'a str,
        lease_id: &'a str,
        body: &'a Execute,
    },
    Cancel {
        v: u8,
        seq: u32,
        execution_id: &'a str,
        lease_id: &'a str,
        body: Empty,
    },
}

#[derive(Serialize)]
pub struct Ack {
    pub capabilities: [Capability; 1],
    pub max_frame_bytes: usize,
}

#[derive(Serialize)]
pub struct Empty {}

#[derive(Debug, thiserror::Error)]
#[error("worker frame is invalid")]
pub struct ProtocolError;

struct StrictJson(u8);

impl<'de> DeserializeSeed<'de> for StrictJson {
    type Value = Value;

    fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
        if self.0 > 32 {
            return Err(D::Error::custom("JSON nesting limit"));
        }
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for StrictJson {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded I-JSON")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        if value > MAX_SAFE_INTEGER {
            return Err(E::custom("JSON integer limit"));
        }
        Ok(Value::Number(value.into()))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        if value.unsigned_abs() > MAX_SAFE_INTEGER {
            return Err(E::custom("JSON integer limit"));
        }
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        if value.fract() == 0.0 && value.abs() > MAX_SAFE_INTEGER as f64 {
            return Err(E::custom("JSON integer limit"));
        }
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut output = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictJson(self.0 + 1))? {
            output.push(value);
        }
        Ok(Value::Array(output))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
        let mut keys = BTreeSet::new();
        let mut output = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(A::Error::custom("duplicate JSON field"));
            }
            let value = object.next_value_seed(StrictJson(self.0 + 1))?;
            output.insert(key, value);
        }
        Ok(Value::Object(output))
    }
}

pub fn parse_json(bytes: &[u8]) -> Result<Value, ProtocolError> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = StrictJson(1)
        .deserialize(&mut decoder)
        .map_err(|_| ProtocolError)?;
    decoder.end().map_err(|_| ProtocolError)?;
    Ok(value)
}

pub fn decode_worker_frame(frame: &[u8]) -> Result<WorkerFrame, ProtocolError> {
    if frame.len() >= MAX_FRAME || frame.contains(&b'\n') || frame.last() == Some(&b'\r') {
        return Err(ProtocolError);
    }
    let value = parse_json(frame)?;
    if !VALIDATOR.is_valid(&value) {
        return Err(ProtocolError);
    }
    serde_json::from_value(value).map_err(|_| ProtocolError)
}

pub async fn read_worker_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<WorkerFrame>, ProtocolError> {
    let mut bytes = Vec::with_capacity(8192);
    loop {
        let available = reader.fill_buf().await.map_err(|_| ProtocolError)?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(ProtocolError)
            };
        }
        let line_end = available.iter().position(|byte| *byte == b'\n');
        let count = line_end.unwrap_or(available.len());
        if bytes.len() + count >= MAX_FRAME {
            return Err(ProtocolError);
        }
        bytes.extend_from_slice(&available[..count]);
        if line_end.is_some() {
            reader.consume(count + 1);
            return decode_worker_frame(&bytes).map(Some);
        }
        reader.consume(count);
    }
}

pub async fn write_host_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    frame: HostFrame<'_>,
) -> Result<(), ProtocolError> {
    let mut bytes = serde_json::to_vec(&frame).map_err(|_| ProtocolError)?;
    if bytes.len() >= MAX_FRAME || !VALIDATOR.is_valid(&parse_json(&bytes)?) {
        return Err(ProtocolError);
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await.map_err(|_| ProtocolError)
}
