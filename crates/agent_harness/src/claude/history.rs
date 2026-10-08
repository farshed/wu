use std::collections::{HashMap, HashSet};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use serde_json::Value;

use super::normalize::decode_tool_use;
use crate::{AgentEvent, ExternalSession, HarnessError};

const TITLE_LIMIT: usize = 120;

/// The CLI names each project folder after its path with every other character turned into `-`.
fn project_dir(config_dir: &Path, cwd: &Path) -> PathBuf {
    let encoded: String = cwd
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    config_dir.join("projects").join(encoded)
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn records(path: &Path) -> std::io::Result<Vec<Value>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::with_capacity(1 << 16, file);
    let mut records = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(records);
        }
        if let Ok(record) = serde_json::from_slice::<Value>(&line) {
            records.push(record);
        }
    }
}

fn tag_value<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

/// What the user typed, or `None` for text the CLI added on its own.
fn typed_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(name) = tag_value(trimmed, "command-name") {
        let arguments = tag_value(trimmed, "command-args").unwrap_or_default();
        let command = if arguments.is_empty() {
            name.to_owned()
        } else {
            format!("{name} {arguments}")
        };
        return Some(command);
    }
    if let Some(command) = tag_value(trimmed, "bash-input") {
        return Some(format!("! {command}"));
    }
    if trimmed.starts_with('<') || trimmed.starts_with("[Request interrupted") {
        return None;
    }
    Some(trimmed.to_owned())
}

fn user_text(record: &Value) -> Option<String> {
    if record.get("isMeta").and_then(Value::as_bool) == Some(true)
        || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let content = record.pointer("/message/content")?;
    if let Some(text) = content.as_str() {
        return typed_text(text);
    }
    let texts: Vec<String> = content
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .filter_map(typed_text)
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n\n"))
}

fn is_conversation(record: &Value) -> bool {
    matches!(
        record.get("type").and_then(Value::as_str),
        Some("user" | "assistant" | "system" | "attachment")
    ) && record.get("isSidechain").and_then(Value::as_bool) != Some(true)
}

/// Rewinds leave old branches in the file, so follow the newest message back to the start.
fn active_branch(records: &[Value]) -> Vec<&Value> {
    let by_id: HashMap<&str, &Value> = records
        .iter()
        .filter(|record| is_conversation(record))
        .filter_map(|record| Some((record.get("uuid")?.as_str()?, record)))
        .collect();
    let Some(mut current) = records
        .iter()
        .rev()
        .find(|record| is_conversation(record) && record.get("uuid").is_some())
    else {
        return Vec::new();
    };
    let mut branch = Vec::new();
    let mut seen = HashSet::new();
    while let Some(id) = current.get("uuid").and_then(Value::as_str) {
        if !seen.insert(id) {
            break;
        }
        branch.push(current);
        let parent = current
            .get("parentUuid")
            .and_then(Value::as_str)
            .or_else(|| current.get("logicalParentUuid").and_then(Value::as_str));
        match parent.and_then(|parent| by_id.get(parent)) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    branch.reverse();
    branch
}

fn tool_result_events(content: &Value) -> Vec<AgentEvent> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .filter_map(|block| {
            Some(AgentEvent::ToolResult {
                id: block.get("tool_use_id")?.as_str()?.to_owned(),
                is_error: block.get("is_error").and_then(Value::as_bool) == Some(true),
                output: None,
                diff: None,
            })
        })
        .collect()
}

fn assistant_events(content: &Value) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    for block in content.as_array().into_iter().flatten() {
        let text_of = |key: &str| {
            block
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned)
        };
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = text_of("text") {
                    events.push(AgentEvent::TextDelta { text });
                    events.push(AgentEvent::AssistantMessageCompleted {
                        assistant_message_id: String::new(),
                    });
                }
            }
            Some("thinking") => {
                if let Some(text) = text_of("thinking") {
                    events.push(AgentEvent::ReasoningDelta { text });
                }
            }
            Some("tool_use") => {
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    events.push(AgentEvent::ToolCall {
                        id: id.to_owned(),
                        call: decode_tool_use(
                            block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                            block.get("input").unwrap_or(&Value::Null),
                        ),
                    });
                }
            }
            _ => {}
        }
    }
    events
}

fn history_events(records: &[Value]) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    for record in active_branch(records) {
        let content = record.pointer("/message/content").unwrap_or(&Value::Null);
        match record.get("type").and_then(Value::as_str) {
            Some("user") => {
                events.extend(tool_result_events(content));
                if let Some(text) = user_text(record) {
                    events.push(AgentEvent::UserMessage { text });
                }
            }
            Some("assistant") => events.extend(assistant_events(content)),
            _ => {}
        }
    }
    events
}

fn clip_title(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim();
    if line.chars().count() > TITLE_LIMIT {
        format!("{}…", line.chars().take(TITLE_LIMIT).collect::<String>())
    } else {
        line.to_owned()
    }
}

/// Only parses the lines that can hold a title, since chat files can be many megabytes.
fn title_records(path: &Path) -> std::io::Result<Vec<Value>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::with_capacity(1 << 16, file);
    let mut records = Vec::new();
    let mut found_message = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(records);
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        let is_title = ["\"custom-title\"", "\"ai-title\"", "\"summary\""]
            .iter()
            .any(|kind| text.contains(&format!("\"type\":{kind}")));
        let is_message = !found_message && text.contains("\"type\":\"user\"");
        if !is_title && !is_message {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<Value>(text) {
            if is_message && user_text(&record).is_some() {
                found_message = true;
            }
            records.push(record);
        }
    }
}

/// The user's own name wins over the CLI's generated one, which wins over the first message.
fn session_title(records: &[Value]) -> Option<String> {
    let last_string = |kind: &str, key: &str| {
        records
            .iter()
            .rev()
            .filter(|record| record.get("type").and_then(Value::as_str) == Some(kind))
            .find_map(|record| record.get(key).and_then(Value::as_str))
            .filter(|title| !title.trim().is_empty())
            .map(clip_title)
    };
    let first_message = || {
        records
            .iter()
            .filter(|record| {
                record.get("type").and_then(Value::as_str) == Some("user")
                    && record.get("isSidechain").and_then(Value::as_bool) != Some(true)
            })
            .find_map(user_text)
            .map(|text| clip_title(&text))
    };
    let has_message = first_message();
    has_message.as_ref()?;
    last_string("custom-title", "customTitle")
        .or_else(|| last_string("ai-title", "aiTitle"))
        .or_else(|| last_string("summary", "summary"))
        .or(has_message)
}

fn modified_at(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs() as i64)
}

#[derive(Default)]
pub(crate) struct TitleCache(Mutex<HashMap<PathBuf, (SystemTime, u64, Option<String>)>>);

impl TitleCache {
    fn title(&self, path: &Path) -> Option<String> {
        let metadata = std::fs::metadata(path).ok()?;
        let stamp = (metadata.modified().ok()?, metadata.len());
        let mut cache = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((modified, length, title)) = cache.get(path)
            && (*modified, *length) == stamp
        {
            return title.clone();
        }
        let title = title_records(path)
            .ok()
            .and_then(|records| session_title(&records));
        cache.insert(path.to_owned(), (stamp.0, stamp.1, title.clone()));
        title
    }
}

pub(crate) fn list_sessions(
    config_dir: &Path,
    cwd: &Path,
    titles: &TitleCache,
) -> Vec<ExternalSession> {
    let Ok(entries) = std::fs::read_dir(project_dir(config_dir, cwd)) else {
        return Vec::new();
    };
    let mut sessions: Vec<ExternalSession> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_owned();
            if !valid_session_id(&id) {
                return None;
            }
            Some(ExternalSession {
                title: titles.title(&path)?,
                updated_at: modified_at(&path),
                id,
            })
        })
        .collect();
    sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
    sessions
}

pub(crate) fn read_history(
    config_dir: &Path,
    cwd: &Path,
    session_id: &str,
) -> Result<Vec<AgentEvent>, HarnessError> {
    if !valid_session_id(session_id) {
        return Err(HarnessError::Protocol(format!(
            "not a Claude Code chat id: {session_id}"
        )));
    }
    let path = project_dir(config_dir, cwd).join(format!("{session_id}.jsonl"));
    Ok(history_events(&records(&path)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_session(config: &Path, cwd: &Path, id: &str, records: &[Value]) {
        let dir = project_dir(config, cwd);
        std::fs::create_dir_all(&dir).expect("project folder");
        let body: String = records.iter().map(|record| format!("{record}\n")).collect();
        std::fs::write(dir.join(format!("{id}.jsonl")), body).expect("session file");
    }

    fn user(uuid: &str, parent: Option<&str>, content: Value) -> Value {
        json!({"type": "user", "uuid": uuid, "parentUuid": parent, "message": {"role": "user", "content": content}})
    }

    fn assistant(uuid: &str, parent: &str, content: Value) -> Value {
        json!({"type": "assistant", "uuid": uuid, "parentUuid": parent, "message": {"role": "assistant", "content": content}})
    }

    #[test]
    fn project_folders_use_the_cli_naming() {
        assert_eq!(
            project_dir(Path::new("/c"), Path::new("/Volumes/Main/dev/lab/wu")),
            Path::new("/c/projects/-Volumes-Main-dev-lab-wu")
        );
        assert_eq!(
            project_dir(Path::new("/c"), Path::new("/a/my.app_v2")),
            Path::new("/c/projects/-a-my-app-v2")
        );
    }

    #[test]
    fn history_follows_the_newest_branch_across_compaction() {
        let records = vec![
            user("u1", None, json!("Fix the login bug")),
            assistant(
                "a1",
                "u1",
                json!([{"type": "thinking", "thinking": "Look at auth"}, {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}}]),
            ),
            user(
                "r1",
                Some("a1"),
                json!([{"type": "tool_result", "tool_use_id": "t1", "content": "src"}]),
            ),
            assistant(
                "old",
                "r1",
                json!([{"type": "text", "text": "Abandoned branch"}]),
            ),
            json!({"type": "system", "subtype": "compact_boundary", "uuid": "c1", "parentUuid": null, "logicalParentUuid": "r1"}),
            user("s1", Some("c1"), json!("Summary of the earlier chat"))
                .as_object()
                .map(|object| {
                    let mut object = object.clone();
                    object.insert("isCompactSummary".into(), json!(true));
                    Value::Object(object)
                })
                .expect("object"),
            assistant("a2", "s1", json!([{"type": "text", "text": "Fixed it."}])),
            user(
                "u2",
                Some("a2"),
                json!("<command-name>/model</command-name>\n<command-args>sonnet</command-args>"),
            ),
            user(
                "meta",
                Some("u2"),
                json!("<local-command-stdout>Set model</local-command-stdout>"),
            ),
            json!({"type": "user", "uuid": "side", "parentUuid": "meta", "isSidechain": true, "message": {"content": "subagent prompt"}}),
        ];
        let events = history_events(&records);
        let summary: Vec<String> = events
            .iter()
            .map(|event| match event {
                AgentEvent::UserMessage { text } => format!("user:{text}"),
                AgentEvent::TextDelta { text } => format!("text:{text}"),
                AgentEvent::ReasoningDelta { text } => format!("thinking:{text}"),
                AgentEvent::ToolCall { id, .. } => format!("call:{id}"),
                AgentEvent::ToolResult { id, .. } => format!("result:{id}"),
                AgentEvent::AssistantMessageCompleted { .. } => "end".into(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            summary,
            [
                "user:Fix the login bug",
                "thinking:Look at auth",
                "call:t1",
                "result:t1",
                "text:Fixed it.",
                "end",
                "user:/model sonnet",
            ]
        );
    }

    #[test]
    fn sessions_are_listed_newest_first_with_the_best_title() {
        let config = tempfile::tempdir().expect("config folder");
        let cwd = Path::new("/work/app");
        write_session(
            config.path(),
            cwd,
            "11111111-aaaa",
            &[
                user("u1", None, json!("First question\nwith detail")),
                json!({"type": "ai-title", "aiTitle": "Old title"}),
                json!({"type": "ai-title", "aiTitle": "Generated title"}),
            ],
        );
        write_session(
            config.path(),
            cwd,
            "22222222-bbbb",
            &[
                user("u1", None, json!("Rename me")),
                json!({"type": "ai-title", "aiTitle": "Generated title"}),
                json!({"type": "custom-title", "customTitle": "My name"}),
            ],
        );
        write_session(
            config.path(),
            cwd,
            "33333333-cccc",
            &[user("u1", None, json!("Just the first line\nand more"))],
        );
        write_session(
            config.path(),
            cwd,
            "44444444-dddd",
            &[user(
                "u1",
                None,
                json!("<local-command-stdout>only</local-command-stdout>"),
            )],
        );
        write_session(
            config.path(),
            Path::new("/work/other"),
            "55555555-eeee",
            &[user("u1", None, json!("Other project"))],
        );

        let mut sessions = list_sessions(config.path(), cwd, &TitleCache::default());
        sessions.sort_by(|left, right| left.id.cmp(&right.id));
        let titles: Vec<(&str, &str)> = sessions
            .iter()
            .map(|session| (session.id.as_str(), session.title.as_str()))
            .collect();
        assert_eq!(
            titles,
            [
                ("11111111-aaaa", "Generated title"),
                ("22222222-bbbb", "My name"),
                ("33333333-cccc", "Just the first line"),
            ]
        );
        assert!(read_history(config.path(), cwd, "../escape").is_err());
        assert_eq!(
            read_history(config.path(), cwd, "33333333-cccc")
                .expect("history")
                .len(),
            1
        );
    }
}
