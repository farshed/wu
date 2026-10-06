use std::collections::HashMap;

use serde_json::{Value, json};

pub(super) type V2ToolKey = (String, String, String);
pub(super) const MAX_PENDING_V2_TOOLS: usize = 4096;
pub(super) const MAX_V2_SESSION_MODELS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct V2ModelIdentity {
    pub(super) provider_id: String,
    pub(super) model_id: String,
}

fn model_identity(model: &Value) -> Option<V2ModelIdentity> {
    let provider_id = model.get("providerID")?.as_str()?;
    let model_id = model.get("id")?.as_str()?;
    (!provider_id.is_empty() && !model_id.is_empty()).then(|| V2ModelIdentity {
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
    })
}

#[cfg(test)]
pub(super) fn normalize_v2_frame(
    event: Value,
    tool_names: &mut HashMap<V2ToolKey, String>,
) -> Vec<Value> {
    normalize_v2_frame_with_session_models(event, tool_names, &mut HashMap::new())
}

fn str_field<'a>(data: &'a Value, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Rewrites a 2.x `/api/event` frame into the 1.x `{type, properties}` payloads the turn loop reads.
pub(super) fn normalize_v2_frame_with_session_models(
    event: Value,
    tool_names: &mut HashMap<V2ToolKey, String>,
    session_models: &mut HashMap<String, V2ModelIdentity>,
) -> Vec<Value> {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    let session_id = str_field(&data, "sessionID");
    if session_id.is_empty() {
        return Vec::new();
    }
    let session = || data.get("sessionID").cloned().unwrap_or(Value::Null);
    let message = || {
        data.get("assistantMessageID")
            .cloned()
            .unwrap_or(Value::Null)
    };
    let tool_key = || {
        (
            session_id.to_owned(),
            str_field(&data, "assistantMessageID").to_owned(),
            str_field(&data, "id").to_owned(),
        )
    };
    if matches!(
        kind,
        "session.execution.succeeded"
            | "session.execution.interrupted"
            | "session.execution.failed"
    ) {
        tool_names.retain(|(owner, _, _), _| owner != session_id);
    }
    match kind {
        "session.status" => vec![json!({"type": "session.status", "properties": data})],
        "session.retry.scheduled" => vec![json!({
            "type": "session.status",
            "properties": {"sessionID": session(), "status": {
                "type": "retry", "attempt": data.get("attempt"), "next": data.get("at"),
                "message": data.pointer("/error/message").and_then(Value::as_str).filter(|s| !s.is_empty())
                    .or_else(|| data.pointer("/error/type").and_then(Value::as_str)).unwrap_or("provider retry"),
            }}
        })],
        "session.tool.progress" => {
            let name = tool_names
                .get(&tool_key())
                .map(String::as_str)
                .unwrap_or_default();
            vec![tool_part(
                &data,
                str_field(&data, "id"),
                name,
                &json!({"status": "running", "metadata": data.get("metadata")}),
            )]
        }
        "session.execution.started" => vec![json!({
            "type": "session.status",
            "properties": { "sessionID": session(), "status": { "type": "busy" } }
        })],
        "session.execution.interrupted" => vec![json!({
            "type": "session.interrupted",
            "properties": { "sessionID": session() }
        })],
        "session.execution.succeeded" => vec![json!({
            "type": "session.idle",
            "properties": { "sessionID": session() }
        })],
        "session.execution.failed" => vec![
            error_payload(&data),
            json!({
                "type": "session.idle",
                "properties": { "sessionID": session() }
            }),
        ],
        "session.step.failed" => {
            // An interrupt echoes as an aborted step; execution.interrupted settles it.
            if data.pointer("/error/type").and_then(Value::as_str) == Some("aborted") {
                return Vec::new();
            }
            let mut warning = error_payload(&data);
            warning["type"] = json!("session.warning");
            vec![warning]
        }
        "session.step.started" => {
            if let Some(model) = data.get("model").and_then(model_identity) {
                session_models.insert(session_id.to_owned(), model);
            }
            vec![json!({
                "type": "message.updated",
                "properties": {
                    "info": { "sessionID": session(), "id": message(), "role": "assistant" }
                }
            })]
        }
        "session.text.started" | "session.text.ended" => vec![stream_part(
            &data,
            "text",
            data.get("text").cloned().unwrap_or(json!("")),
        )],
        "session.text.delta" | "session.reasoning.delta" => vec![json!({
            "type": "message.part.delta",
            "properties": {
                "sessionID": session(),
                "messageID": message(),
                "partID": part_id(&data, if kind == "session.reasoning.delta" { 'r' } else { 't' }),
                "field": "text",
                "delta": data.get("delta").cloned().unwrap_or(json!("")),
            }
        })],
        "session.reasoning.started" | "session.reasoning.ended" => vec![stream_part(
            &data,
            "reasoning",
            data.get("text").cloned().unwrap_or(json!("")),
        )],
        "session.tool.input.started" => {
            let name = str_field(&data, "name");
            tool_names.insert(tool_key(), name.to_owned());
            vec![tool_part(
                &data,
                str_field(&data, "id"),
                name,
                &json!({ "status": "pending" }),
            )]
        }
        "session.tool.called" => {
            let name = tool_names
                .get(&tool_key())
                .map(String::as_str)
                .unwrap_or_default();
            let state = json!({
                "status": "running",
                "input": data.get("input").cloned().unwrap_or(json!({})),
            });
            vec![tool_part(&data, str_field(&data, "id"), name, &state)]
        }
        "session.tool.success" => {
            let name = tool_names.remove(&tool_key()).unwrap_or_default();
            let output = data
                .get("content")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            vec![tool_part(
                &data,
                str_field(&data, "id"),
                &name,
                &json!({ "status": "completed", "output": output }),
            )]
        }
        "session.tool.failed" | "session.tool.error" => {
            let name = tool_names.remove(&tool_key()).unwrap_or_default();
            let error = data.get("error").cloned().unwrap_or(Value::Null);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .unwrap_or_default();
            vec![tool_part(
                &data,
                str_field(&data, "id"),
                &name,
                &json!({ "status": "error", "error": message }),
            )]
        }
        "session.step.ended" => {
            let tokens = data.get("tokens").cloned().unwrap_or(Value::Null);
            if !tokens.is_object() {
                return Vec::new();
            }
            let mut info = json!({
                "sessionID": session(),
                "id": "usage",
                "role": "assistant",
                "tokens": tokens,
            });
            if let Some(model) = session_models.get(session_id) {
                info["providerID"] = json!(model.provider_id);
                info["modelID"] = json!(model.model_id);
            }
            vec![json!({
                "type": "message.updated",
                "properties": { "info": info }
            })]
        }
        // Cumulative session totals measure spend, not context occupancy.
        "session.usage.updated" => Vec::new(),
        "session.created" => vec![json!({
            "type": "session.created",
            "properties": {
                "info": {
                    "id": session_id,
                    "parentID": data.get("parentID").cloned().unwrap_or(Value::Null),
                    "title": data
                        .get("title")
                        .or_else(|| data.get("slug"))
                        .cloned()
                        .unwrap_or(Value::Null),
                }
            }
        })],
        "permission.asked" => {
            if data.get("id").and_then(Value::as_str).is_none() {
                return Vec::new();
            }
            vec![json!({
                "type": "permission.asked",
                "properties": data
            })]
        }
        _ => Vec::new(),
    }
}

// 2.x provider failures can carry an empty message; the type keeps the chip readable.
fn error_payload(data: &Value) -> Value {
    let error = data.get("error").cloned().unwrap_or(Value::Null);
    let name = error.get("type").and_then(Value::as_str).unwrap_or("");
    let message = match error.get("message").and_then(Value::as_str) {
        Some(message) if !message.is_empty() => message,
        _ => name,
    };
    json!({
        "type": "session.error",
        "properties": {
            "sessionID": data.get("sessionID").cloned().unwrap_or(Value::Null),
            "error": {
                "name": name,
                "data": { "message": message },
            },
        }
    })
}

fn part_id(data: &Value, kind: char) -> String {
    let message = str_field(data, "assistantMessageID");
    let ordinal = data.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
    format!("{message}:{kind}{ordinal}")
}

fn stream_part(data: &Value, part_type: &str, text: Value) -> Value {
    let kind = if part_type == "text" { 't' } else { 'r' };
    json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "sessionID": data.get("sessionID").cloned().unwrap_or(Value::Null),
            "id": part_id(data, kind),
            "messageID": data.get("assistantMessageID").cloned().unwrap_or(Value::Null),
            "type": part_type,
            "text": text,
        }}
    })
}

fn tool_part(data: &Value, id: &str, name: &str, state: &Value) -> Value {
    // Provider call ids repeat across assistant messages.
    let id = format!(
        "{}:{}:{id}",
        str_field(data, "sessionID"),
        str_field(data, "assistantMessageID")
    );
    json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "sessionID": data.get("sessionID").cloned().unwrap_or(Value::Null),
            "messageID": data.get("assistantMessageID").cloned().unwrap_or(Value::Null),
            "id": id,
            "callID": id,
            "type": "tool",
            "tool": name,
            "state": state,
        }}
    })
}
