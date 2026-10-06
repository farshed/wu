use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug)]
pub(crate) enum Frame {
    System(SystemFrame),
    StreamEvent(StreamEventFrame),
    Assistant(MessageFrame),
    User(MessageFrame),
    RateLimit(RateLimitFrame),
    Result(ResultFrame),
    ControlRequest(ControlRequestFrame),
    Other,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SystemFrame {
    #[serde(default)]
    pub subtype: String,
    #[serde(default, rename = "permissionMode")]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default, alias = "toolUseId")]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, alias = "taskId")]
    pub task_id: Option<String>,
    #[serde(default)]
    pub subagent_type: Option<String>,
    #[serde(default, alias = "compactMetadata")]
    pub compact_metadata: Option<Value>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub task_type: Option<String>,
    #[serde(default)]
    pub tasks: Vec<WireBackgroundTask>,
    #[serde(default)]
    pub compact_result: Option<String>,
    #[serde(default)]
    pub compact_error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireBackgroundTask {
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub task_type: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub ambient: bool,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct StreamEventFrame {
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub event: StreamEventBody,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct StreamEventBody {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub delta: Delta,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Delta {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub thinking: String,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct MessageFrame {
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub message: MessageBody,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct MessageBody {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default)]
    pub content: Value,
}

impl MessageBody {
    pub fn blocks(&self) -> impl Iterator<Item = ContentBlock> + '_ {
        self.content
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|b| serde_json::from_value(b.clone()).ok())
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ContentBlock {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub tool_use_id: String,
    #[serde(default)]
    pub is_error: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimitFrame {
    #[serde(default)]
    pub rate_limit_info: RateLimitInfo,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimitInfo {
    #[serde(default)]
    pub status: String,
    #[serde(rename = "rateLimitType", default)]
    pub rate_limit_type: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ResultFrame {
    #[serde(default, rename = "modelUsage")]
    pub model_usage: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub errors: Vec<Value>,
    #[serde(default)]
    pub usage: UsageBody,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct UsageBody {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ControlRequestFrame {
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub request: ControlRequestBody,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ControlRequestBody {
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub input: Value,
}

pub(crate) fn parse_frame(line: &str) -> Result<Frame, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    let frame = match kind {
        "system" => Frame::System(serde_json::from_value(value)?),
        "stream_event" => Frame::StreamEvent(serde_json::from_value(value)?),
        "assistant" => Frame::Assistant(serde_json::from_value(value)?),
        "user" => Frame::User(serde_json::from_value(value)?),
        "rate_limit_event" => Frame::RateLimit(serde_json::from_value(value)?),
        "result" => Frame::Result(serde_json::from_value(value)?),
        "control_request" => Frame::ControlRequest(serde_json::from_value(value)?),
        _ => Frame::Other,
    };
    Ok(frame)
}

pub(crate) struct ImageBlock {
    pub media_type: &'static str,
    pub base64_data: String,
}

fn user_content(text: &str, images: &[ImageBlock]) -> Value {
    if images.is_empty() {
        return Value::String(text.to_owned());
    }
    let mut blocks: Vec<Value> = images
        .iter()
        .map(|image| {
            json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.media_type,
                    "data": image.base64_data,
                },
            })
        })
        .collect();
    // The API rejects empty text blocks, which image-only messages would otherwise carry.
    if !text.trim().is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    Value::Array(blocks)
}

pub(crate) fn user_message_line(text: &str, images: &[ImageBlock]) -> String {
    json!({
        "type": "user",
        "message": { "role": "user", "content": user_content(text, images) },
        "parent_tool_use_id": null,
    })
    .to_string()
}

/// Only pass `immediate` when no tool is open: priority `now` aborts in-flight tool calls.
pub(crate) fn steer_message_line(
    text: &str,
    images: &[ImageBlock],
    id: &str,
    immediate: bool,
) -> String {
    json!({
        "type": "user",
        "uuid": id,
        "priority": if immediate { "now" } else { "next" },
        "message": { "role": "user", "content": user_content(text, images) },
        "parent_tool_use_id": null,
    })
    .to_string()
}

pub(crate) fn control_response_line(request_id: &str, response: Value) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
    .to_string()
}

pub(crate) fn allow_response(updated_input: Value) -> Value {
    json!({ "behavior": "allow", "updatedInput": updated_input })
}

pub(crate) fn deny_response(message: &str) -> Value {
    json!({ "behavior": "deny", "message": message })
}

pub(crate) fn interrupt_request_line(request_id: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "interrupt" },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_only_messages_carry_no_empty_text_block() {
        let image = ImageBlock {
            media_type: "image/png",
            base64_data: "AAAA".into(),
        };
        let content = user_content("  ", std::slice::from_ref(&image));
        assert_eq!(content.as_array().map(Vec::len), Some(1));
        assert_eq!(content[0]["type"], "image");
        assert_eq!(user_content("hi", &[]), serde_json::Value::String("hi".into()));
    }

    #[test]
    fn steer_priority_follows_tool_state() {
        let now: serde_json::Value =
            serde_json::from_str(&steer_message_line("hi", &[], "u1", true)).unwrap();
        let next: serde_json::Value =
            serde_json::from_str(&steer_message_line("hi", &[], "u2", false)).unwrap();
        assert_eq!(now["priority"], "now");
        assert_eq!(next["priority"], "next");
        assert_eq!(next["uuid"], "u2");
    }

    #[test]
    fn parses_known_and_unknown_frames() {
        let init = r#"{"type":"system","subtype":"init","model":"m","tools":["Bash"],"cwd":"/x","session_id":"s1"}"#;
        match parse_frame(init).expect("parses") {
            Frame::System(f) => {
                assert_eq!(f.subtype, "init");
                assert_eq!(f.session_id, "s1");
            }
            other => panic!("unexpected frame: {other:?}"),
        }
        assert!(matches!(
            parse_frame(r#"{"type":"mystery_frame"}"#).expect("parses"),
            Frame::Other
        ));
        assert!(parse_frame("not json").is_err());
    }

    #[test]
    fn user_line_shape_matches_protocol() {
        let line = user_message_line("hi", &[]);
        let v: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"], "hi");
        assert!(v["parent_tool_use_id"].is_null());
    }

    #[test]
    fn images_precede_the_text_block() {
        let images = [ImageBlock {
            media_type: "image/png",
            base64_data: "AAAA".into(),
        }];
        for line in [
            user_message_line("look", &images),
            steer_message_line("look", &images, "u1", true),
        ] {
            let v: Value = serde_json::from_str(&line).expect("json");
            let content = v["message"]["content"].as_array().expect("blocks");
            assert_eq!(content.len(), 2);
            assert_eq!(content[0]["type"], "image");
            assert_eq!(content[0]["source"]["type"], "base64");
            assert_eq!(content[0]["source"]["media_type"], "image/png");
            assert_eq!(content[0]["source"]["data"], "AAAA");
            assert_eq!(content[1], json!({ "type": "text", "text": "look" }));
        }
    }
}
