use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use crate::{AgentEvent, TodoItem, TodoStatus, ToolCall, UserInputQuestion};

const OUTPUT_CAP: usize = 4096;

#[derive(Default)]
pub(super) struct PartState {
    kind: String,
    emitted: usize,
    tool_started: bool,
    tool_done: bool,
}

#[derive(Default)]
pub(super) struct SessionFeed {
    pub(super) message_is_assistant: HashMap<String, bool>,
    pub(super) parts_awaiting_role: Vec<Value>,
    parts: HashMap<String, PartState>,
}

pub(super) struct ChildRun {
    pub(super) parent_tool_use_id: String,
    pub(super) feed: SessionFeed,
    pub(super) done: bool,
}

impl ChildRun {
    fn new(parent_tool_use_id: &str) -> Self {
        Self {
            parent_tool_use_id: parent_tool_use_id.to_owned(),
            feed: SessionFeed::default(),
            done: false,
        }
    }
}

pub(super) struct PendingSpawn {
    pub(super) tool_part_id: String,
    pub(super) description: String,
}

pub(super) type SpawnContext<'a> = (
    &'a mut HashMap<String, ChildRun>,
    &'a mut VecDeque<PendingSpawn>,
    &'a mut HashMap<String, String>,
);

pub(super) fn tag(parent: &str, event: AgentEvent) -> AgentEvent {
    AgentEvent::Subagent {
        parent_tool_use_id: parent.to_owned(),
        event: Box::new(event),
    }
}

fn is_spawn_tool(tool: &str) -> bool {
    matches!(tool, "task" | "subagent")
}

fn hold_part(feed: &mut SessionFeed, part: &Value, part_id: &str) {
    if !feed
        .parts_awaiting_role
        .iter()
        .any(|held| held.get("id").and_then(Value::as_str) == Some(part_id))
    {
        feed.parts_awaiting_role.push(part.clone());
    }
}

pub(super) fn part_snapshot_events(
    feed: &mut SessionFeed,
    part: &Value,
    is_main: bool,
    spawn_context: Option<SpawnContext<'_>>,
) -> Vec<AgentEvent> {
    let Some(part_id) = part.get("id").and_then(Value::as_str) else {
        return Vec::new();
    };
    let message_id = part
        .get("messageID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "text" | "reasoning" => {
            let role = feed.message_is_assistant.get(message_id).copied();
            if kind == "text" && role.is_none() {
                hold_part(feed, part, part_id);
                return Vec::new();
            }
            if kind == "text" && role == Some(false) {
                // On the main feed a user part is our own prompt echo; in a child it is the prompt sent to it.
                if is_main {
                    return Vec::new();
                }
                let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                if text.trim().is_empty() {
                    return Vec::new();
                }
                let entry = feed
                    .parts
                    .entry(part_id.to_owned())
                    .or_insert_with(|| PartState {
                        kind: kind.to_owned(),
                        ..PartState::default()
                    });
                if entry.emitted > 0 {
                    return Vec::new();
                }
                entry.emitted = text.len();
                return vec![AgentEvent::UserMessage {
                    text: text.to_owned(),
                }];
            }
            if role != Some(true) {
                if kind == "reasoning" {
                    hold_part(feed, part, part_id);
                }
                return Vec::new();
            }
            let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
            let entry = feed
                .parts
                .entry(part_id.to_owned())
                .or_insert_with(|| PartState {
                    kind: kind.to_owned(),
                    ..PartState::default()
                });
            let Some(suffix) = text
                .get(entry.emitted..)
                .filter(|suffix| !suffix.is_empty())
                .map(str::to_owned)
            else {
                return Vec::new();
            };
            entry.emitted = text.len();
            vec![if entry.kind == "reasoning" {
                AgentEvent::ReasoningDelta { text: suffix }
            } else {
                AgentEvent::TextDelta { text: suffix }
            }]
        }
        "tool" => {
            let tool = part.get("tool").and_then(Value::as_str).unwrap_or_default();
            let state = part.get("state");
            let status = state
                .and_then(|state| state.get("status"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let input = state
                .and_then(|state| state.get("input"))
                .cloned()
                .unwrap_or(Value::Null);
            // A spawn chip keys on the part id, which the child's completion also reports.
            let call_id = if is_spawn_tool(tool) && is_main {
                part_id.to_owned()
            } else {
                part.get("callID")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .unwrap_or(part_id)
                    .to_owned()
            };
            let entry = feed
                .parts
                .entry(part_id.to_owned())
                .or_insert_with(|| PartState {
                    kind: "tool".to_owned(),
                    ..PartState::default()
                });
            let mut events = Vec::new();
            let has_input = input.as_object().is_some_and(|input| !input.is_empty());
            if !entry.tool_started
                && (has_input || matches!(status, "running" | "completed" | "error"))
            {
                entry.tool_started = true;
                events.push(AgentEvent::ToolCall {
                    id: call_id.clone(),
                    call: tool_call(tool, &input),
                });
                if is_spawn_tool(tool)
                    && is_main
                    && let Some((children, pending, unbound)) = spawn_context
                {
                    register_spawn(children, pending, unbound, part, part_id, &input);
                }
            }
            if entry.tool_started && !entry.tool_done && matches!(status, "completed" | "error") {
                entry.tool_done = true;
                let output = state
                    .and_then(|state| {
                        state
                            .get("output")
                            .or_else(|| state.get("error"))
                            .and_then(Value::as_str)
                    })
                    .filter(|text| !text.is_empty())
                    .map(|text| cap_text(text, OUTPUT_CAP));
                events.push(AgentEvent::ToolResult {
                    id: call_id,
                    is_error: status == "error",
                    output,
                    diff: None,
                });
            }
            events
        }
        _ => Vec::new(),
    }
}

pub(super) fn part_delta_events(
    feed: &mut SessionFeed,
    properties: &Value,
    part_id: &str,
    delta: &str,
) -> Vec<AgentEvent> {
    if delta.is_empty() {
        return Vec::new();
    }
    let message_id = properties
        .get("messageID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if feed.message_is_assistant.get(message_id) != Some(&true) {
        return Vec::new();
    }
    let entry = feed
        .parts
        .entry(part_id.to_owned())
        .or_insert_with(|| PartState {
            kind: "text".to_owned(),
            ..PartState::default()
        });
    if entry.kind == "tool" {
        return Vec::new();
    }
    entry.emitted += delta.len();
    vec![if entry.kind == "reasoning" {
        AgentEvent::ReasoningDelta {
            text: delta.to_owned(),
        }
    } else {
        AgentEvent::TextDelta {
            text: delta.to_owned(),
        }
    }]
}

pub(super) fn task_completion(part: &Value) -> Option<(String, bool)> {
    if part.get("type").and_then(Value::as_str) != Some("tool")
        || !part
            .get("tool")
            .and_then(Value::as_str)
            .is_some_and(is_spawn_tool)
    {
        return None;
    }
    let state = part.get("state")?;
    let status = state.get("status").and_then(Value::as_str)?;
    if !matches!(status, "completed" | "error") {
        return None;
    }
    Some((
        child_session_id(state).unwrap_or_default().to_owned(),
        status == "error",
    ))
}

fn child_session_id(state: &Value) -> Option<&str> {
    state
        .get("metadata")
        .and_then(|metadata| {
            metadata
                .get("sessionId")
                .or_else(|| metadata.get("sessionID"))
        })
        .and_then(Value::as_str)
}

/// Binds by the child title (`"{description} (@{agent} subagent)"`), else first in line.
pub(super) fn bind_child(
    children: &mut HashMap<String, ChildRun>,
    pending: &mut VecDeque<PendingSpawn>,
    child_id: &str,
    title: &str,
) -> bool {
    let index = pending
        .iter()
        .position(|spawn| !spawn.description.is_empty() && title.starts_with(&spawn.description))
        .or((!pending.is_empty()).then_some(0));
    match index.and_then(|index| pending.remove(index)) {
        Some(spawn) => {
            children.insert(child_id.to_owned(), ChildRun::new(&spawn.tool_part_id));
            true
        }
        None => false,
    }
}

fn register_spawn(
    children: &mut HashMap<String, ChildRun>,
    pending: &mut VecDeque<PendingSpawn>,
    unbound: &mut HashMap<String, String>,
    part: &Value,
    part_id: &str,
    input: &Value,
) {
    if pending.iter().any(|spawn| spawn.tool_part_id == part_id)
        || children
            .values()
            .any(|child| child.parent_tool_use_id == part_id)
    {
        return;
    }
    let child_id = part
        .get("state")
        .and_then(child_session_id)
        .unwrap_or_default();
    if !child_id.is_empty() && !children.contains_key(child_id) {
        unbound.remove(child_id);
        children.insert(child_id.to_owned(), ChildRun::new(part_id));
        return;
    }
    let description = input
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let matched = unbound
        .iter()
        .find(|(_, title)| !description.is_empty() && title.starts_with(&description))
        .map(|(id, _)| id.clone())
        .or_else(|| {
            (unbound.len() == 1)
                .then(|| unbound.keys().next().cloned())
                .flatten()
        });
    if let Some(id) = matched {
        unbound.remove(&id);
        children.insert(id, ChildRun::new(part_id));
        return;
    }
    pending.push_back(PendingSpawn {
        tool_part_id: part_id.to_owned(),
        description,
    });
}

pub(super) fn map_questions(properties: &Value) -> Vec<UserInputQuestion> {
    properties
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, question)| {
            Some(UserInputQuestion {
                id: format!("q{index}"),
                header: question
                    .get("header")
                    .and_then(Value::as_str)
                    .unwrap_or("Question")
                    .to_owned(),
                question: question.get("question").and_then(Value::as_str)?.to_owned(),
                options: question
                    .get("options")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|option| option.get("label").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect(),
                prefill: None,
                multiline: false,
                multi_select: question
                    .get("multiple")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

pub(super) fn cap_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub(super) fn tool_call(name: &str, input: &Value) -> ToolCall {
    let field = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| input.get(*key))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let path = || field(&["filePath", "file_path", "path"]);
    match name {
        "bash" => ToolCall::Exec {
            command: field(&["command"]).unwrap_or_default(),
        },
        "read" => ToolCall::ReadFile {
            path: path().unwrap_or_default(),
        },
        "write" => ToolCall::WriteFile {
            path: path().unwrap_or_default(),
            content: field(&["content"]),
        },
        "edit" => ToolCall::EditFile {
            path: path().unwrap_or_default(),
            old_string: field(&["oldString", "old_string"]),
            new_string: field(&["newString", "new_string"]),
        },
        "patch" | "apply_patch" => ToolCall::ApplyPatch { path: path() },
        "grep" => ToolCall::Search {
            pattern: field(&["pattern"]).unwrap_or_default(),
            path: field(&["path", "include"]),
        },
        "glob" => ToolCall::Glob {
            pattern: field(&["pattern"]).unwrap_or_default(),
        },
        "webfetch" => ToolCall::WebFetch {
            url: field(&["url"]).unwrap_or_default(),
            prompt: None,
        },
        "websearch" => ToolCall::WebSearch {
            query: field(&["query"]).unwrap_or_default(),
        },
        "todowrite" => ToolCall::Todo {
            items: input
                .get("todos")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|todo| {
                    TodoItem::new(
                        todo.get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        TodoStatus::parse(
                            todo.get("status")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        ),
                    )
                })
                .collect(),
        },
        "task" | "subagent" => ToolCall::Unknown {
            name: field(&["description"])
                .map(|description| format!("Agent: {description}"))
                .unwrap_or_else(|| "Agent".into()),
            input: (!input.is_null()).then(|| input.clone()),
        },
        _ => ToolCall::Unknown {
            name: name.to_owned(),
            input: (!input.is_null()).then(|| input.clone()),
        },
    }
}

pub(super) fn context_usage_event(
    info: &Value,
    context_windows: &HashMap<String, u64>,
) -> Option<AgentEvent> {
    let tokens = info.get("tokens")?;
    let counts: Vec<u64> = [
        tokens.get("input"),
        tokens.get("output"),
        tokens.pointer("/cache/read"),
        tokens.pointer("/cache/write"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_u64)
    .collect();
    let tokens = tokens
        .get("total")
        .and_then(Value::as_u64)
        .filter(|total| *total > 0)
        .or_else(|| {
            (!counts.is_empty()).then(|| counts.into_iter().fold(0u64, u64::saturating_add))
        });
    // New assistant placeholders report zero before the request runs.
    if tokens == Some(0) && info.pointer("/time/completed").is_none() {
        return None;
    }
    let window = info
        .get("providerID")
        .and_then(Value::as_str)
        .zip(info.get("modelID").and_then(Value::as_str))
        .and_then(|(provider, model)| context_windows.get(&format!("{provider}/{model}")).copied());
    (tokens.is_some() || window.is_some()).then_some(AgentEvent::ContextUsage { tokens, window })
}
