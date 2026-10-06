use agent_harness::{
    AgentEvent, CancellationToken, ClaudeHarness, CodexHarness, DoneStatus, Harness, HarnessId,
    Model, OpencodeHarness, PermissionMode, ReasoningLevel, RunControls, RunRequest, SkillRef,
    SlashCommand, SteerMessage, ToolCall, UserInputAnswer, UserInputQuestion, usage::PlanUsage,
};
use anyhow::{Context as _, Result};
use collections::HashMap;
use futures::StreamExt as _;
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Task};
use language::LanguageRegistry;
use markdown::Markdown;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use util::ResultExt as _;

use crate::AgentKind;

const TITLE_MAX_CHARS: usize = 60;
const TOOL_OUTPUT_MAX_CHARS: usize = 200_000;
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const USAGE_FORCED_MIN_INTERVAL: Duration = Duration::from_secs(30);
const CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

fn metadata_path(directory: &Path, id: &str) -> PathBuf {
    directory.join(format!("{id}.meta.json"))
}

fn entries_path(directory: &Path, id: &str) -> PathBuf {
    directory.join(format!("{id}.entries.json"))
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

static FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let _guard = FILE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let directory = path
        .parent()
        .context("session file has no parent directory")?;
    std::fs::create_dir_all(directory)?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, contents)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

pub(crate) fn diff_for_call(call: &ToolCall) -> Option<agent_harness::ToolDiff> {
    match call {
        ToolCall::EditFile {
            path,
            old_string,
            new_string: Some(new_string),
        } => Some(agent_harness::ToolDiff {
            path: path.clone(),
            old_text: Some(old_string.clone().unwrap_or_default()),
            new_text: new_string.clone(),
        }),
        ToolCall::WriteFile {
            path,
            content: Some(content),
        } => Some(agent_harness::ToolDiff {
            path: path.clone(),
            old_text: None,
            new_text: content.clone(),
        }),
        _ => None,
    }
}

fn attachment_paths(attachments: &[PathBuf]) -> Vec<String> {
    attachments
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn title_from(text: &str) -> String {
    let line = agent_harness::view::single_line(text);
    if line.chars().count() <= TITLE_MAX_CHARS {
        return line;
    }
    let mut title: String = line.chars().take(TITLE_MAX_CHARS - 1).collect();
    title.push('…');
    title
}

fn truncate_output(text: String) -> String {
    if text.chars().count() <= TOOL_OUTPUT_MAX_CHARS {
        return text;
    }
    let mut truncated: String = text.chars().take(TOOL_OUTPUT_MAX_CHARS).collect();
    truncated.push('…');
    truncated
}

impl AgentKind {
    pub(crate) fn harness_id(self) -> HarnessId {
        match self {
            AgentKind::Claude => HarnessId::ClaudeCode,
            AgentKind::Codex => HarnessId::Codex,
            AgentKind::Opencode => HarnessId::Opencode,
        }
    }

    fn new_harness(self) -> Arc<dyn Harness> {
        match self {
            AgentKind::Claude => Arc::new(ClaudeHarness::new()),
            AgentKind::Codex => Arc::new(CodexHarness::new()),
            AgentKind::Opencode => Arc::new(OpencodeHarness::new()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunSettings {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning: Option<ReasoningLevel>,
    #[serde(default)]
    pub options: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub permission: PermissionMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub tokens: Option<u64>,
    pub window: Option<u64>,
}

impl ContextSnapshot {
    pub fn fraction(self) -> Option<f64> {
        Some(self.tokens? as f64 / self.window.filter(|window| *window > 0)? as f64)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub kind: AgentKind,
    pub title: String,
    pub cwd: PathBuf,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub native_session_id: Option<String>,
    #[serde(default)]
    pub settings: RunSettings,
    #[serde(default)]
    pub context: Option<ContextSnapshot>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub side_chat: bool,
    #[serde(default)]
    pub fork_context: Option<String>,
    #[serde(default)]
    pub outcome: Option<ChatOutcome>,
    #[serde(default)]
    pub unseen: bool,
    #[serde(default)]
    pub project_root: Option<PathBuf>,
    #[serde(default)]
    pub branch: Option<String>,
}

impl SessionMetadata {
    fn new(kind: AgentKind, cwd: PathBuf, settings: RunSettings) -> Self {
        let settings = RunSettings {
            permission: PermissionMode::default(),
            ..settings
        };
        let created_at = now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            title: String::new(),
            cwd,
            created_at,
            updated_at: created_at,
            native_session_id: None,
            settings,
            context: None,
            pinned: false,
            archived: false,
            section: None,
            parent_id: None,
            side_chat: false,
            fork_context: None,
            outcome: None,
            unseen: false,
            project_root: None,
            branch: None,
        }
    }

    pub fn project_root(&self) -> &Path {
        self.project_root.as_deref().unwrap_or(&self.cwd)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatOutcome {
    Completed,
    Errored,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prompt {
    pub text: String,
    pub attachments: Vec<PathBuf>,
    pub skills: Vec<SkillRef>,
}

impl Prompt {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }

    fn merge(&mut self, other: Prompt) {
        if !other.text.trim().is_empty() {
            if !self.text.trim().is_empty() {
                self.text.push_str("\n\n");
            }
            self.text.push_str(&other.text);
        }
        self.attachments.extend(other.attachments);
        self.skills.extend(other.skills);
    }
}

impl From<String> for Prompt {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for Prompt {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

pub struct QueuedMessage {
    pub id: u64,
    pub prompt: Prompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Running,
    Completed,
    Failed,
    Canceled,
}

pub struct ToolEntry {
    pub id: String,
    pub call: ToolCall,
    pub status: ToolStatus,
    pub output: Option<SharedString>,
    pub diff: Option<agent_harness::ToolDiff>,
}

pub enum Entry {
    User {
        text: SharedString,
        at: i64,
        attachments: Vec<PathBuf>,
        skills: Vec<SkillRef>,
        undelivered: bool,
    },
    Assistant { markdown: Entity<Markdown>, at: i64 },
    Thinking(Entity<Markdown>),
    Tool(ToolEntry),
    Notice { text: SharedString, is_error: bool },
    Image { path: PathBuf },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SerializedEntry {
    User {
        text: String,
        #[serde(default)]
        at: i64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<PathBuf>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skills: Vec<SkillRef>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        undelivered: bool,
    },
    Assistant {
        text: String,
        #[serde(default)]
        at: i64,
    },
    Thinking {
        text: String,
    },
    Tool {
        id: String,
        call: ToolCall,
        status: ToolStatus,
        #[serde(default)]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<agent_harness::ToolDiff>,
    },
    Notice {
        text: String,
        is_error: bool,
    },
    Image {
        path: PathBuf,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SavedTranscript {
    EntriesOnly(Vec<SerializedEntry>),
    Current {
        entries: Vec<SerializedEntry>,
        #[serde(default)]
        subagents: Vec<SavedSubagent>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        queue: Vec<SavedPrompt>,
    },
}

#[derive(Serialize, Deserialize)]
struct SavedPrompt {
    text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skills: Vec<SkillRef>,
}

impl From<SavedPrompt> for Prompt {
    fn from(saved: SavedPrompt) -> Self {
        Self {
            text: saved.text,
            attachments: saved.attachments,
            skills: saved.skills,
        }
    }
}

impl From<&Prompt> for SavedPrompt {
    fn from(prompt: &Prompt) -> Self {
        Self {
            text: prompt.text.clone(),
            attachments: prompt.attachments.clone(),
            skills: prompt.skills.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedSubagent {
    id: String,
    entries: Vec<SerializedEntry>,
    #[serde(default)]
    status: Option<DoneStatus>,
}

#[derive(Default)]
struct Transcript {
    entries: Vec<Entry>,
    text_block_open: bool,
}

impl Transcript {
    fn load(entries: Vec<SerializedEntry>, languages: &Arc<LanguageRegistry>, cx: &mut App) -> Self {
        Self {
            entries: entries
                .into_iter()
                .filter(|entry| {
                    !matches!(entry, SerializedEntry::Thinking { text } if text.trim().is_empty())
                })
                .map(|entry| deserialize_entry(entry, languages, cx))
                .collect(),
            text_block_open: false,
        }
    }

    fn append_text(
        &mut self,
        text: &str,
        thinking: bool,
        languages: &Arc<LanguageRegistry>,
        cx: &mut App,
    ) {
        if text.is_empty() {
            return;
        }
        let extends_last = self.text_block_open
            && matches!(
                (self.entries.last(), thinking),
                (Some(Entry::Assistant { .. }), false) | (Some(Entry::Thinking(_)), true)
            );
        if extends_last {
            match self.entries.last_mut() {
                Some(Entry::Assistant { markdown, at }) => {
                    *at = now();
                    markdown.update(cx, |markdown, cx| markdown.append(text, cx));
                }
                Some(Entry::Thinking(markdown)) => {
                    markdown.update(cx, |markdown, cx| markdown.append(text, cx));
                }
                _ => {}
            }
        } else {
            let markdown = new_markdown(text.to_string(), languages, cx);
            self.entries.push(if thinking {
                Entry::Thinking(markdown)
            } else {
                Entry::Assistant {
                    markdown,
                    at: now(),
                }
            });
            self.text_block_open = true;
        }
    }

    fn push_user(&mut self, text: String, attachments: Vec<PathBuf>, skills: Vec<SkillRef>) {
        self.text_block_open = false;
        self.entries.push(Entry::User {
            text: text.into(),
            at: now(),
            attachments,
            skills,
            undelivered: false,
        });
    }

    fn push_notice(&mut self, text: String, is_error: bool) {
        self.text_block_open = false;
        self.entries.push(Entry::Notice {
            text: text.into(),
            is_error,
        });
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolEntry> {
        self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Tool(tool) if tool.id == id => Some(tool),
            _ => None,
        })
    }

    fn tool(&self, id: &str) -> Option<&ToolEntry> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Tool(tool) if tool.id == id => Some(tool),
            _ => None,
        })
    }

    fn start_tool(&mut self, id: String, call: ToolCall) {
        self.text_block_open = false;
        let diff = diff_for_call(&call);
        if let Some(tool) = self.tool_mut(&id) {
            tool.call = call;
            if diff.is_some() {
                tool.diff = diff;
            }
        } else {
            self.entries.push(Entry::Tool(ToolEntry {
                id,
                call,
                status: ToolStatus::Running,
                output: None,
                diff,
            }));
        }
    }

    fn finish_tool(
        &mut self,
        id: &str,
        is_error: bool,
        output: Option<String>,
        diff: Option<agent_harness::ToolDiff>,
    ) {
        if let Some(tool) = self.tool_mut(id) {
            tool.status = if is_error {
                ToolStatus::Failed
            } else {
                ToolStatus::Completed
            };
            tool.output = output.map(|output| truncate_output(output).into());
            if diff.is_some() {
                tool.diff = diff;
            }
        }
    }

    fn cancel_running_tools(&mut self) {
        for entry in &mut self.entries {
            if let Entry::Tool(tool) = entry
                && tool.status == ToolStatus::Running
            {
                tool.status = ToolStatus::Canceled;
            }
        }
    }

    fn serialize(&self, cx: &App) -> Vec<SerializedEntry> {
        self.entries
            .iter()
            .map(|entry| match entry {
                Entry::User {
                    text,
                    at,
                    attachments,
                    skills,
                    undelivered,
                } => SerializedEntry::User {
                    text: text.to_string(),
                    at: *at,
                    attachments: attachments.clone(),
                    skills: skills.clone(),
                    undelivered: *undelivered,
                },
                Entry::Assistant { markdown, at } => SerializedEntry::Assistant {
                    text: markdown.read(cx).source().to_string(),
                    at: *at,
                },
                Entry::Thinking(markdown) => SerializedEntry::Thinking {
                    text: markdown.read(cx).source().to_string(),
                },
                Entry::Tool(tool) => SerializedEntry::Tool {
                    id: tool.id.clone(),
                    call: tool.call.clone(),
                    status: tool.status,
                    output: tool.output.as_ref().map(|output| output.to_string()),
                    diff: tool.diff.clone(),
                },
                Entry::Notice { text, is_error } => SerializedEntry::Notice {
                    text: text.to_string(),
                    is_error: *is_error,
                },
                Entry::Image { path } => SerializedEntry::Image { path: path.clone() },
            })
            .collect()
    }
}

fn new_markdown(text: String, languages: &Arc<LanguageRegistry>, cx: &mut App) -> Entity<Markdown> {
    let languages = languages.clone();
    cx.new(|cx| Markdown::new(text.into(), Some(languages), None, cx))
}

fn deserialize_entry(
    entry: SerializedEntry,
    languages: &Arc<LanguageRegistry>,
    cx: &mut App,
) -> Entry {
    match entry {
        SerializedEntry::User {
            text,
            at,
            attachments,
            skills,
            undelivered,
        } => Entry::User {
            text: text.into(),
            at,
            attachments,
            skills,
            undelivered,
        },
        SerializedEntry::Assistant { text, at } => Entry::Assistant {
            markdown: new_markdown(text, languages, cx),
            at,
        },
        SerializedEntry::Thinking { text } => Entry::Thinking(new_markdown(text, languages, cx)),
        SerializedEntry::Tool {
            id,
            call,
            status,
            output,
            diff,
        } => Entry::Tool(ToolEntry {
            diff: diff.or_else(|| diff_for_call(&call)),
            id,
            call,
            status: if status == ToolStatus::Running {
                ToolStatus::Canceled
            } else {
                status
            },
            output: output.map(Into::into),
        }),
        SerializedEntry::Notice { text, is_error } => Entry::Notice {
            text: text.into(),
            is_error,
        },
        SerializedEntry::Image { path } => Entry::Image { path },
    }
}

pub struct Subagent {
    transcript: Transcript,
    status: Option<DoneStatus>,
    started_at: Instant,
}

impl Subagent {
    fn new() -> Self {
        Self {
            transcript: Transcript::default(),
            status: None,
            started_at: Instant::now(),
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.transcript.entries
    }

    pub fn status(&self) -> Option<DoneStatus> {
        self.status
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }
}

#[derive(Clone)]
pub struct BackgroundTask {
    pub task_id: String,
    pub tool_use_id: Option<String>,
    pub kind: agent_harness::BackgroundTaskKind,
    pub description: String,
    pub started_at: Instant,
}

pub struct PendingQuestion {
    pub questions: Vec<UserInputQuestion>,
    responder: Option<tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>>,
}

impl PendingQuestion {
    pub fn id(&self) -> &str {
        self.questions
            .first()
            .map(|question| question.id.as_str())
            .unwrap_or_default()
    }

    pub fn is_permission(&self) -> bool {
        !self.questions.is_empty()
            && self
                .questions
                .iter()
            .all(|question| question.header == agent_harness::claude::PERMISSION_HEADER)
    }
}

enum RunInput {
    Event(Result<AgentEvent, agent_harness::HarnessError>),
    Question {
        questions: Vec<UserInputQuestion>,
        responder: tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>,
    },
    Ended,
}

struct ActiveRun {
    steering: tokio::sync::mpsc::Sender<SteerMessage>,
    interrupt: CancellationToken,
    _events: Task<()>,
}

pub enum SessionEvent {
    MetadataChanged,
    SettingsChanged,
    Finished { errored: bool },
    NeedsInput,
}

pub struct AgentSession {
    directory: Arc<Path>,
    working_since: Option<Instant>,
    run_failed: bool,
    harness: Arc<dyn Harness>,
    restart_on_next_send: bool,
    metadata: SessionMetadata,
    transcript: Transcript,
    subagents: HashMap<String, Subagent>,
    working: bool,
    compacting: bool,
    background_tasks: Vec<BackgroundTask>,
    known_tasks: HashMap<String, BackgroundTask>,
    run: Option<ActiveRun>,
    prompt_after_stop: Option<Prompt>,
    user_stopped: bool,
    queue: Vec<QueuedMessage>,
    next_queue_id: u64,
    editing_queued: Option<u64>,
    deleted: bool,
    pending_questions: Vec<PendingQuestion>,
    noticed_auto_unavailable: bool,
    languages: Arc<LanguageRegistry>,
    save_metadata_task: Task<()>,
    save_entries_task: Task<()>,
}

impl EventEmitter<SessionEvent> for AgentSession {}

impl AgentSession {
    fn new(
        directory: Arc<Path>,
        harness: Arc<dyn Harness>,
        metadata: SessionMetadata,
        saved: SavedTranscript,
        languages: Arc<LanguageRegistry>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (entries, saved_subagents, saved_queue) = match saved {
            SavedTranscript::Current {
                entries,
                subagents,
                queue,
            } => (entries, subagents, queue),
            SavedTranscript::EntriesOnly(entries) => (entries, Vec::new(), Vec::new()),
        };
        let transcript = Transcript::load(entries, &languages, cx);
        let subagents = saved_subagents
            .into_iter()
            .map(|saved| {
                let subagent = Subagent {
                    transcript: Transcript::load(saved.entries, &languages, cx),
                    status: Some(saved.status.unwrap_or(DoneStatus::Interrupted)),
                    started_at: Instant::now(),
                };
                (saved.id, subagent)
            })
            .collect();
        Self {
            directory,
            working_since: None,
            run_failed: false,
            harness,
            restart_on_next_send: false,
            metadata,
            transcript,
            subagents,
            working: false,
            compacting: false,
            background_tasks: Vec::new(),
            known_tasks: HashMap::default(),
            run: None,
            prompt_after_stop: None,
            user_stopped: false,
            queue: saved_queue
                .into_iter()
                .enumerate()
                .map(|(index, prompt)| QueuedMessage {
                    id: index as u64,
                    prompt: prompt.into(),
                })
                .collect(),
            next_queue_id: u64::MAX / 2,
            editing_queued: None,
            deleted: false,
            pending_questions: Vec::new(),
            noticed_auto_unavailable: false,
            languages,
            save_metadata_task: Task::ready(()),
            save_entries_task: Task::ready(()),
        }
    }

    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    pub fn kind(&self) -> AgentKind {
        self.metadata.kind
    }

    pub fn entries(&self) -> &[Entry] {
        &self.transcript.entries
    }

    pub fn subagent(&self, id: &str) -> Option<&Subagent> {
        self.subagents.get(id)
    }

    pub fn subagent_running(&self, id: &str) -> bool {
        let Some(subagent) = self.subagents.get(id) else {
            return false;
        };
        let spawn_running = std::iter::once(&self.transcript)
            .chain(self.subagents.values().map(|subagent| &subagent.transcript))
            .find_map(|transcript| transcript.tool(id))
            .is_some_and(|tool| tool.status == ToolStatus::Running);
        subagent.status.is_none() && self.run.is_some() && (spawn_running || self.working)
    }

    pub fn mark_seen(&mut self, cx: &mut Context<Self>) {
        if self.metadata.unseen {
            self.metadata.unseen = false;
            self.save_metadata(cx);
            cx.dismiss_system_notification(&self.metadata.id);
        }
    }

    pub fn update_metadata(&mut self, change: impl FnOnce(&mut SessionMetadata), cx: &mut Context<Self>) {
        change(&mut self.metadata);
        self.save_metadata(cx);
        cx.notify();
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted
    }

    pub fn is_working(&self) -> bool {
        self.working
    }

    pub fn background_tasks(&self) -> &[BackgroundTask] {
        &self.background_tasks
    }

    pub fn tool_entry(&self, id: &str) -> Option<&ToolEntry> {
        std::iter::once(&self.transcript)
            .chain(self.subagents.values().map(|subagent| &subagent.transcript))
            .find_map(|transcript| transcript.tool(id))
    }

    pub fn is_compacting(&self) -> bool {
        self.working && self.compacting
    }

    pub fn can_steer(&self, prompt: &Prompt) -> bool {
        // Claude reads a slash command sent into a running turn as plain text.
        self.working
            && !(self.metadata.kind == AgentKind::Claude
                && agent_harness::leading_command(&prompt.text).is_some())
    }

    pub fn working_since(&self) -> Option<Instant> {
        self.working.then_some(self.working_since).flatten()
    }

    pub fn run_failed(&self) -> bool {
        self.run_failed
    }

    pub fn pending_questions(&self) -> &[PendingQuestion] {
        &self.pending_questions
    }

    pub fn settings(&self) -> &RunSettings {
        &self.metadata.settings
    }

    pub fn context(&self) -> Option<ContextSnapshot> {
        self.metadata.context
    }

    /// Model and effort are launch flags: a change takes effect by resuming in a new process.
    pub fn update_settings(
        &mut self,
        change: impl FnOnce(&mut RunSettings),
        cx: &mut Context<Self>,
    ) {
        if self.change_settings(change, cx) {
            cx.emit(SessionEvent::SettingsChanged);
        }
    }

    /// Like `update_settings`, but not remembered as the default for new chats.
    pub fn pin_settings(&mut self, change: impl FnOnce(&mut RunSettings), cx: &mut Context<Self>) {
        let restart_on_next_send = self.restart_on_next_send;
        self.change_settings(change, cx);
        self.restart_on_next_send = restart_on_next_send;
    }

    fn change_settings(
        &mut self,
        change: impl FnOnce(&mut RunSettings),
        cx: &mut Context<Self>,
    ) -> bool {
        let before = self.metadata.settings.clone();
        change(&mut self.metadata.settings);
        if self.metadata.settings == before {
            return false;
        }
        self.restart_on_next_send = self.run.is_some();
        self.save_metadata(cx);
        cx.notify();
        true
    }

    pub fn queue(&self) -> &[QueuedMessage] {
        &self.queue
    }

    pub fn send_message(&mut self, prompt: impl Into<Prompt>, cx: &mut Context<Self>) {
        let mut prompt = prompt.into();
        prompt.text = prompt.text.trim().to_string();
        if prompt.is_empty() || self.deleted {
            return;
        }
        let stopping = self
            .run
            .as_ref()
            .is_some_and(|run| run.interrupt.is_cancelled());
        if self.working && self.run.is_some() && !stopping {
            self.enqueue(prompt, cx);
            return;
        }
        if !stopping && self.has_sendable_queued() {
            self.enqueue(prompt, cx);
            if let Some(next) = self.take_next_queued() {
                self.deliver(next.prompt, cx);
            }
            return;
        }
        self.deliver(prompt, cx);
    }

    fn has_sendable_queued(&self) -> bool {
        self.queue
            .iter()
            .any(|queued| Some(queued.id) != self.editing_queued)
    }

    fn take_next_queued(&mut self) -> Option<QueuedMessage> {
        let index = self
            .queue
            .iter()
            .position(|queued| Some(queued.id) != self.editing_queued)?;
        Some(self.queue.remove(index))
    }

    fn enqueue(&mut self, prompt: Prompt, cx: &mut Context<Self>) {
        self.next_queue_id += 1;
        self.queue.push(QueuedMessage {
            id: self.next_queue_id,
            prompt,
        });
        self.save_entries(cx);
        cx.notify();
    }

    fn take_queued(&mut self, id: u64) -> Option<(usize, Prompt)> {
        let index = self.queue.iter().position(|queued| queued.id == id)?;
        if self.editing_queued == Some(id) {
            self.editing_queued = None;
        }
        Some((index, self.queue.remove(index).prompt))
    }

    pub fn steer_queued(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some((_, prompt)) = self.take_queued(id) {
            self.deliver(prompt, cx);
        }
    }

    pub fn send_queued_now(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some((_, prompt)) = self.take_queued(id) else {
            return;
        };
        if self.working
            && let Some(run) = &self.run
        {
            run.interrupt.cancel();
        }
        self.deliver(prompt, cx);
    }

    pub fn remove_queued(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.take_queued(id).is_some() {
            self.save_entries(cx);
            cx.notify();
        }
    }

    pub fn move_queued(&mut self, id: u64, to_index: usize, cx: &mut Context<Self>) {
        let Some(from) = self.queue.iter().position(|queued| queued.id == id) else {
            return;
        };
        let queued = self.queue.remove(from);
        let to_index = to_index.min(self.queue.len());
        self.queue.insert(to_index, queued);
        self.save_entries(cx);
        cx.notify();
    }

    pub fn editing_queued(&self) -> Option<u64> {
        self.editing_queued
    }

    pub fn begin_queued_edit(&mut self, id: u64, cx: &mut Context<Self>) -> Option<Prompt> {
        let queued = self.queue.iter().find(|queued| queued.id == id)?;
        let prompt = queued.prompt.clone();
        self.editing_queued = Some(id);
        cx.notify();
        Some(prompt)
    }

    pub fn finish_queued_edit(
        &mut self,
        id: u64,
        edited: Option<Prompt>,
        cx: &mut Context<Self>,
    ) {
        if self.editing_queued == Some(id) {
            self.editing_queued = None;
        }
        if let Some(prompt) = edited.filter(|prompt| !prompt.is_empty()) {
            match self.queue.iter_mut().find(|queued| queued.id == id) {
                Some(queued) => queued.prompt = prompt,
                None => self.enqueue(prompt, cx),
            }
            self.save_entries(cx);
        }
        cx.notify();
    }

    pub fn retry_undelivered(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(Entry::User {
            text,
            attachments,
            skills,
            undelivered: true,
            ..
        }) = self.transcript.entries.get(index)
        else {
            return;
        };
        let prompt = Prompt {
            text: text.to_string(),
            attachments: attachments.clone(),
            skills: skills.clone(),
        };
        let notices = self.transcript.entries[index + 1..]
            .iter()
            .take_while(|entry| matches!(entry, Entry::Notice { is_error: true, .. }))
            .count();
        self.transcript.entries.drain(index..=index + notices);
        self.send_message(prompt, cx);
    }

    fn deliver(&mut self, prompt: Prompt, cx: &mut Context<Self>) {
        let stopping = self
            .run
            .as_ref()
            .is_some_and(|run| run.interrupt.is_cancelled());
        let is_command = |prompt: &Prompt| agent_harness::leading_command(&prompt.text).is_some();
        let steering_command = self.run.is_some() && !stopping && !self.can_steer(&prompt);
        // Merging would turn the other message into the command's arguments.
        let merging_command = stopping
            && self
                .prompt_after_stop
                .as_ref()
                .is_some_and(|pending| is_command(pending) || is_command(&prompt));
        if self.working && (steering_command || merging_command) {
            self.next_queue_id += 1;
            self.queue.insert(
                0,
                QueuedMessage {
                    id: self.next_queue_id,
                    prompt,
                },
            );
            self.save_entries(cx);
            cx.notify();
            return;
        }
        self.user_stopped = false;
        if self.restart_on_next_send && !self.working {
            self.run = None;
            self.restart_on_next_send = false;
        }
        if self.metadata.title.is_empty() {
            self.metadata.title = title_from(&prompt.text);
        }
        self.transcript.push_user(
            prompt.text.clone(),
            prompt.attachments.clone(),
            prompt.skills.clone(),
        );
        if !self.working {
            self.working_since = Some(Instant::now());
        }
        self.working = true;
        self.run_failed = false;
        self.metadata.outcome = None;
        self.touch(cx);

        if let Some(run) = &self.run {
            if run.interrupt.is_cancelled() {
                match &mut self.prompt_after_stop {
                    Some(pending) => pending.merge(prompt),
                    None => self.prompt_after_stop = Some(prompt),
                }
                cx.notify();
                return;
            }
            let steer = SteerMessage {
                prompt: prompt.text.clone(),
                message_id: None,
                attachments: attachment_paths(&prompt.attachments),
                skills: prompt.skills.clone(),
            };
            if run.steering.try_send(steer).is_ok() {
                cx.notify();
                return;
            }
            self.run = None;
        }
        self.start_run(prompt, cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.prompt_after_stop = None;
        self.user_stopped = true;
        match &self.run {
            Some(run) => run.interrupt.cancel(),
            None => {
                self.working = false;
                cx.notify();
            }
        }
    }

    pub fn answer(&mut self, id: &str, answers: Vec<UserInputAnswer>, cx: &mut Context<Self>) {
        let Some(index) = self
            .pending_questions
            .iter()
            .position(|pending| pending.id() == id)
        else {
            return;
        };
        let mut pending = self.pending_questions.remove(index);
        cx.dismiss_system_notification(&self.metadata.id);
        if let Some(responder) = pending.responder.take()
            && responder.send(answers).is_err()
        {
            log::debug!("agent stopped waiting for an answer");
        }
        cx.notify();
    }

    pub fn answer_permission(&mut self, id: &str, allow: bool, cx: &mut Context<Self>) {
        let Some(pending) = self
            .pending_questions
            .iter()
            .find(|pending| pending.id() == id)
        else {
            return;
        };
        let label = if allow {
            agent_harness::claude::PERMISSION_ALLOW
        } else {
            agent_harness::claude::PERMISSION_DENY
        };
        let answers = pending
            .questions
            .iter()
            .map(|question| UserInputAnswer {
                question_id: question.id.clone(),
                labels: vec![label.to_string()],
            })
            .collect();
        self.answer(id, answers, cx);
    }

    fn start_run(&mut self, prompt: Prompt, cx: &mut Context<Self>) {
        self.restart_on_next_send = false;
        let harness = self.harness.clone();
        if self.metadata.native_session_id.is_none() {
            self.metadata.context = None;
        }
        let text = match self.metadata.fork_context.take() {
            Some(context) if agent_harness::leading_command(&prompt.text).is_none() => {
                self.save_metadata(cx);
                format!(
                    "Continue this conversation using the following prior conversation as context.\n<conversation>\n{context}\n</conversation>\n\n{}",
                    prompt.text
                )
            }
            context => {
                self.metadata.fork_context = context;
                prompt.text.clone()
            }
        };
        let settings = self.metadata.settings.clone();
        let request = RunRequest {
            prompt: text,
            attachments: attachment_paths(&prompt.attachments),
            skills: prompt.skills.clone(),
            model: settings.model,
            reasoning: settings.reasoning,
            model_options: settings.options,
            cwd: self.metadata.cwd.to_string_lossy().into_owned(),
            permission: settings.permission,
            resume: self.metadata.native_session_id.clone(),
        };
        let (steering_tx, steering_rx) = tokio::sync::mpsc::channel(32);
        let interrupt = CancellationToken::new();
        let (question_tx, question_rx) = futures::channel::mpsc::unbounded::<(
            Vec<UserInputQuestion>,
            tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>,
        )>();
        let controls = RunControls {
            request_input: Box::new(move |questions| {
                let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
                if question_tx.unbounded_send((questions, answer_tx)).is_err() {
                    log::debug!("agent asked a question after its chat closed");
                }
                answer_rx
            }),
            steering: steering_rx,
            interrupt: interrupt.clone(),
        };

        let run = spawn_agent_work(cx, async move { harness.run(request, controls).await });
        let events = cx.spawn(async move |this, cx| {
            let stream = match run.await {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| this.fail_run(error.to_string(), cx))
                        .log_err();
                    return;
                }
                Err(error) => {
                    this.update(cx, |this, cx| this.fail_run(error.to_string(), cx))
                        .log_err();
                    return;
                }
            };
            let events = stream
                .map(RunInput::Event)
                .chain(futures::stream::iter([RunInput::Ended]));
            let questions = question_rx.map(|(questions, responder)| RunInput::Question {
                questions,
                responder,
            });
            let mut inputs = futures::stream::select(events, questions);
            while let Some(input) = inputs.next().await {
                let update = match input {
                    RunInput::Event(Ok(event)) => {
                        this.update(cx, |this, cx| this.apply_event(event, cx))
                    }
                    RunInput::Event(Err(error)) => {
                        this.update(cx, |this, cx| this.push_notice(error.to_string(), true, cx))
                    }
                    RunInput::Question {
                        questions,
                        responder,
                    } => this.update(cx, |this, cx| this.push_question(questions, responder, cx)),
                    RunInput::Ended => break,
                };
                if update.is_err() {
                    return;
                }
            }
            this.update(cx, |this, cx| this.run_ended(cx)).log_err();
        });
        self.run = Some(ActiveRun {
            steering: steering_tx,
            interrupt,
            _events: events,
        });
        cx.notify();
    }

    fn fail_run(&mut self, message: String, cx: &mut Context<Self>) {
        self.run_failed = true;
        if let Some(Entry::User { undelivered, .. }) = self.transcript.entries.last_mut() {
            *undelivered = true;
        }
        self.push_notice(message, true, cx);
        self.run_ended(cx);
    }

    fn run_ended(&mut self, cx: &mut Context<Self>) {
        self.run = None;
        self.background_tasks.clear();
        self.known_tasks.clear();
        self.pending_questions.clear();
        self.cancel_running_tools();
        for subagent in self.subagents.values_mut() {
            subagent.transcript.cancel_running_tools();
            subagent.status.get_or_insert(DoneStatus::Interrupted);
        }
        self.save_entries(cx);
        match self.prompt_after_stop.take() {
            Some(prompt) if !self.deleted => {
                self.working = true;
                self.working_since = Some(Instant::now());
                self.start_run(prompt, cx);
            }
            _ => {
                self.working = false;
                if !self.user_stopped {
                    self.send_next_queued(cx);
                }
            }
        }
        cx.notify();
    }

    fn send_next_queued(&mut self, cx: &mut Context<Self>) {
        if self.working || self.deleted {
            return;
        }
        if let Some(next) = self.take_next_queued() {
            self.deliver(next.prompt, cx);
        }
    }

    fn close_for_deletion(&mut self, cx: &mut Context<Self>) -> Vec<Task<()>> {
        self.deleted = true;
        self.run = None;
        self.prompt_after_stop = None;
        self.pending_questions.clear();
        self.working = false;
        cx.notify();
        vec![
            std::mem::replace(&mut self.save_metadata_task, Task::ready(())),
            std::mem::replace(&mut self.save_entries_task, Task::ready(())),
        ]
    }

    fn push_question(
        &mut self,
        questions: Vec<UserInputQuestion>,
        responder: tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>,
        cx: &mut Context<Self>,
    ) {
        if questions.is_empty() {
            if responder.send(Vec::new()).is_err() {
                log::debug!("agent stopped waiting for an answer");
            }
            return;
        }
        let pending = PendingQuestion {
            questions,
            responder: Some(responder),
        };
        self.pending_questions.push(pending);
        cx.emit(SessionEvent::NeedsInput);
        cx.notify();
    }

    fn apply_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::SessionStarted { session_id, .. } => {
                if !session_id.is_empty() {
                    self.set_native_session_id(session_id, cx);
                }
            }
            AgentEvent::PermissionModeReported { mode } => {
                let asked_for_auto = self.metadata.kind == AgentKind::Claude
                    && self.metadata.settings.permission == PermissionMode::Auto;
                if asked_for_auto && mode != "auto" && !self.noticed_auto_unavailable {
                    self.noticed_auto_unavailable = true;
                    self.push_notice(
                        "Auto mode isn't available here, so Claude will ask before acting. \
                         It's off with fast mode, and on some plans and models."
                            .into(),
                        false,
                        cx,
                    );
                }
            }
            AgentEvent::TextDelta { text } => {
                self.resume_working();
                self.append_text(&text, false, cx)
            }
            AgentEvent::ReasoningDelta { text } => {
                self.resume_working();
                self.append_text(&text, true, cx)
            }
            AgentEvent::AssistantMessageCompleted { .. } => self.transcript.text_block_open = false,
            AgentEvent::Steered { .. } => {
                self.transcript.text_block_open = false;
                self.resume_working();
            }
            AgentEvent::ToolCall { id, call } => {
                self.resume_working();
                if call.is_subagent_spawn() {
                    self.subagents.entry(id.clone()).or_insert_with(Subagent::new);
                }
                self.transcript.start_tool(id, call);
                self.save_entries(cx);
            }
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                diff,
            } => {
                self.transcript.finish_tool(&id, is_error, output, diff);
                self.save_entries(cx);
            }
            AgentEvent::Error { message } => self.push_notice(message, true, cx),
            AgentEvent::Done {
                status,
                error,
                session_id,
                ..
            } => {
                if let Some(session_id) = session_id.filter(|id| !id.is_empty()) {
                    self.set_native_session_id(session_id, cx);
                }
                self.working = self.prompt_after_stop.is_some();
                self.compacting = false;
                self.transcript.text_block_open = false;
                self.cancel_running_tools();
                match status {
                    DoneStatus::Completed => {
                        self.metadata.outcome = Some(ChatOutcome::Completed);
                        self.metadata.unseen = true;
                        cx.emit(SessionEvent::Finished { errored: false });
                    }
                    DoneStatus::Interrupted => {}
                    DoneStatus::Errored => {
                        self.run_failed = true;
                        self.metadata.outcome = Some(ChatOutcome::Errored);
                        self.metadata.unseen = true;
                        cx.emit(SessionEvent::Finished { errored: true });
                        self.push_notice(
                            error.unwrap_or_else(|| "The agent stopped with an error.".into()),
                            true,
                            cx,
                        )
                    }
                }
                self.touch(cx);
                if status != DoneStatus::Interrupted {
                    self.send_next_queued(cx);
                }
            }
            AgentEvent::GeneratedImage { path, .. } => {
                self.transcript.text_block_open = false;
                self.transcript.entries.push(Entry::Image { path: path.into() });
                self.save_entries(cx);
            }
            AgentEvent::Compacting { active } => {
                if active {
                    self.resume_working();
                }
                self.compacting = active;
            }
            AgentEvent::TaskStarted {
                task_id,
                tool_use_id,
                kind,
                description,
            } => {
                let started_at = self
                    .known_tasks
                    .get(&task_id)
                    .map_or_else(Instant::now, |task| task.started_at);
                self.known_tasks.insert(
                    task_id.clone(),
                    BackgroundTask {
                        task_id,
                        tool_use_id,
                        kind,
                        description,
                        started_at,
                    },
                );
            }
            AgentEvent::TaskFinished { task_id, .. } => {
                self.known_tasks.remove(&task_id);
                self.background_tasks.retain(|task| task.task_id != task_id);
            }
            AgentEvent::BackgroundTasksChanged { tasks } => {
                let previous = std::mem::take(&mut self.background_tasks);
                self.background_tasks = tasks
                    .into_iter()
                    .map(|info| {
                        let known = self
                            .known_tasks
                            .get(&info.task_id)
                            .or_else(|| previous.iter().find(|task| task.task_id == info.task_id));
                        BackgroundTask {
                            tool_use_id: known.and_then(|task| task.tool_use_id.clone()),
                            started_at: known.map_or_else(Instant::now, |task| task.started_at),
                            description: if info.description.trim().is_empty() {
                                known.map(|task| task.description.clone()).unwrap_or_default()
                            } else {
                                info.description
                            },
                            kind: info.kind,
                            task_id: info.task_id,
                        }
                    })
                    .collect();
            }
            AgentEvent::Compacted { tokens, manual } => {
                self.compacting = false;
                // After /compact the Claude process keeps the old history in memory; a resume loads the new one.
                if manual && self.metadata.kind == AgentKind::Claude {
                    self.restart_on_next_send = true;
                }
                let mut context = self.metadata.context.unwrap_or_default();
                if context.tokens != tokens {
                    context.tokens = tokens;
                    self.metadata.context = Some(context);
                    self.save_metadata(cx);
                }
                self.push_notice("Context compacted.".into(), false, cx);
            }
            AgentEvent::ContextUsage { tokens, window } => {
                let mut context = self.metadata.context.unwrap_or_default();
                if tokens.is_some() {
                    context.tokens = tokens;
                }
                if let Some(window) = window.filter(|window| *window > 0) {
                    context.window = Some(window);
                }
                if self.metadata.context != Some(context) {
                    self.metadata.context = Some(context);
                    self.save_metadata(cx);
                }
            }
            AgentEvent::Usage { .. }
            | AgentEvent::AvailableCommands { .. }
            | AgentEvent::InputRequested { .. }
            | AgentEvent::InputResolved { .. }
            | AgentEvent::UserMessage { .. } => {}
            AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            } => self.apply_subagent_event(parent_tool_use_id, *event, cx),
        }
        cx.notify();
    }

    /// The agent can start a turn on its own after a background command or subagent finishes.
    fn resume_working(&mut self) {
        if !self.working {
            self.working = true;
            self.working_since = Some(Instant::now());
        }
    }

    fn apply_subagent_event(&mut self, parent: String, event: AgentEvent, cx: &mut Context<Self>) {
        let languages = self.languages.clone();
        let subagent = self.subagents.entry(parent).or_insert_with(Subagent::new);
        let transcript = &mut subagent.transcript;
        match event {
            AgentEvent::UserMessage { text } => transcript.push_user(text, Vec::new(), Vec::new()),
            AgentEvent::TextDelta { text } => transcript.append_text(&text, false, &languages, cx),
            AgentEvent::ReasoningDelta { text } => {
                transcript.append_text(&text, true, &languages, cx)
            }
            AgentEvent::AssistantMessageCompleted { .. } | AgentEvent::Steered { .. } => {
                transcript.text_block_open = false
            }
            AgentEvent::ToolCall { id, call } => transcript.start_tool(id, call),
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                diff,
            } => transcript.finish_tool(&id, is_error, output, diff),
            AgentEvent::Error { message } => transcript.push_notice(message, true),
            AgentEvent::Done { status, error, .. } => {
                transcript.cancel_running_tools();
                transcript.text_block_open = false;
                if let Some(error) = error.filter(|_| status == DoneStatus::Errored) {
                    transcript.push_notice(error, true);
                }
                subagent.status = Some(status);
            }
            AgentEvent::Subagent {
                parent_tool_use_id,
                event,
            } => return self.apply_subagent_event(parent_tool_use_id, *event, cx),
            _ => return,
        }
        self.save_entries(cx);
    }

    fn append_text(&mut self, text: &str, thinking: bool, cx: &mut Context<Self>) {
        let languages = self.languages.clone();
        self.transcript.append_text(text, thinking, &languages, cx);
        self.save_entries(cx);
    }

    fn push_notice(&mut self, text: String, is_error: bool, cx: &mut Context<Self>) {
        self.transcript.push_notice(text, is_error);
        self.save_entries(cx);
        cx.notify();
    }

    fn cancel_running_tools(&mut self) {
        self.transcript.cancel_running_tools();
    }

    fn set_native_session_id(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.metadata.native_session_id.as_deref() != Some(session_id.as_str()) {
            self.metadata.native_session_id = Some(session_id);
            self.save_metadata(cx);
        }
    }

    fn touch(&mut self, cx: &mut Context<Self>) {
        self.metadata.updated_at = now();
        self.save_metadata(cx);
        self.save_entries(cx);
    }

    fn saved_transcript(&self, cx: &App) -> SavedTranscript {
        let mut subagents: Vec<SavedSubagent> = self
            .subagents
            .iter()
            .map(|(id, subagent)| SavedSubagent {
                id: id.clone(),
                entries: subagent.transcript.serialize(cx),
                status: subagent.status,
            })
            .collect();
        subagents.sort_by(|left, right| left.id.cmp(&right.id));
        SavedTranscript::Current {
            entries: self.transcript.serialize(cx),
            subagents,
            queue: self.queue.iter().map(|queued| (&queued.prompt).into()).collect(),
        }
    }

    fn save_metadata(&mut self, cx: &mut Context<Self>) {
        if self.deleted {
            return;
        }
        cx.emit(SessionEvent::MetadataChanged);
        let metadata = self.metadata.clone();
        let directory = self.directory.clone();
        let previous_save = std::mem::replace(&mut self.save_metadata_task, Task::ready(()));
        self.save_metadata_task = cx.background_spawn(async move {
            previous_save.await;
            let path = metadata_path(&directory, &metadata.id);
            serde_json::to_vec_pretty(&metadata)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
    }

    fn save_entries(&mut self, cx: &mut Context<Self>) {
        if self.deleted {
            return;
        }
        let directory = self.directory.clone();
        self.save_entries_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let Some((id, entries)) = this
                .read_with(cx, |this, cx| {
                    (!this.deleted).then(|| (this.metadata.id.clone(), this.saved_transcript(cx)))
                })
                .log_err()
                .flatten()
            else {
                return;
            };
            cx.background_spawn(async move {
                serde_json::to_vec(&entries)
                    .map_err(anyhow::Error::from)
                    .and_then(|json| write_atomically(&entries_path(&directory, &id), &json))
                    .log_err();
            })
            .await;
        });
    }
}

#[derive(Clone)]
pub struct SessionSummary {
    pub id: String,
    pub kind: AgentKind,
    pub title: SharedString,
    pub created_at: i64,
    pub updated_at: i64,
    pub working: bool,
    pub needs_input: bool,
    pub pinned: bool,
    pub archived: bool,
    pub section: Option<String>,
    pub parent_id: Option<String>,
    pub side_chat: bool,
    pub outcome: Option<ChatOutcome>,
    pub unseen: bool,
    pub project_root: PathBuf,
    pub branch: Option<String>,
    pub native_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSection {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatSort {
    #[default]
    Updated,
    Created,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatListPrefs {
    pub sections: Vec<ChatSection>,
    pub all_projects: bool,
    pub group_by_project: bool,
    pub sort: ChatSort,
    pub compact_rows: bool,
    pub collapsed_sections: Vec<String>,
    pub favorite_models: Vec<String>,
    pub dictation_device: Option<String>,
}

impl ChatListPrefs {
    pub fn favorite_key(kind: AgentKind, model_id: &str) -> String {
        format!("{}:{model_id}", kind.label())
    }

    pub fn is_favorite(&self, kind: AgentKind, model_id: &str) -> bool {
        self.favorite_models
            .contains(&Self::favorite_key(kind, model_id))
    }
}

pub enum StoreEvent {
    SessionFinished { id: String, errored: bool },
    SessionNeedsInput { id: String },
}

fn chat_list_path(directory: &Path) -> PathBuf {
    directory.join("chat_list.json")
}

fn fork_context(entries: &[SerializedEntry]) -> Option<String> {
    let history: Vec<serde_json::Value> = entries
        .iter()
        .filter_map(|entry| match entry {
            SerializedEntry::User { text, .. } => Some(("user", text.clone())),
            SerializedEntry::Assistant { text, .. } => Some(("assistant", text.clone())),
            SerializedEntry::Tool { call, output, .. } => Some((
                "assistant",
                format!(
                    "Tool: {}\n{}",
                    serde_json::to_string(call).unwrap_or_default(),
                    output.clone().unwrap_or_default()
                ),
            )),
            SerializedEntry::Thinking { .. }
            | SerializedEntry::Notice { .. }
            | SerializedEntry::Image { .. } => None,
        })
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(role, text)| serde_json::json!({ "role": role, "text": text }))
        .collect();
    (!history.is_empty()).then(|| serde_json::Value::Array(history).to_string())
}

pub(crate) struct ShellEnvLoaded(
    pub futures::future::Shared<futures::channel::oneshot::Receiver<()>>,
);

impl Global for ShellEnvLoaded {}

fn shell_env_loaded(cx: &App) -> impl std::future::Future<Output = ()> + Send + 'static {
    let loaded = cx.try_global::<ShellEnvLoaded>().map(|loaded| loaded.0.clone());
    async move {
        if let Some(loaded) = loaded {
            loaded.await.ok();
        }
    }
}

/// Agent CLIs and their config folders come from the login shell's environment, which loads after startup.
fn spawn_agent_work<T: 'static, F, R>(
    cx: &Context<T>,
    work: F,
) -> Task<std::result::Result<R, gpui_tokio::JoinError>>
where
    F: std::future::Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let env_loaded = shell_env_loaded(cx);
    gpui_tokio::Tokio::spawn(cx, async move {
        env_loaded.await;
        work.await
    })
}

fn shell_env_is_loaded(cx: &App) -> bool {
    cx.try_global::<ShellEnvLoaded>()
        .is_none_or(|loaded| loaded.0.peek().is_some())
}

struct GlobalAgentStore(Entity<AgentStore>);

impl Global for GlobalAgentStore {}

impl EventEmitter<StoreEvent> for AgentStore {}

/// Owns every live chat, so an agent keeps running after its tab is closed.
pub struct AgentStore {
    directory: Arc<Path>,
    sessions: Vec<SessionMetadata>,
    live: HashMap<String, Entity<AgentSession>>,
    languages: Arc<LanguageRegistry>,
    harnesses: HashMap<AgentKind, Arc<dyn Harness>>,
    catalogs: HashMap<AgentKind, ModelCatalog>,
    defaults: HashMap<AgentKind, RunSettings>,
    plan_usage: HashMap<AgentKind, PlanUsageState>,
    commands: HashMap<(AgentKind, PathBuf), CommandCatalog>,
    skills: HashMap<(AgentKind, PathBuf), SkillCatalog>,
    accounts: HashMap<AgentKind, AccountsState>,
    list_prefs: ChatListPrefs,
    save_list_prefs_task: Task<()>,
    metadata_writes: HashMap<String, Task<()>>,
    save_defaults_task: Task<()>,
    _load: Task<()>,
}

#[derive(Default)]
struct ModelCatalog {
    models: Vec<Model>,
    fallback: Vec<Model>,
    loaded: bool,
    _task: Option<Task<()>>,
}

#[derive(Default)]
pub struct AccountsState {
    pub accounts: Vec<agent_harness::accounts::Account>,
    pub usage: HashMap<String, std::result::Result<PlanUsage, String>>,
    pub error: Option<SharedString>,
    pub loading: bool,
    pub switching: Option<String>,
    _listing: Option<Task<()>>,
    _activation: Option<Task<()>>,
}

fn accounts_dir() -> PathBuf {
    paths::data_dir().clone()
}

#[derive(Default)]
pub struct SkillCatalog {
    pub skills: Vec<agent_harness::Skill>,
    pub loaded: bool,
    fetched_at: Option<Instant>,
    _task: Option<Task<()>>,
}

#[derive(Default)]
pub struct CommandCatalog {
    pub commands: Vec<SlashCommand>,
    pub loaded: bool,
    pub error: Option<SharedString>,
    fetched_at: Option<Instant>,
    _task: Option<Task<()>>,
}

impl CommandCatalog {
    pub fn is_loading(&self) -> bool {
        self._task.is_some()
    }
}

#[derive(Default)]
pub struct PlanUsageState {
    pub usage: Option<PlanUsage>,
    pub error: Option<SharedString>,
    pub loading: bool,
    fetched_at: Option<Instant>,
    _task: Option<Task<()>>,
}

fn defaults_path(directory: &Path) -> PathBuf {
    directory.join("defaults.json")
}

impl AgentStore {
    pub fn global(languages: Arc<LanguageRegistry>, cx: &mut App) -> Entity<Self> {
        if let Some(store) = cx.try_global::<GlobalAgentStore>() {
            return store.0.clone();
        }
        let directory = paths::data_dir().join("agent_sessions");
        let store = cx.new(|cx| Self::new(directory.into(), languages, cx));
        cx.set_global(GlobalAgentStore(store.clone()));
        crate::notifications::watch(&store, cx);
        store
    }

    fn new(directory: Arc<Path>, languages: Arc<LanguageRegistry>, cx: &mut Context<Self>) -> Self {
        let load = cx.spawn({
            let directory = directory.clone();
            async move |this, cx| {
                let defaults_file = defaults_path(&directory);
                let list_file = chat_list_path(&directory);
                let (sessions, defaults, list_prefs) = cx
                    .background_spawn(async move {
                        let defaults = std::fs::read(&defaults_file).ok().and_then(|json| {
                            serde_json::from_slice::<HashMap<AgentKind, RunSettings>>(&json)
                                .log_err()
                        });
                        let list_prefs = std::fs::read(&list_file).ok().and_then(|json| {
                            serde_json::from_slice::<ChatListPrefs>(&json).log_err()
                        });
                        (load_all_metadata(&directory), defaults, list_prefs)
                    })
                    .await;
                let sessions = sessions.log_err().unwrap_or_default();
                this.update(cx, |this, cx| {
                    if let Some(defaults) = defaults {
                        this.defaults = defaults;
                    }
                    if let Some(list_prefs) = list_prefs {
                        this.list_prefs = list_prefs;
                    }
                    for metadata in sessions {
                        if !this.sessions.iter().any(|known| known.id == metadata.id) {
                            this.sessions.push(metadata);
                        }
                    }
                    this.sort();
                    cx.notify();
                })
                .log_err();
            }
        });
        Self {
            directory,
            sessions: Vec::new(),
            live: HashMap::default(),
            languages,
            harnesses: HashMap::default(),
            catalogs: HashMap::default(),
            defaults: HashMap::default(),
            plan_usage: HashMap::default(),
            commands: HashMap::default(),
            skills: HashMap::default(),
            accounts: HashMap::default(),
            list_prefs: ChatListPrefs::default(),
            save_list_prefs_task: Task::ready(()),
            metadata_writes: HashMap::default(),
            save_defaults_task: Task::ready(()),
            _load: load,
        }
    }

    fn harness(&mut self, kind: AgentKind) -> Arc<dyn Harness> {
        self.harnesses
            .entry(kind)
            .or_insert_with(|| kind.new_harness())
            .clone()
    }

    pub fn languages(&self) -> &Arc<LanguageRegistry> {
        &self.languages
    }

    pub fn models(&self, kind: AgentKind) -> &[Model] {
        match self.catalogs.get(&kind) {
            Some(catalog) if !catalog.models.is_empty() => &catalog.models,
            Some(catalog) => &catalog.fallback,
            None => &[],
        }
    }

    pub fn ensure_models(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        let harness = self.harness(kind);
        let env_loaded = shell_env_is_loaded(cx);
        let catalog = self.catalogs.entry(kind).or_default();
        if catalog.fallback.is_empty() && env_loaded {
            catalog.fallback = harness.fallback_models();
        }
        if catalog.loaded || catalog._task.is_some() {
            return;
        }
        let discovery = spawn_agent_work(cx, async move { harness.models().await });
        let task = cx.spawn(async move |this, cx| {
            let models = match discovery.await {
                Ok(Ok(models)) => models,
                Ok(Err(error)) => {
                    log::info!("{} model discovery failed: {error}", kind.label());
                    Vec::new()
                }
                Err(error) => {
                    log::error!("{} model discovery stopped: {error}", kind.label());
                    Vec::new()
                }
            };
            this.update(cx, |this, cx| {
                let fallback = this.harness(kind).fallback_models();
                let catalog = this.catalogs.entry(kind).or_default();
                catalog.fallback = fallback;
                catalog.loaded = !models.is_empty();
                catalog.models = models;
                catalog._task = None;
                cx.notify();
            })
            .log_err();
        });
        if let Some(catalog) = self.catalogs.get_mut(&kind) {
            catalog._task = Some(task);
        }
    }

    pub fn skills(&self, kind: AgentKind, cwd: &Path) -> Option<&SkillCatalog> {
        self.skills.get(&(kind, cwd.to_path_buf()))
    }

    pub fn refresh_skills(&mut self, kind: AgentKind, cwd: PathBuf, cx: &mut Context<Self>) {
        let key = (kind, cwd.clone());
        if self.skills.get(&key).is_some_and(|catalog| {
            catalog._task.is_some()
                || catalog
                    .fetched_at
                    .is_some_and(|fetched_at| fetched_at.elapsed() < CATALOG_REFRESH_INTERVAL)
        }) {
            return;
        }
        let harness = self.harness(kind);
        let discovery = spawn_agent_work(cx, async move { harness.skills(&cwd).await });
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                let skills = match discovery.await {
                    Ok(Ok(skills)) => skills,
                    Ok(Err(error)) => {
                        log::info!("{} skill discovery failed: {error}", kind.label());
                        Vec::new()
                    }
                    Err(error) => {
                        log::error!("{} skill discovery stopped: {error}", kind.label());
                        Vec::new()
                    }
                };
                this.update(cx, |this, cx| {
                    let catalog = this.skills.entry(key).or_default();
                    catalog._task = None;
                    catalog.loaded = true;
                    catalog.fetched_at = Some(Instant::now());
                    catalog.skills = skills;
                    cx.notify();
                })
                .log_err();
            }
        });
        self.skills.entry(key).or_default()._task = Some(task);
    }

    pub fn commands(&self, kind: AgentKind, cwd: &Path) -> Option<&CommandCatalog> {
        self.commands.get(&(kind, cwd.to_path_buf()))
    }

    pub fn refresh_commands(&mut self, kind: AgentKind, cwd: PathBuf, cx: &mut Context<Self>) {
        let key = (kind, cwd.clone());
        if self.commands.get(&key).is_some_and(|catalog| {
            catalog.is_loading()
                || catalog
                    .fetched_at
                    .is_some_and(|fetched_at| fetched_at.elapsed() < CATALOG_REFRESH_INTERVAL)
        }) {
            return;
        }
        let harness = self.harness(kind);
        let discovery = spawn_agent_work(cx, async move { harness.commands(&cwd).await });
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                let result = match discovery.await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                this.update(cx, |this, cx| {
                    let catalog = this.commands.entry(key).or_default();
                    catalog._task = None;
                    catalog.loaded = true;
                    catalog.fetched_at = Some(Instant::now());
                    match result {
                        Ok(commands) => {
                            catalog.commands = commands;
                            catalog.error = None;
                        }
                        Err(error) => {
                            log::info!("{} command discovery failed: {error}", kind.label());
                            catalog.error = Some(error.into());
                        }
                    }
                    cx.notify();
                })
                .log_err();
            }
        });
        self.commands.entry(key).or_default()._task = Some(task);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn skip_plan_usage(&mut self) {
        for kind in AgentKind::ALL {
            self.plan_usage.entry(kind).or_default().loading = true;
        }
    }

    pub fn accounts(&self, kind: AgentKind) -> Option<&AccountsState> {
        self.accounts.get(&kind)
    }

    pub fn refresh_accounts(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        if !kind.has_plan_usage() {
            return;
        }
        let state = self.accounts.entry(kind).or_default();
        if state.loading {
            return;
        }
        state.loading = true;
        let harness_id = kind.harness_id();
        let listing = spawn_agent_work(cx, async move {
            agent_harness::accounts::list(harness_id, &accounts_dir()).await
        });
        let http = cx.http_client();
        let task = cx.spawn(async move |this, cx| {
            let accounts = match listing.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            let others: Vec<String> = accounts
                .as_ref()
                .map(|accounts| {
                    accounts
                        .iter()
                        .filter(|account| !account.active)
                        .map(|account| account.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            this.update(cx, |this, cx| {
                let state = this.accounts.entry(kind).or_default();
                state.loading = false;
                match accounts {
                    Ok(accounts) => {
                        state.accounts = accounts;
                        state.error = None;
                    }
                    Err(error) => state.error = Some(error.into()),
                }
                cx.notify();
            })
            .log_err();
            for account_id in others {
                let request = {
                    let account_id = account_id.clone();
                    let Ok(request) = this.update(cx, |_, cx| {
                        spawn_agent_work(cx, async move {
                            agent_harness::accounts::usage_request(
                                harness_id,
                                &account_id,
                                &accounts_dir(),
                            )
                            .await
                        })
                    }) else {
                        return;
                    };
                    request
                };
                let usage = fetch_plan_usage(harness_id, request, http.clone()).await;
                this.update(cx, |this, cx| {
                    this.accounts
                        .entry(kind)
                        .or_default()
                        .usage
                        .insert(account_id, usage);
                    cx.notify();
                })
                .log_err();
            }
        });
        if let Some(state) = self.accounts.get_mut(&kind) {
            state._listing = Some(task);
        }
        cx.notify();
    }

    pub fn activate_account(&mut self, kind: AgentKind, account_id: String, cx: &mut Context<Self>) {
        let state = self.accounts.entry(kind).or_default();
        if state.switching.is_some() {
            return;
        }
        state.switching = Some(account_id.clone());
        let harness_id = kind.harness_id();
        let activation = spawn_agent_work(cx, async move {
            agent_harness::accounts::activate(harness_id, &account_id, &accounts_dir()).await
        });
        let task = cx.spawn(async move |this, cx| {
            let result = match activation.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |this, cx| {
                let state = this.accounts.entry(kind).or_default();
                state.switching = None;
                if let Err(error) = result {
                    state.error = Some(error.into());
                }
                if let Some(usage) = this.plan_usage.get_mut(&kind) {
                    usage.fetched_at = None;
                }
                if let Some(catalog) = this.catalogs.get_mut(&kind) {
                    catalog.loaded = false;
                }
                this.refresh_plan_usage(kind, true, cx);
                this.ensure_models(kind, cx);
                this.refresh_accounts(kind, cx);
            })
            .log_err();
        });
        if let Some(state) = self.accounts.get_mut(&kind) {
            state._activation = Some(task);
        }
        cx.notify();
    }

    pub fn plan_usage(&self, kind: AgentKind) -> Option<&PlanUsageState> {
        self.plan_usage.get(&kind)
    }

    pub fn refresh_plan_usage(&mut self, kind: AgentKind, force: bool, cx: &mut Context<Self>) {
        if !kind.has_plan_usage() {
            return;
        }
        let state = self.plan_usage.entry(kind).or_default();
        let min_age = if force {
            USAGE_FORCED_MIN_INTERVAL
        } else {
            USAGE_REFRESH_INTERVAL
        };
        if state.loading
            || state
                .fetched_at
                .is_some_and(|fetched_at| fetched_at.elapsed() < min_age)
        {
            return;
        }
        state.loading = true;
        let harness_id = kind.harness_id();
        let request = spawn_agent_work(cx, async move {
            agent_harness::usage::usage_request(harness_id).await
        });
        let http = cx.http_client();
        let task = cx.spawn(async move |this, cx| {
            let result = fetch_plan_usage(harness_id, request, http).await;
            this.update(cx, |this, cx| {
                let state = this.plan_usage.entry(kind).or_default();
                state.loading = false;
                state.fetched_at = Some(Instant::now());
                state._task = None;
                match result {
                    Ok(usage) => {
                        state.usage = Some(usage);
                        state.error = None;
                    }
                    Err(error) => state.error = Some(error.into()),
                }
                cx.notify();
            })
            .log_err();
        });
        if let Some(state) = self.plan_usage.get_mut(&kind) {
            state._task = Some(task);
        }
    }

    fn remember_settings(
        &mut self,
        kind: AgentKind,
        settings: RunSettings,
        cx: &mut Context<Self>,
    ) {
        if self.defaults.get(&kind) == Some(&settings) {
            return;
        }
        self.defaults.insert(kind, settings);
        let defaults = self.defaults.clone();
        let path = defaults_path(&self.directory);
        let previous_save = std::mem::replace(&mut self.save_defaults_task, Task::ready(()));
        self.save_defaults_task = cx.background_spawn(async move {
            previous_save.await;
            serde_json::to_vec_pretty(&defaults)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
    }

    pub fn sessions_in(&self, roots: &[PathBuf], cx: &App) -> Vec<SessionSummary> {
        let all_projects = self.list_prefs.all_projects;
        let mut summaries: Vec<SessionSummary> = self
            .sessions
            .iter()
            .filter(|metadata| {
                all_projects || roots.iter().any(|root| root == metadata.project_root())
            })
            .map(|metadata| {
                let live = self.live.get(&metadata.id).map(|session| session.read(cx));
                SessionSummary {
                    id: metadata.id.clone(),
                    kind: metadata.kind,
                    title: if metadata.title.is_empty() {
                        "New chat".into()
                    } else {
                        metadata.title.clone().into()
                    },
                    created_at: metadata.created_at,
                    updated_at: metadata.updated_at,
                    working: live.is_some_and(|session| session.is_working()),
                    needs_input: live.is_some_and(|session| !session.pending_questions.is_empty()),
                    pinned: metadata.pinned,
                    archived: metadata.archived,
                    section: metadata.section.clone(),
                    parent_id: metadata.parent_id.clone(),
                    side_chat: metadata.side_chat,
                    outcome: metadata.outcome,
                    unseen: metadata.unseen,
                    project_root: metadata.project_root().to_path_buf(),
                    branch: metadata.branch.clone(),
                    native_session_id: metadata.native_session_id.clone(),
                }
            })
            .collect();
        if self.list_prefs.sort == ChatSort::Created {
            summaries.sort_by_key(|summary| std::cmp::Reverse(summary.created_at));
        }
        summaries
    }

    pub fn sessions_metadata(&self) -> impl Iterator<Item = &SessionMetadata> {
        self.sessions.iter()
    }

    pub fn list_prefs(&self) -> &ChatListPrefs {
        &self.list_prefs
    }

    pub fn update_list_prefs(&mut self, change: impl FnOnce(&mut ChatListPrefs), cx: &mut Context<Self>) {
        change(&mut self.list_prefs);
        let prefs = self.list_prefs.clone();
        let path = chat_list_path(&self.directory);
        let previous_save = std::mem::replace(&mut self.save_list_prefs_task, Task::ready(()));
        self.save_list_prefs_task = cx.background_spawn(async move {
            previous_save.await;
            serde_json::to_vec_pretty(&prefs)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
        cx.notify();
    }

    pub fn update_session_metadata(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut SessionMetadata),
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.live.get(id).cloned() {
            session.update(cx, |session, cx| session.update_metadata(change, cx));
            return;
        }
        let Some(metadata) = self.sessions.iter_mut().find(|metadata| metadata.id == id) else {
            return;
        };
        change(metadata);
        let metadata = metadata.clone();
        let path = metadata_path(&self.directory, &metadata.id);
        let previous_write = self
            .metadata_writes
            .remove(&metadata.id)
            .unwrap_or_else(|| Task::ready(()));
        let write = cx.background_spawn(async move {
            previous_write.await;
            serde_json::to_vec_pretty(&metadata)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
        self.metadata_writes.insert(id.to_string(), write);
        self.sort();
        cx.notify();
    }

    pub fn rename_session(&mut self, id: &str, title: String, cx: &mut Context<Self>) {
        let title = title.trim().to_string();
        if title.is_empty() {
            return;
        }
        self.update_session_metadata(id, |metadata| metadata.title = title, cx);
    }

    pub fn set_pinned(&mut self, id: &str, pinned: bool, cx: &mut Context<Self>) {
        self.update_session_metadata(id, |metadata| metadata.pinned = pinned, cx);
    }

    pub fn set_archived(&mut self, id: &str, archived: bool, cx: &mut Context<Self>) {
        self.update_session_metadata(
            id,
            |metadata| {
                metadata.archived = archived;
                if archived {
                    metadata.pinned = false;
                }
            },
            cx,
        );
    }

    pub fn move_to_section(&mut self, id: &str, section: Option<String>, cx: &mut Context<Self>) {
        self.update_session_metadata(
            id,
            |metadata| {
                metadata.section = section;
                metadata.archived = false;
            },
            cx,
        );
    }

    pub fn create_section(&mut self, name: String, cx: &mut Context<Self>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let section = ChatSection {
            id: id.clone(),
            name: name.trim().to_string(),
        };
        self.update_list_prefs(|prefs| prefs.sections.push(section), cx);
        id
    }

    pub fn rename_section(&mut self, id: &str, name: String, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.update_list_prefs(
            |prefs| {
                if let Some(section) = prefs.sections.iter_mut().find(|section| section.id == id) {
                    section.name = name;
                }
            },
            cx,
        );
    }

    pub fn delete_section(&mut self, id: &str, cx: &mut Context<Self>) {
        let members: Vec<String> = self
            .sessions
            .iter()
            .filter(|metadata| metadata.section.as_deref() == Some(id))
            .map(|metadata| metadata.id.clone())
            .collect();
        for member in members {
            self.update_session_metadata(&member, |metadata| metadata.section = None, cx);
        }
        self.update_list_prefs(|prefs| prefs.sections.retain(|section| section.id != id), cx);
    }

    pub fn archive_section(&mut self, id: &str, cx: &mut Context<Self>) {
        let members: Vec<String> = self
            .sessions
            .iter()
            .filter(|metadata| metadata.section.as_deref() == Some(id) && !metadata.archived)
            .map(|metadata| metadata.id.clone())
            .collect();
        for member in members {
            self.set_archived(&member, true, cx);
        }
    }

    pub fn fork_session(
        &mut self,
        id: &str,
        side_chat: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<AgentSession>>> {
        let opening = self.open_session(id, cx);
        cx.spawn(async move |this, cx| {
            let parent = opening.await?;
            this.update(cx, |this, cx| {
                let (parent_metadata, mut entries) = parent.read_with(cx, |parent, cx| {
                    let mut entries = parent.transcript.serialize(cx);
                    if parent.is_working()
                        && let Some(last_user) = entries
                            .iter()
                            .rposition(|entry| matches!(entry, SerializedEntry::User { .. }))
                    {
                        entries.truncate(last_user);
                    }
                    (parent.metadata.clone(), entries)
                });
                let mut metadata = SessionMetadata::new(
                    parent_metadata.kind,
                    parent_metadata.cwd.clone(),
                    parent_metadata.settings.clone(),
                );
                metadata.parent_id = Some(parent_metadata.id.clone());
                metadata.settings.permission = parent_metadata.settings.permission;
                metadata.side_chat = side_chat;
                metadata.project_root = parent_metadata.project_root.clone();
                metadata.branch = parent_metadata.branch.clone();
                metadata.fork_context = fork_context(&entries);
                let parent_title = if parent_metadata.title.is_empty() {
                    "New chat".to_string()
                } else {
                    parent_metadata.title.clone()
                };
                let notice = if side_chat {
                    entries.clear();
                    format!("Side chat of \"{parent_title}\"")
                } else {
                    metadata.title = parent_metadata.title;
                    format!("Forked from \"{parent_title}\"")
                };
                entries.push(SerializedEntry::Notice {
                    text: notice,
                    is_error: false,
                });
                Ok(this.insert_session(metadata, SavedTranscript::EntriesOnly(entries), cx))
            })?
        })
    }

    fn insert_session(
        &mut self,
        metadata: SessionMetadata,
        saved: SavedTranscript,
        cx: &mut Context<Self>,
    ) -> Entity<AgentSession> {
        let languages = self.languages.clone();
        let directory = self.directory.clone();
        let harness = self.harness(metadata.kind);
        let session = cx.new(|cx| {
            let mut session =
                AgentSession::new(directory, harness, metadata.clone(), saved, languages, cx);
            session.touch(cx);
            session
        });
        self.sessions.push(metadata);
        self.sort();
        self.track(session.clone(), cx);
        session
    }

    pub fn create_session(
        &mut self,
        kind: AgentKind,
        cwd: PathBuf,
        cx: &mut Context<Self>,
    ) -> Entity<AgentSession> {
        self.create_session_in(kind, cwd, None, None, cx)
    }

    pub fn create_session_in(
        &mut self,
        kind: AgentKind,
        cwd: PathBuf,
        project_root: Option<PathBuf>,
        branch: Option<String>,
        cx: &mut Context<Self>,
    ) -> Entity<AgentSession> {
        let mut metadata = SessionMetadata::new(
            kind,
            cwd,
            self.defaults.get(&kind).cloned().unwrap_or_default(),
        );
        metadata.project_root = project_root.filter(|root| *root != metadata.cwd);
        metadata.branch = branch;
        let languages = self.languages.clone();
        let directory = self.directory.clone();
        let harness = self.harness(kind);
        let session = cx.new(|cx| {
            AgentSession::new(
                directory,
                harness,
                metadata.clone(),
                SavedTranscript::EntriesOnly(Vec::new()),
                languages,
                cx,
            )
        });
        self.sessions.push(metadata);
        self.sort();
        self.track(session.clone(), cx);
        session
    }

    pub fn open_session(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<AgentSession>>> {
        if let Some(session) = self.live.get(id) {
            return Task::ready(Ok(session.clone()));
        }
        let Some(metadata) = self
            .sessions
            .iter()
            .find(|metadata| metadata.id == id)
            .cloned()
        else {
            return Task::ready(Err(anyhow::anyhow!("chat {id} not found")));
        };
        let directory = self.directory.clone();
        cx.spawn(async move |this, cx| {
            let path = entries_path(&directory, &metadata.id);
            let entries = cx
                .background_spawn(async move {
                    match std::fs::read(&path) {
                        Ok(json) => serde_json::from_slice::<SavedTranscript>(&json)
                            .context("failed to parse the chat transcript"),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            Ok(SavedTranscript::EntriesOnly(Vec::new()))
                        }
                        Err(error) => Err(error).context("failed to read the chat transcript"),
                    }
                })
                .await?;
            this.update(cx, |this, cx| {
                if let Some(session) = this.live.get(&metadata.id) {
                    return Ok(session.clone());
                }
                if !this.sessions.iter().any(|known| known.id == metadata.id) {
                    anyhow::bail!("chat {} was deleted", metadata.id);
                }
                let languages = this.languages.clone();
                let harness = this.harness(metadata.kind);
                let pending_write = this.metadata_writes.remove(&metadata.id);
                let session = cx.new(|cx| {
                    let mut session =
                        AgentSession::new(directory, harness, metadata, entries, languages, cx);
                    if let Some(pending_write) = pending_write {
                        session.save_metadata_task = pending_write;
                    }
                    session
                });
                this.track(session.clone(), cx);
                Ok(session)
            })?
        })
    }

    pub fn delete_session(&mut self, id: &str, cx: &mut Context<Self>) {
        let mut pending_writes = self
            .live
            .remove(id)
            .map(|session| session.update(cx, |session, cx| session.close_for_deletion(cx)))
            .unwrap_or_default();
        pending_writes.extend(self.metadata_writes.remove(id));
        self.sessions.retain(|metadata| metadata.id != id);
        let id = id.to_string();
        let directory = self.directory.clone();
        cx.background_spawn(async move {
            futures::future::join_all(pending_writes).await;
            let _guard = FILE_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for path in [
                metadata_path(&directory, &id),
                entries_path(&directory, &id),
            ] {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => log::error!("failed to delete {}: {error}", path.display()),
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub fn models_discovered(&self, kind: AgentKind) -> Option<&[Model]> {
        self.catalogs
            .get(&kind)
            .filter(|catalog| catalog.loaded && !catalog.models.is_empty())
            .map(|catalog| catalog.models.as_slice())
    }

    fn track(&mut self, session: Entity<AgentSession>, cx: &mut Context<Self>) {
        let id = session.read(cx).metadata.id.clone();
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&session, |this, session, event, cx| match event {
            SessionEvent::MetadataChanged => {
                let metadata = session.read(cx).metadata.clone();
                if let Some(known) = this
                    .sessions
                    .iter_mut()
                    .find(|known| known.id == metadata.id)
                {
                    *known = metadata;
                }
                this.sort();
                cx.notify();
            }
            SessionEvent::SettingsChanged => {
                let session = session.read(cx);
                let (kind, settings) = (session.kind(), session.settings().clone());
                this.remember_settings(kind, settings, cx);
            }
            SessionEvent::Finished { errored } => {
                let id = session.read(cx).metadata.id.clone();
                cx.emit(StoreEvent::SessionFinished {
                    id,
                    errored: *errored,
                });
            }
            SessionEvent::NeedsInput => {
                let id = session.read(cx).metadata.id.clone();
                cx.emit(StoreEvent::SessionNeedsInput { id });
            }
        })
        .detach();
        self.live.insert(id, session);
        cx.notify();
    }

    fn sort(&mut self) {
        self.sessions
            .sort_by_key(|metadata| std::cmp::Reverse(metadata.updated_at));
    }
}

async fn fetch_plan_usage(
    harness_id: HarnessId,
    request: Task<
        std::result::Result<
            std::result::Result<agent_harness::usage::UsageRequest, String>,
            gpui_tokio::JoinError,
        >,
    >,
    http: Arc<dyn http_client::HttpClient>,
) -> std::result::Result<PlanUsage, String> {
    use futures::AsyncReadExt as _;
    let request = request.await.map_err(|error| error.to_string())??;
    let mut builder = http_client::Request::builder()
        .method(http_client::Method::GET)
        .uri(request.url);
    for (name, value) in &request.headers {
        builder = builder.header(*name, value);
    }
    let unavailable = |error: anyhow::Error| format!("Usage unavailable: {error}");
    let http_request = builder
        .body(http_client::AsyncBody::empty())
        .map_err(|error| unavailable(error.into()))?;
    let mut response = http.send(http_request).await.map_err(unavailable)?;
    if !response.status().is_success() {
        return Err(agent_harness::usage::status_message(
            response.status().as_u16(),
        ));
    }
    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .map_err(|error| unavailable(error.into()))?;
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| unavailable(error.into()))?;
    request
        .parse(harness_id, &json)
        .ok_or_else(|| "Usage unavailable: unexpected response".to_string())
}

fn load_all_metadata(directory: &Path) -> Result<Vec<SessionMetadata>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("failed to list saved chats"),
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let is_metadata = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".meta.json"));
        if !is_metadata {
            continue;
        }
        let parsed = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|json| Ok(serde_json::from_slice::<SessionMetadata>(&json)?))
            .with_context(|| format!("failed to load {}", path.display()));
        if let Some(metadata) = parsed.log_err() {
            sessions.push(metadata);
        }
    }
    Ok(sessions)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn fake_claude() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../agent_harness/tests/fixtures/fake-claude.sh")
    }

    pub(crate) fn new_store(directory: &Path, cx: &mut TestAppContext) -> Entity<AgentStore> {
        cx.executor().allow_parking();
        // SAFETY: tests in this crate only ever set this variable to the same fixture.
        unsafe { std::env::set_var("CLAUDE_CODE_EXECUTABLE", fake_claude()) };
        cx.update(gpui_tokio::init);
        let languages = Arc::new(LanguageRegistry::test(cx.background_executor.clone()));
        cx.new(|cx| AgentStore::new(directory.into(), languages, cx))
    }

    pub(crate) fn run_until(
        cx: &mut TestAppContext,
        mut done: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        for _ in 0..500 {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the agent never reached the expected state");
    }

    #[gpui::test]
    async fn says_once_when_claude_did_not_start_in_auto(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let notices = |cx: &mut TestAppContext| {
            session.read_with(cx, |session, cx| {
                summarize(session.entries(), cx)
                    .into_iter()
                    .filter(|entry| entry.starts_with("notice: Auto mode isn't available"))
                    .count()
            })
        };
        let reported = |mode: &str| {
            vec![AgentEvent::PermissionModeReported {
                mode: mode.to_string(),
            }]
        };
        feed_events(&session, reported("auto"), cx);
        assert_eq!(notices(cx), 0);
        feed_events(&session, reported("default"), cx);
        feed_events(&session, reported("default"), cx);
        assert_eq!(notices(cx), 1);
    }

    #[gpui::test]
    async fn a_chosen_permission_mode_stays_with_its_chat(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        assert_eq!(
            session.read_with(cx, |session, _| session.settings().permission),
            PermissionMode::Auto
        );
        session.update(cx, |session, cx| {
            session.update_settings(
                |settings| settings.permission = PermissionMode::FullAccess,
                cx,
            )
        });
        let id = session_id(&session, cx);
        cx.run_until_parked();

        let reopened_store = new_store(directory.path(), cx);
        cx.run_until_parked();
        let reopened = reopened_store
            .update(cx, |store, cx| store.open_session(&id, cx))
            .await
            .expect("chat reopens");
        assert_eq!(
            reopened.read_with(cx, |session, _| session.settings().permission),
            PermissionMode::FullAccess
        );
        let fresh = reopened_store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        assert_eq!(
            fresh.read_with(cx, |session, _| session.settings().permission),
            PermissionMode::Auto,
            "new chats always start in auto"
        );
    }

    #[gpui::test]
    async fn claude_reply_streams_into_the_transcript_and_is_saved(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:happy", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
        });

        session.read_with(cx, |session, cx| {
            assert_eq!(session.metadata().title, "scenario:happy");
            assert_eq!(
                session.metadata().native_session_id.as_deref(),
                Some("sess-1")
            );
            let summary: Vec<String> = session
                .entries()
                .iter()
                .map(|entry| match entry {
                    Entry::User { text, .. } => format!("user: {text}"),
                    Entry::Assistant { markdown, .. } => {
                        format!("assistant: {}", markdown.read(cx).source())
                    }
                    Entry::Thinking(markdown) => {
                        format!("thinking: {}", markdown.read(cx).source())
                    }
                    Entry::Tool(tool) => format!("tool {}: {:?}", tool.id, tool.status),
                    Entry::Notice { text, .. } => format!("notice: {text}"),
                    Entry::Image { path } => format!("image: {}", path.display()),
                })
                .collect();
            assert_eq!(
                summary,
                [
                    "user: scenario:happy",
                    "thinking: pondering",
                    "assistant: Hello",
                    "tool tool-1: Completed",
                    "tool tool-2: Failed",
                ]
            );
        });

        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        let saved_entries = entries_path(directory.path(), &session_id(&session, cx));
        run_until(cx, |_| saved_entries.exists());
        let summaries = store.read_with(cx, |store, cx| {
            store.sessions_in(&[directory.path().to_path_buf()], cx)
        });
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].working);
    }

    #[gpui::test]
    async fn permissions_and_questions_wait_for_the_user(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:askuser", cx)
        });

        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.pending_questions().is_empty())
        });
        let permission_id = session.read_with(cx, |session, _| {
            let pending = &session.pending_questions()[0];
            assert!(pending.is_permission());
            assert_eq!(pending.questions[0].question, "Run `ls`?");
            pending.id().to_string()
        });
        session.update(cx, |session, cx| {
            session.answer_permission(&permission_id, true, cx)
        });

        run_until(cx, |cx| {
            session.read_with(cx, |session, _| {
                session
                    .pending_questions()
                    .first()
                    .is_some_and(|pending| !pending.is_permission())
            })
        });
        let (question_id, answers) = session.read_with(cx, |session, _| {
            let pending = &session.pending_questions()[0];
            let answers = pending
                .questions
                .iter()
                .map(|question| UserInputAnswer {
                    question_id: question.id.clone(),
                    labels: vec!["B".into()],
                })
                .collect::<Vec<_>>();
            (pending.id().to_string(), answers)
        });
        session.update(cx, |session, cx| session.answer(&question_id, answers, cx));

        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
        });
        session.read_with(cx, |session, _| {
            assert!(session.pending_questions().is_empty());
            assert!(
                !session
                    .entries()
                    .iter()
                    .any(|entry| matches!(entry, Entry::Notice { is_error: true, .. })),
                "the fixture reports an error unless both answers arrive"
            );
        });
    }

    #[gpui::test]
    async fn picked_settings_become_the_default_for_new_chats(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let first = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Codex, directory.path().to_path_buf(), cx)
        });
        first.update(cx, |session, cx| {
            session.update_settings(
                |settings| {
                    settings.model = Some("gpt-5.6-sol".into());
                    settings.reasoning = Some(ReasoningLevel::High);
                },
                cx,
            )
        });
        cx.run_until_parked();
        let second = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Codex, directory.path().to_path_buf(), cx)
        });
        second.read_with(cx, |session, _| {
            assert_eq!(session.settings().model.as_deref(), Some("gpt-5.6-sol"));
            assert_eq!(session.settings().reasoning, Some(ReasoningLevel::High));
        });
        let claude = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        claude.read_with(cx, |session, _| {
            assert_eq!(session.settings(), &RunSettings::default())
        });
    }

    #[gpui::test]
    async fn context_usage_keeps_known_fields_across_partial_reports(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: Some(20_000),
                    window: None,
                },
                cx,
            );
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: None,
                    window: Some(200_000),
                },
                cx,
            );
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: Some(50_000),
                    window: Some(0),
                },
                cx,
            );
        });
        session.read_with(cx, |session, _| {
            let context = session.context().expect("context measured");
            assert_eq!(context.tokens, Some(50_000));
            assert_eq!(context.window, Some(200_000));
            assert_eq!(context.fraction(), Some(0.25));
        });
    }

    fn last_reply(session: &Entity<AgentSession>, cx: &mut TestAppContext) -> String {
        session.read_with(cx, |session, cx| {
            session
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match entry {
                    Entry::Assistant { markdown, .. } => {
                        Some(markdown.read(cx).source().to_string())
                    }
                    _ => None,
                })
                .unwrap_or_default()
        })
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn changed_settings_restart_the_agent_with_new_flags(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = directory.path().join("echo-args-claude");
        std::fs::write(
            &script,
            r#"#!/bin/sh
args="$*"
printf '{"type":"system","subtype":"init","model":"m","tools":[],"cwd":"/tmp","session_id":"sess-args"}\n'
while read -r line; do
  printf '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"%s"}}}\n' "$args"
  printf '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[]}}\n'
  printf '{"type":"result","subtype":"success","result":"ok","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-args"}\n'
done
"#,
        )
        .expect("write fixture");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("make fixture executable");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| {
            store.harnesses.insert(
                AgentKind::Claude,
                Arc::new(ClaudeHarness::new().with_executable(script)),
            );
        });
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.update_settings(
                |settings| {
                    settings.model = Some("claude-opus-5-5".into());
                    settings.reasoning = Some(ReasoningLevel::High);
                },
                cx,
            );
            session.send_message("first", cx);
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
        });
        let first = last_reply(&session, cx);
        assert!(first.contains("--model claude-opus-5-5"), "{first}");
        assert!(first.contains("--effort high"), "{first}");
        assert!(!first.contains("--resume"), "{first}");

        session.update(cx, |session, cx| {
            session.update_settings(
                |settings| settings.reasoning = Some(ReasoningLevel::Low),
                cx,
            );
            session.send_message("second", cx);
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
                && last_reply(&session, cx) != first
        });
        let second = last_reply(&session, cx);
        assert!(second.contains("--effort low"), "{second}");
        assert!(second.contains("--resume=sess-args"), "{second}");
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn manual_compaction_reloads_claude_on_the_next_message(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().expect("temporary directory");
        let script = directory.path().join("steer-aware-claude");
        std::fs::write(
            &script,
            r#"#!/bin/sh
case "$*" in *--resume*) run=resumed;; *) run=new;; esac
printf '{"type":"system","subtype":"init","model":"m","tools":[],"cwd":"/tmp","session_id":"sess-commands"}\n'
while read -r line; do
  case "$line" in *priority*) kind=steer;; *) kind=first;; esac
  case "$line" in
    *'"/compact'*)
      printf '{"type":"system","subtype":"status","status":"compacting","session_id":"sess-commands"}\n'
      printf '{"type":"system","subtype":"compact_boundary","session_id":"sess-commands","compact_metadata":{"trigger":"manual","pre_tokens":900,"post_tokens":120}}\n'
      printf '{"type":"system","subtype":"status","status":null,"compact_result":"success","session_id":"sess-commands"}\n';;
    *'"long task'*)
      printf '{"type":"system","subtype":"compact_boundary","session_id":"sess-commands","compact_metadata":{"trigger":"auto","pre_tokens":900,"post_tokens":150}}\n';;
    *'"doomed'*)
      printf '{"type":"system","subtype":"status","status":"compacting","session_id":"sess-commands"}\n'
      printf '{"type":"system","subtype":"status","status":null,"compact_result":"failed","compact_error":"prompt_too_long","session_id":"sess-commands"}\n';;
  esac
  printf '{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"%s %s"}}}\n' "$run" "$kind"
  printf '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[]}}\n'
  printf '{"type":"result","subtype":"success","result":"ok","errors":[],"usage":{"input_tokens":1,"output_tokens":1},"session_id":"sess-commands"}\n'
done
"#,
        )
        .expect("write fixture");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("make fixture executable");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| {
            store.harnesses.insert(
                AgentKind::Claude,
                Arc::new(ClaudeHarness::new().with_executable(script)),
            );
        });
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });

        for (message, expected) in [
            ("hello", "new first"),
            ("follow up", "new steer"),
            ("/compact", "new steer"),
            ("after compacting", "resumed first"),
            ("long task", "resumed steer"),
            ("after auto compaction", "resumed steer"),
            ("doomed", "resumed steer"),
        ] {
            let entries_before = session.read_with(cx, |session, _| session.entries().len());
            session.update(cx, |session, cx| session.send_message(message, cx));
            run_until(cx, |cx| {
                session.read_with(cx, |session, _| {
                    !session.is_working() && session.entries().len() > entries_before + 1
                })
            });
            assert_eq!(last_reply(&session, cx), expected, "reply to {message:?}");
        }
        session.read_with(cx, |session, _| {
            assert!(!session.is_compacting());
            assert_eq!(
                session.metadata.context.and_then(|context| context.tokens),
                Some(150)
            );
            let notices: Vec<(&str, bool)> = session
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Notice { text, is_error } => Some((text.as_ref(), *is_error)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                notices,
                [
                    ("Context compacted.", false),
                    ("Context compacted.", false),
                    ("Couldn't compact the conversation: prompt_too_long", true),
                ]
            );
        });
    }

    #[gpui::test]
    async fn compacting_label_follows_the_agent(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.working = true;
            session.apply_event(AgentEvent::Compacting { active: true }, cx);
            assert!(session.is_compacting());
            session.apply_event(AgentEvent::Compacting { active: false }, cx);
            assert!(!session.is_compacting());
            session.apply_event(AgentEvent::Compacting { active: true }, cx);
            session.apply_event(
                AgentEvent::Compacted {
                    tokens: None,
                    manual: false,
                },
                cx,
            );
            assert!(!session.is_compacting());
            assert!(!session.restart_on_next_send, "auto compaction keeps the process");
            assert_eq!(session.metadata.context.and_then(|context| context.tokens), None);
            session.apply_event(AgentEvent::Compacting { active: true }, cx);
            session.apply_event(
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
                cx,
            );
            assert!(!session.is_compacting());
        });
    }

    #[gpui::test]
    async fn commands_are_never_glued_to_messages_sent_while_stopping(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:interrupt", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| {
                session.metadata().native_session_id.is_some()
            })
        });
        session.update(cx, |session, cx| {
            session.stop(cx);
            session.send_message("/compact", cx);
            session.send_message("scenario:happy", cx);
            assert_eq!(
                session.prompt_after_stop.as_ref().map(|prompt| prompt.text.as_str()),
                Some("/compact")
            );
            let queued: Vec<&str> = session
                .queue()
                .iter()
                .map(|queued| queued.prompt.text.as_str())
                .collect();
            assert_eq!(queued, ["scenario:happy"]);
            assert!(!session.can_steer(&Prompt::from("/compact".to_string())));
        });
    }

    fn use_quick_stopping_claude(store: &Entity<AgentStore>, cx: &mut TestAppContext) {
        store.update(cx, |store, _| {
            store.harnesses.insert(
                AgentKind::Claude,
                Arc::new(
                    ClaudeHarness::new()
                        .with_executable(fake_claude())
                        .with_graces(Duration::from_millis(50), Duration::from_millis(50)),
                ),
            );
        });
    }

    #[gpui::test]
    async fn message_sent_while_stopping_runs_after_the_stop(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:interrupt", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| {
                session.metadata().native_session_id.is_some()
            })
        });
        session.update(cx, |session, cx| {
            session.stop(cx);
            session.send_message("scenario:happy", cx);
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
                && last_reply(&session, cx) == "Hello"
        });
    }

    #[gpui::test]
    async fn deleting_a_running_chat_removes_its_files_for_good(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:interrupt", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| {
                session.metadata().native_session_id.is_some()
            })
        });
        let id = session_id(&session, cx);
        let metadata_file = metadata_path(directory.path(), &id);
        run_until(cx, |_| metadata_file.exists());

        store.update(cx, |store, cx| store.delete_session(&id, cx));
        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: Some(1),
                    window: Some(2),
                },
                cx,
            );
            session.send_message("scenario:happy", cx);
        });
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        run_until(cx, |_| !metadata_file.exists());
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        assert!(!metadata_file.exists());
        assert!(!entries_path(directory.path(), &id).exists());
        session.read_with(cx, |session, _| assert!(!session.is_working()));
    }

    #[gpui::test]
    async fn a_steer_started_after_a_turn_ends_counts_as_working(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Codex, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
                cx,
            );
            assert!(!session.is_working());
            session.apply_event(
                AgentEvent::Steered {
                    assistant_message_id: None,
                    next_assistant_message_id: None,
                },
                cx,
            );
            assert!(session.is_working());
        });
    }

    #[gpui::test]
    async fn a_turn_the_agent_starts_on_its_own_counts_as_working(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let done = || AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        };
        session.update(cx, |session, cx| {
            for wake in [
                AgentEvent::TextDelta {
                    text: "The build finished.".into(),
                },
                AgentEvent::ReasoningDelta {
                    text: "Checking the output".into(),
                },
                AgentEvent::ToolCall {
                    id: "tool-1".into(),
                    call: ToolCall::Exec {
                        command: "cat build.log".into(),
                    },
                },
            ] {
                session.apply_event(done(), cx);
                assert!(!session.is_working());
                session.apply_event(wake, cx);
                assert!(session.is_working());
                assert!(session.working_since().is_some());
            }
            session.apply_event(done(), cx);
            session.apply_event(
                AgentEvent::TaskFinished {
                    task_id: "task-1".into(),
                    status: DoneStatus::Completed,
                },
                cx,
            );
            session.apply_event(
                AgentEvent::ContextUsage {
                    tokens: Some(10),
                    window: None,
                },
                cx,
            );
            assert!(!session.is_working());
        });
    }

    #[gpui::test]
    async fn a_chat_deleted_while_opening_stays_deleted(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let id = store.update(cx, |store, cx| {
            let session =
                store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx);
            session.read(cx).metadata().id.clone()
        });
        store.update(cx, |store, _| store.live.clear());
        let opening = store.update(cx, |store, cx| store.open_session(&id, cx));
        store.update(cx, |store, cx| store.delete_session(&id, cx));
        assert!(opening.await.is_err());
        store.read_with(cx, |store, _| assert!(!store.live.contains_key(&id)));
    }

    #[gpui::test]
    async fn background_tasks_follow_the_live_list(cx: &mut TestAppContext) {
        use agent_harness::{BackgroundTaskInfo, BackgroundTaskKind};
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let info = |task_id: &str, kind| BackgroundTaskInfo {
            task_id: task_id.into(),
            kind,
            description: String::new(),
        };
        feed_events(
            &session,
            vec![
                AgentEvent::TaskStarted {
                    task_id: "fg1".into(),
                    tool_use_id: Some("toolu_fg".into()),
                    kind: BackgroundTaskKind::Shell,
                    description: "Foreground command".into(),
                },
                AgentEvent::TaskStarted {
                    task_id: "bg1".into(),
                    tool_use_id: Some("toolu_bg".into()),
                    kind: BackgroundTaskKind::Shell,
                    description: "Run tests".into(),
                },
            ],
            cx,
        );
        let tasks = |cx: &mut TestAppContext| {
            session.read_with(cx, |session, _| {
                session
                    .background_tasks()
                    .iter()
                    .map(|task| (task.task_id.clone(), task.description.clone(), task.tool_use_id.clone()))
                    .collect::<Vec<_>>()
            })
        };
        assert!(tasks(cx).is_empty(), "started tasks are not background work by themselves");
        feed_events(
            &session,
            vec![AgentEvent::BackgroundTasksChanged {
                tasks: vec![
                    info("bg1", BackgroundTaskKind::Shell),
                    info("a1", BackgroundTaskKind::Agent),
                ],
            }],
            cx,
        );
        assert_eq!(
            tasks(cx),
            [
                ("bg1".into(), "Run tests".into(), Some("toolu_bg".into())),
                ("a1".into(), String::new(), None),
            ]
        );
        feed_events(
            &session,
            vec![AgentEvent::BackgroundTasksChanged {
                tasks: vec![info("a1", BackgroundTaskKind::Agent)],
            }],
            cx,
        );
        assert_eq!(tasks(cx).len(), 1, "the list replaces the set even without an end message");
        feed_events(
            &session,
            vec![AgentEvent::TaskFinished {
                task_id: "a1".into(),
                status: DoneStatus::Completed,
            }],
            cx,
        );
        assert!(tasks(cx).is_empty());
        feed_events(
            &session,
            vec![AgentEvent::BackgroundTasksChanged {
                tasks: vec![info("bg2", BackgroundTaskKind::Shell)],
            }],
            cx,
        );
        session.update(cx, |session, cx| session.run_ended(cx));
        assert!(tasks(cx).is_empty(), "tasks end with the agent process");
    }

    #[gpui::test]
    async fn empty_reasoning_never_adds_a_thought(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        feed_events(
            &session,
            vec![
                AgentEvent::ReasoningDelta {
                    text: String::new(),
                },
                AgentEvent::TextDelta {
                    text: "one ".into(),
                },
                AgentEvent::ReasoningDelta {
                    text: String::new(),
                },
                AgentEvent::TextDelta { text: "two".into() },
            ],
            cx,
        );
        assert_eq!(last_reply(&session, cx), "one two");
        session.read_with(cx, |session, _| {
            assert_eq!(session.entries().len(), 1);
        });
    }

    pub(crate) fn feed_user(session: &Entity<AgentSession>, text: &str, cx: &mut TestAppContext) {
        session.update(cx, |session, cx| {
            session
                .transcript
                .push_user(text.to_string(), Vec::new(), Vec::new());
            cx.notify();
        });
    }

    pub(crate) fn feed_events(
        session: &Entity<AgentSession>,
        events: Vec<AgentEvent>,
        cx: &mut TestAppContext,
    ) {
        session.update(cx, |session, cx| {
            session.working = true;
            for event in events {
                session.apply_event(event, cx);
            }
        });
    }

    pub(crate) fn feed_subagent(session: &Entity<AgentSession>, cx: &mut TestAppContext) {
        let child = |event| AgentEvent::Subagent {
            parent_tool_use_id: "spawn-1".into(),
            event: Box::new(event),
        };
        session.update(cx, |session, cx| {
            session.working = true;
            for event in [
                AgentEvent::ToolCall {
                    id: "spawn-1".into(),
                    call: ToolCall::Unknown {
                        name: "Agent: scan the repo".into(),
                        input: None,
                    },
                },
                child(AgentEvent::UserMessage {
                    text: "Scan the repo".into(),
                }),
                child(AgentEvent::TextDelta {
                    text: "Looking".into(),
                }),
                child(AgentEvent::ToolCall {
                    id: "child-tool".into(),
                    call: ToolCall::Exec {
                        command: "ls".into(),
                    },
                }),
            ] {
                session.apply_event(event, cx);
            }
        });
    }

    fn summarize(entries: &[Entry], cx: &App) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::User { text, .. } => format!("user: {text}"),
                Entry::Assistant { markdown, .. } => {
                    format!("assistant: {}", markdown.read(cx).source())
                }
                Entry::Thinking(markdown) => format!("thinking: {}", markdown.read(cx).source()),
                Entry::Tool(tool) => format!("tool {}: {:?}", tool.id, tool.status),
                Entry::Notice { text, .. } => format!("notice: {text}"),
                Entry::Image { path } => format!("image: {}", path.display()),
            })
            .collect()
    }

    #[gpui::test]
    async fn subagent_activity_gets_its_own_saved_transcript(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        feed_subagent(&session, cx);
        session.read_with(cx, |session, cx| {
            assert_eq!(summarize(session.entries(), cx), ["tool spawn-1: Running"]);
            let subagent = session.subagent("spawn-1").expect("subagent recorded");
            assert_eq!(
                summarize(subagent.entries(), cx),
                [
                    "user: Scan the repo",
                    "assistant: Looking",
                    "tool child-tool: Running"
                ]
            );
            assert_eq!(subagent.status(), None);
        });
        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::Subagent {
                    parent_tool_use_id: "spawn-1".into(),
                    event: Box::new(AgentEvent::Done {
                        status: DoneStatus::Completed,
                        result: None,
                        error: None,
                        session_id: None,
                    }),
                },
                cx,
            );
            assert!(!session.subagent_running("spawn-1"));
        });

        let id = session_id(&session, cx);
        session.update(cx, |session, cx| session.touch(cx));
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        let reopened_store = new_store(directory.path(), cx);
        cx.run_until_parked();
        let reopened = reopened_store
            .update(cx, |store, cx| store.open_session(&id, cx))
            .await
            .expect("chat reopens");
        reopened.read_with(cx, |session, cx| {
            let subagent = session.subagent("spawn-1").expect("subagent saved");
            assert_eq!(subagent.status(), Some(DoneStatus::Completed));
            assert_eq!(
                summarize(subagent.entries(), cx),
                [
                    "user: Scan the repo",
                    "assistant: Looking",
                    "tool child-tool: Canceled"
                ]
            );
        });
    }

    #[gpui::test]
    async fn claude_background_subagent_traffic_lands_in_its_transcript(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:wake", cx)
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| session.run.is_none() && !session.is_working())
        });
        session.read_with(cx, |session, cx| {
            let subagent = session.subagent("toolu_agent").expect("subagent recorded");
            assert_eq!(
                summarize(subagent.entries(), cx),
                ["assistant: sub working", "tool sub-t1: Completed"]
            );
            assert!(!summarize(session.entries(), cx).contains(&"assistant: sub working".into()));
        });
    }

    #[test]
    fn transcripts_saved_before_subagents_still_load() {
        let saved: SavedTranscript =
            serde_json::from_str(r#"[{"type":"user","text":"hi","at":1}]"#).expect("parses");
        assert!(matches!(saved, SavedTranscript::EntriesOnly(entries) if entries.len() == 1));
        let saved: SavedTranscript =
            serde_json::from_str(r#"{"entries":[],"subagents":[{"id":"a","entries":[]}]}"#)
                .expect("parses");
        assert!(
            matches!(saved, SavedTranscript::Current { subagents, .. } if subagents.len() == 1)
        );
        let saved: SavedTranscript =
            serde_json::from_str(r#"{"entries":[],"queue":[{"text":"later"}]}"#).expect("parses");
        assert!(matches!(saved, SavedTranscript::Current { queue, .. } if queue.len() == 1));
    }

    fn user_texts(session: &Entity<AgentSession>, cx: &mut TestAppContext) -> Vec<String> {
        session.read_with(cx, |session, _| {
            session
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    Entry::User { text, .. } => Some(text.to_string()),
                    _ => None,
                })
                .collect()
        })
    }

    #[gpui::test]
    async fn messages_sent_while_working_queue_and_go_out_after_the_turn(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| session.send_message("scenario:interrupt", cx));
        run_until(cx, |cx| session.read_with(cx, |session, _| session.run.is_some()));
        session.update(cx, |session, cx| {
            session.send_message("first queued", cx);
            session.send_message("second queued", cx);
            session.send_message("third queued", cx);
        });
        let ids: Vec<u64> = session.read_with(cx, |session, _| {
            session.queue().iter().map(|queued| queued.id).collect()
        });
        assert_eq!(ids.len(), 3);
        assert_eq!(user_texts(&session, cx), ["scenario:interrupt"]);
        session.update(cx, |session, cx| {
            session.move_queued(ids[2], 0, cx);
            session.remove_queued(ids[1], cx);
        });
        let prompt = session
            .update(cx, |session, cx| session.begin_queued_edit(ids[0], cx))
            .expect("queued message");
        session.update(cx, |session, cx| {
            session.finish_queued_edit(
                ids[0],
                Some(Prompt::text(format!("{} edited", prompt.text))),
                cx,
            )
        });
        let queued: Vec<String> = session.read_with(cx, |session, _| {
            session.queue().iter().map(|queued| queued.prompt.text.clone()).collect()
        });
        assert_eq!(queued, ["third queued", "first queued edited"]);

        session.update(cx, |session, cx| {
            session.apply_event(
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
                cx,
            );
        });
        assert_eq!(
            user_texts(&session, cx),
            ["scenario:interrupt", "third queued"]
        );
        session.read_with(cx, |session, _| {
            assert_eq!(session.queue().len(), 1);
            assert!(session.is_working());
        });
    }

    #[gpui::test]
    async fn send_now_shows_the_message_and_queue_order_survives_idle(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        use_quick_stopping_claude(&store, cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| session.send_message("scenario:interrupt", cx));
        run_until(cx, |cx| session.read_with(cx, |session, _| session.run.is_some()));
        session.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        let id = session.read_with(cx, |session, _| session.queue()[0].id);
        session.update(cx, |session, cx| session.send_queued_now(id, cx));
        assert_eq!(
            user_texts(&session, cx),
            ["scenario:interrupt", "scenario:happy"]
        );
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
                && last_reply(&session, cx) == "Hello"
        });

        session.update(cx, |session, cx| {
            session.enqueue(Prompt::text("older"), cx);
            session.send_message("newer", cx);
        });
        assert_eq!(user_texts(&session, cx).last().map(String::as_str), Some("older"));
        let queued: Vec<String> = session.read_with(cx, |session, _| {
            session.queue().iter().map(|queued| queued.prompt.text.clone()).collect()
        });
        assert_eq!(queued, ["newer"]);
    }

    #[gpui::test]
    async fn the_queue_continues_after_a_failed_turn_but_not_after_stop(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let done = |status| AgentEvent::Done {
            status,
            result: None,
            error: None,
            session_id: None,
        };
        session.update(cx, |session, cx| {
            session.enqueue(Prompt::text("after error"), cx);
            session.working = true;
            session.apply_event(done(DoneStatus::Errored), cx);
        });
        assert_eq!(user_texts(&session, cx).last().map(String::as_str), Some("after error"));

        session.update(cx, |session, cx| {
            session.enqueue(Prompt::text("after stop"), cx);
            session.stop(cx);
            session.working = true;
            session.apply_event(done(DoneStatus::Interrupted), cx);
            session.run_ended(cx);
        });
        session.read_with(cx, |session, _| assert_eq!(session.queue().len(), 1));
    }

    pub(crate) fn enqueue(session: &Entity<AgentSession>, text: &str, cx: &mut TestAppContext) {
        session.update(cx, |session, cx| session.enqueue(Prompt::text(text), cx));
    }

    fn saved_queue(session: &Entity<AgentSession>, cx: &mut TestAppContext) -> Vec<String> {
        session.read_with(cx, |session, cx| match session.saved_transcript(cx) {
            SavedTranscript::Current { queue, .. } => {
                queue.into_iter().map(|prompt| prompt.text).collect()
            }
            SavedTranscript::EntriesOnly(_) => Vec::new(),
        })
    }

    #[gpui::test]
    async fn a_queued_message_being_edited_stays_saved_and_is_not_sent(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        enqueue(&session, "being edited", cx);
        enqueue(&session, "next up", cx);
        let edited_id = session.read_with(cx, |session, _| session.queue()[0].id);
        let prompt = session
            .update(cx, |session, cx| session.begin_queued_edit(edited_id, cx))
            .expect("queued message");
        assert_eq!(prompt.text, "being edited");
        assert_eq!(saved_queue(&session, cx), ["being edited", "next up"]);

        session.update(cx, |session, cx| {
            session.working = true;
            session.apply_event(
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
                cx,
            );
        });
        assert_eq!(user_texts(&session, cx).last().map(String::as_str), Some("next up"));
        assert_eq!(saved_queue(&session, cx), ["being edited"]);

        session.update(cx, |session, cx| {
            session.finish_queued_edit(edited_id, Some(Prompt::text("  ")), cx)
        });
        session.read_with(cx, |session, _| assert_eq!(session.editing_queued(), None));
        assert_eq!(saved_queue(&session, cx), ["being edited"]);
    }

    #[gpui::test]
    async fn retrying_keeps_unrelated_errors(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.transcript.push_notice("older crash".into(), true);
            session
                .transcript
                .push_user("failed".into(), Vec::new(), Vec::new());
            session.fail_run("could not start".into(), cx);
        });
        let index = session.read_with(cx, |session, _| {
            session
                .entries()
                .iter()
                .position(|entry| matches!(entry, Entry::User { undelivered: true, .. }))
                .expect("undelivered message")
        });
        session.update(cx, |session, cx| session.retry_undelivered(index, cx));
        session.read_with(cx, |session, cx| {
            let summary = summarize(session.entries(), cx);
            assert_eq!(summary[0], "notice: older crash");
            assert!(!summary.contains(&"notice: could not start".to_string()));
            assert_eq!(summary.iter().filter(|line| *line == "user: failed").count(), 1);
        });
    }

    #[gpui::test]
    async fn pins_sections_and_archive_are_saved_without_opening_the_chat(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let id = store.update(cx, |store, cx| {
            let session = store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx);
            session.update(cx, |session, cx| session.touch(cx));
            session.read(cx).metadata().id.clone()
        });
        store.update(cx, |store, _| store.live.clear());
        let section = store.update(cx, |store, cx| {
            store.rename_session(&id, "  Renamed  ".into(), cx);
            store.set_pinned(&id, true, cx);
            let section = store.create_section("Later".into(), cx);
            store.move_to_section(&id, Some(section.clone()), cx);
            section
        });
        cx.run_until_parked();
        let reopened = new_store(directory.path(), cx);
        cx.run_until_parked();
        reopened.read_with(cx, |store, cx| {
            let summary = &store.sessions_in(&[directory.path().to_path_buf()], cx)[0];
            assert_eq!(summary.title.as_ref(), "Renamed");
            assert!(summary.pinned);
            assert_eq!(summary.section.as_deref(), Some(section.as_str()));
            assert_eq!(store.list_prefs().sections[0].name, "Later");
        });
        reopened.update(cx, |store, cx| {
            store.set_archived(&id, true, cx);
            store.delete_section(&section, cx);
        });
        reopened.read_with(cx, |store, cx| {
            let summary = &store.sessions_in(&[directory.path().to_path_buf()], cx)[0];
            assert!(summary.archived);
            assert!(!summary.pinned);
            assert_eq!(summary.section, None);
            assert!(store.list_prefs().sections.is_empty());
        });
    }

    #[gpui::test]
    async fn forks_copy_history_and_send_it_with_their_first_message(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let parent = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        parent.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        run_until(cx, |cx| parent.read_with(cx, |session, _| !session.is_working()));
        parent.update(cx, |session, cx| {
            session.update_settings(
                |settings| settings.permission = PermissionMode::FullAccess,
                cx,
            )
        });
        let parent_id = session_id(&parent, cx);

        let fork = store
            .update(cx, |store, cx| store.fork_session(&parent_id, false, cx))
            .await
            .expect("fork");
        fork.read_with(cx, |fork, cx| {
            let summary = summarize(fork.entries(), cx);
            assert_eq!(summary.first().map(String::as_str), Some("user: scenario:happy"));
            assert!(summary.last().is_some_and(|last| last.starts_with("notice: Forked from")));
            let context = fork.metadata().fork_context.clone().expect("history");
            assert!(context.contains("scenario:happy") && context.contains("Hello"));
            assert_eq!(fork.metadata().parent_id.as_deref(), Some(parent_id.as_str()));
            assert!(!fork.metadata().side_chat);
            assert_eq!(fork.settings().permission, PermissionMode::FullAccess);
        });

        let side = store
            .update(cx, |store, cx| store.fork_session(&parent_id, true, cx))
            .await
            .expect("side chat");
        side.read_with(cx, |side, cx| {
            assert_eq!(summarize(side.entries(), cx).len(), 1);
            assert!(side.metadata().side_chat);
            assert!(side.metadata().fork_context.is_some());
        });
        side.update(cx, |side, cx| side.send_message("scenario:happy", cx));
        run_until(cx, |cx| side.read_with(cx, |session, _| !session.is_working()));
        side.read_with(cx, |side, _| assert!(side.metadata().fork_context.is_none()));
    }

    #[gpui::test]
    async fn finished_runs_stay_unseen_until_viewed(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| session.send_message("scenario:happy", cx));
        run_until(cx, |cx| session.read_with(cx, |session, _| !session.is_working()));
        session.read_with(cx, |session, _| {
            assert_eq!(session.metadata().outcome, Some(ChatOutcome::Completed));
            assert!(session.metadata().unseen);
        });
        session.update(cx, |session, cx| session.mark_seen(cx));
        session.read_with(cx, |session, _| assert!(!session.metadata().unseen));
    }

    fn session_id(session: &Entity<AgentSession>, cx: &mut TestAppContext) -> String {
        session.read_with(cx, |session, _| session.metadata().id.clone())
    }
}
