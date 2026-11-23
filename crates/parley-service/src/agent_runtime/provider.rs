//! Bounded loopback tool transport (UCR-001 Q7).
//!
//! The delegated runtime reuses the fixed-role provider configuration and
//! endpoint validation instead of adding a second provider subsystem. Tool
//! calls carry no owner credential, no session cookie and no filesystem or
//! generic HTTP access. The probe is separate from model health so health
//! checks do not imply tool support.

use crate::jobs::{self, ProviderConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const MAX_CHAT_BODY: usize = 1_048_576;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolCall {
    pub id: String,
    pub function: ToolCallFunction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

/// A minimal tool-capable probe. It sends one tiny tool definition and only
/// checks that the loopback provider accepts it; it never runs a command.
pub(crate) async fn tool_capable(config: &ProviderConfig) -> Result<bool, String> {
    if config.model.trim().is_empty() {
        return Err("no local model is configured".into());
    }
    let tools = json!([{
        "type": "function",
        "function": {
            "name": "commands__describe",
            "description": "Describe a registered command",
            "parameters": {"type": "object", "additionalProperties": false, "properties": {"id": {"type": "string"}}, "required": ["id"]}
        }
    }]);
    let messages =
        json!([{"role": "user", "content": "Tool support probe. Do not call the tool."}]);
    chat(config, messages, tools).await.map(|_| true)
}

/// One assistant turn with the exposed tool schemas. The request is bounded
/// and the response is parsed into at most `MAX_TOOL_CALLS` typed calls.
pub(crate) const MAX_TOOL_CALLS: usize = 4;

pub(crate) async fn chat(
    config: &ProviderConfig,
    messages: Value,
    tools: Value,
) -> Result<ChatMessage, String> {
    let (host, port) = jobs::validate_endpoint(&config.endpoint)?;
    let body = json!({
        "model": config.model,
        "stream": false,
        "messages": messages,
        "tools": tools,
        "options": {"num_ctx": config.num_ctx, "num_predict": config.num_predict, "temperature": 0.2}
    })
    .to_string();
    let mut stream = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|error| format!("provider connection failed: {error}"))?;
    let request = format!(
        "POST /api/chat HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("provider write failed: {error}"))?;
    let mut response = String::new();
    stream
        .take((MAX_CHAT_BODY + 8192) as u64)
        .read_to_string(&mut response)
        .await
        .map_err(|error| format!("provider read failed: {error}"))?;
    let (head, payload) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "provider response has no body".to_owned())?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    if status != 200 {
        return Err(format!("provider returned status {status}"));
    }
    // Loopback fixtures may answer with a plain body or with Ollama's chunk
    // framing; both are bounded by the same read cap.
    let decoded = match serde_json::from_str::<ChatResponse>(payload) {
        Ok(parsed) => parsed,
        Err(_) => {
            let joined = jobs::decode_chunks(payload)?;
            serde_json::from_str::<ChatResponse>(&joined)
                .map_err(|error| format!("provider response is not a tool chat: {error}"))?
        }
    };
    let parsed: ChatResponse = decoded;
    let mut message = parsed.message;
    if message.content.len() > 32 * 1024 {
        return Err("assistant text exceeds the bound".into());
    }
    if let Some(calls) = message.tool_calls.as_mut() {
        if calls.len() > MAX_TOOL_CALLS {
            return Err("too many tool calls in one turn".into());
        }
        for call in calls.iter() {
            if call.id.is_empty() || call.id.len() > 200 || call.function.name.len() > 64 {
                return Err("malformed tool call".into());
            }
            if call.function.arguments.len() > 16 * 1024 {
                return Err("tool arguments exceed the bound".into());
            }
        }
    }
    Ok(message)
}
