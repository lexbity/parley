//! Closed JSON Lines codec for the private Parley ↔ worker contract (AR-001).
//!
//! Every frame is schema-checked and additionally checked with Rust type
//! rules that JSON Schema cannot express (safe integers, duplicate keys,
//! depth, monotonic sequence, execution/lease binding in the supervisor).

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
    /// Embedding identity for the `embed` mode. Present only when the profile
    /// has a separately qualified embedding model; chat executions leave them
    /// unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_dimensions: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Coach,
    TemplateBuilder,
    Interviewer,
    Briefer,
    Extractor,
    Matcher,
    Composer,
    Linter,
}

/// One server correlation id bound to a named, frozen tool descriptor. The
/// SDK id never becomes business idempotency; the host assigns ordinal keys.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CallRef {
    pub call_ref: String,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegatedMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<CallRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultEntry {
    pub ordinal: u32,
    pub result: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Continuation {
    pub turn_no: u32,
    pub results: Vec<ToolResultEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Execute {
    Probe {
        profile: Profile,
        deadline_ms: u32,
        /// The capability being probed. A structured probe exercises chat
        /// output; an embed probe exercises the embedding transport.
        capability: Capability,
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
    /// One bounded Pi Agent Core step over a canonical host transcript. The
    /// worker gets frozen descriptors only; every tool result comes from the
    /// host journal, never from the model or an ambient tool registry.
    Delegated {
        profile: Profile,
        deadline_ms: u32,
        task: String,
        transcript: Vec<DelegatedMessage>,
        tools: Vec<AuthorizedTool>,
        continuation: Option<Continuation>,
    },
    /// One bounded embedding batch through the same Unix relay. The worker
    /// verifies response order, exact declared dimensions and finite values;
    /// the host partitions batches so one frame never exceeds the cap.
    Embed {
        profile: Profile,
        deadline_ms: u32,
        inputs: Vec<String>,
        dimensions: u32,
    },
}

impl Execute {
    pub fn profile(&self) -> &Profile {
        match self {
            Self::Probe { profile, .. }
            | Self::StructuredRole { profile, .. }
            | Self::Delegated { profile, .. }
            | Self::Embed { profile, .. } => profile,
        }
    }

    pub fn deadline_ms(&self) -> u32 {
        match self {
            Self::Probe { deadline_ms, .. }
            | Self::StructuredRole { deadline_ms, .. }
            | Self::Delegated { deadline_ms, .. }
            | Self::Embed { deadline_ms, .. } => *deadline_ms,
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
                | (Self::Delegated { .. }, Completion::Delegated { .. })
                | (Self::Embed { .. }, Completion::Embed { .. })
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
        /// Present for an embed capability probe; the observed vector width.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dimensions: Option<u32>,
    },
    StructuredRole {
        observed: ObservedModel,
        usage: Usage,
        candidate: Value,
    },
    Delegated {
        observed: ObservedModel,
        usage: Usage,
        text: String,
    },
    Embed {
        observed: ObservedModel,
        usage: Usage,
        vectors: Vec<Vec<f32>>,
    },
}

impl Completion {
    pub fn observation(&self) -> (&ObservedModel, &Usage) {
        match self {
            Self::Probe {
                observed, usage, ..
            }
            | Self::StructuredRole {
                observed, usage, ..
            }
            | Self::Delegated {
                observed, usage, ..
            }
            | Self::Embed {
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
    pub capabilities: Vec<Capability>,
    pub namespaces: Namespaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    StructuredRole,
    Delegated,
    Embed,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerFrame {
    Hello {
        seq: u32,
        body: Hello,
    },
    /// A complete assistant message with its ordered, correlation-bound call
    /// list. No tool may run before the host journals and accepts this turn.
    AssistantTurn {
        seq: u32,
        execution_id: String,
        lease_id: String,
        body: AssistantTurn,
    },
    ToolRequest {
        seq: u32,
        execution_id: String,
        lease_id: String,
        body: ToolRequest,
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
pub struct AssistantTurn {
    pub text: String,
    pub calls: Vec<CallRef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequest {
    pub turn_no: u32,
    pub ordinal: u32,
    pub name: String,
    pub args: Value,
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
    TurnAccepted {
        v: u8,
        seq: u32,
        execution_id: &'a str,
        lease_id: &'a str,
        body: &'a TurnAccepted,
    },
    ToolResult {
        v: u8,
        seq: u32,
        execution_id: &'a str,
        lease_id: &'a str,
        body: &'a ToolResult,
    },
    Suspend {
        v: u8,
        seq: u32,
        execution_id: &'a str,
        lease_id: &'a str,
        body: &'a Suspend,
    },
    Cancel {
        v: u8,
        seq: u32,
        execution_id: &'a str,
        lease_id: &'a str,
        body: Empty,
    },
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TurnAccepted {
    pub turn_no: u32,
    pub ordinals: Vec<u32>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResult {
    pub turn_no: u32,
    pub ordinal: u32,
    pub result: Value,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Suspend {
    pub reason: &'static str,
}

#[derive(Serialize)]
pub struct Ack {
    pub capabilities: Vec<Capability>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::BufReader;

    fn hello() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "v": 1,
            "kind": "hello",
            "seq": 1,
            "body": {
                "build": "a".repeat(64),
                "schema_digest": "b".repeat(64),
                "capabilities": ["structured_role", "delegated", "embed"],
                "namespaces": {
                    "user": "user:[1]", "net": "net:[2]", "pid": "pid:[3]",
                    "ipc": "ipc:[4]", "uts": "uts:[5]"
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn ar01_json_reader_rejects_duplicate_invalid_and_oversized_values() {
        for invalid in [
            &b"{\"field\":1,\"field\":2}"[..],
            b"\xef\xbb\xbf{}",
            b"{} trailing",
            b"\xff",
            b"9007199254740992",
            b"1e400",
        ] {
            assert!(parse_json(invalid).is_err());
        }
        assert_eq!(
            parse_json(b"-9007199254740991").unwrap(),
            -9_007_199_254_740_991i64
        );
        let nested = format!("{}0{}", "[".repeat(31), "]".repeat(31));
        assert!(parse_json(nested.as_bytes()).is_ok());
        let too_deep = format!("{}0{}", "[".repeat(32), "]".repeat(32));
        assert!(parse_json(too_deep.as_bytes()).is_err());
    }

    #[test]
    fn ar01_frame_decoder_enforces_schema_and_line_bounds() {
        let frame = hello();
        assert!(matches!(
            decode_worker_frame(&frame),
            Ok(WorkerFrame::Hello { seq: 1, .. })
        ));
        for invalid in [
            serde_json::to_vec(&serde_json::json!({"v":2,"kind":"hello","seq":1,"body":{}}))
                .unwrap(),
            serde_json::to_vec(
                &serde_json::json!({"v":1,"kind":"hello","seq":1,"body":{},"extra":true}),
            )
            .unwrap(),
            [frame.clone(), b"\n".to_vec()].concat(),
            [frame.clone(), b"\r".to_vec()].concat(),
            vec![b'x'; MAX_FRAME],
        ] {
            assert!(decode_worker_frame(&invalid).is_err());
        }
    }

    #[test]
    fn ar05_delegated_frames_are_closed_and_lease_bound() {
        let ids = ("exec_1".to_owned(), "lease_1".to_owned());
        let turn = serde_json::json!({
            "v":1,"kind":"assistant_turn","seq":3,
            "execution_id":ids.0,"lease_id":ids.1,
            "body":{"text":"","calls":[{"call_ref":"sdk_1","name":"commands__run","args":{"id":"brief.start"}}]}
        });
        assert!(matches!(
            decode_worker_frame(&serde_json::to_vec(&turn).unwrap()),
            Ok(WorkerFrame::AssistantTurn { seq: 3, .. })
        ));
        let mut bad = turn.clone();
        bad["body"]["calls"][0]["call_id"] = json!("extra");
        assert!(decode_worker_frame(&serde_json::to_vec(&bad).unwrap()).is_err());
        let mut requests = serde_json::json!({
            "v":1,"kind":"tool_request","seq":4,
            "execution_id":ids.0,"lease_id":ids.1,
            "body":{"turn_no":1,"ordinal":1,"name":"commands__run","args":{"id":"brief.start"}}
        });
        assert!(matches!(
            decode_worker_frame(&serde_json::to_vec(&requests).unwrap()),
            Ok(WorkerFrame::ToolRequest { .. })
        ));
        requests["body"]["ordinal"] = json!(5);
        assert!(decode_worker_frame(&serde_json::to_vec(&requests).unwrap()).is_err());
    }

    #[tokio::test]
    async fn ar01_frame_stream_handles_eof_and_truncation() {
        let mut reader = BufReader::new(&b""[..]);
        assert!(read_worker_frame(&mut reader).await.unwrap().is_none());
        let mut reader = BufReader::new(&b"{\"v\":1}"[..]);
        assert!(read_worker_frame(&mut reader).await.is_err());
        let mut bytes = hello();
        bytes.push(b'\n');
        bytes.extend_from_slice(&hello());
        let mut reader = BufReader::new(bytes.as_slice());
        assert!(matches!(
            read_worker_frame(&mut reader).await.unwrap(),
            Some(WorkerFrame::Hello { .. })
        ));
        assert!(read_worker_frame(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn ar01_host_frames_are_schema_checked_and_lf_terminated() {
        let mut output = Vec::new();
        write_host_frame(
            &mut output,
            HostFrame::HelloAck {
                v: 1,
                seq: 1,
                body: Ack {
                    capabilities: vec![
                        Capability::StructuredRole,
                        Capability::Delegated,
                        Capability::Embed,
                    ],
                    max_frame_bytes: MAX_FRAME,
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(output.last(), Some(&b'\n'));
        assert!(VALIDATOR.is_valid(&parse_json(&output[..output.len() - 1]).unwrap()));
    }
}
