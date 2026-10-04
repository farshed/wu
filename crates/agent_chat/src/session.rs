use agent_harness::{
    AgentEvent, CancellationToken, ClaudeHarness, CodexHarness, DoneStatus, Harness, HarnessId,
    Model, ReasoningLevel, RunControls, RunRequest, SandboxLevel, SteerMessage, ToolCall,
    UserInputAnswer, UserInputQuestion, usage::PlanUsage,
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
const TOOL_OUTPUT_MAX_CHARS: usize = 20_000;
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const USAGE_FORCED_MIN_INTERVAL: Duration = Duration::from_secs(30);

fn metadata_path(directory: &Path, id: &str) -> PathBuf {
    directory.join(format!("{id}.meta.json"))
}

fn entries_path(directory: &Path, id: &str) -> PathBuf {
    directory.join(format!("{id}.entries.json"))
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let directory = path
        .parent()
        .context("session file has no parent directory")?;
    std::fs::create_dir_all(directory)?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, contents)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
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
    fn harness_id(self) -> HarnessId {
        match self {
            AgentKind::Claude => HarnessId::ClaudeCode,
            AgentKind::Codex => HarnessId::Codex,
        }
    }

    fn new_harness(self) -> Arc<dyn Harness> {
        match self {
            AgentKind::Claude => Arc::new(ClaudeHarness::new()),
            AgentKind::Codex => Arc::new(CodexHarness::new()),
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
}

pub enum Entry {
    User { text: SharedString, at: i64 },
    Assistant { markdown: Entity<Markdown>, at: i64 },
    Thinking(Entity<Markdown>),
    Tool(ToolEntry),
    Notice { text: SharedString, is_error: bool },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SerializedEntry {
    User {
        text: String,
        #[serde(default)]
        at: i64,
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
    },
    Notice {
        text: String,
        is_error: bool,
    },
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
        self.questions
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
}

pub struct AgentSession {
    directory: Arc<Path>,
    working_since: Option<Instant>,
    run_failed: bool,
    harness: Arc<dyn Harness>,
    restart_on_next_send: bool,
    metadata: SessionMetadata,
    entries: Vec<Entry>,
    text_block_open: bool,
    working: bool,
    run: Option<ActiveRun>,
    pending_questions: Vec<PendingQuestion>,
    auto_approve: bool,
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
        entries: Vec<SerializedEntry>,
        languages: Arc<LanguageRegistry>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut session = Self {
            directory,
            working_since: None,
            run_failed: false,
            harness,
            restart_on_next_send: false,
            metadata,
            entries: Vec::new(),
            text_block_open: false,
            working: false,
            run: None,
            pending_questions: Vec::new(),
            auto_approve: false,
            languages,
            save_metadata_task: Task::ready(()),
            save_entries_task: Task::ready(()),
        };
        session.entries = entries
            .into_iter()
            .map(|entry| session.deserialize_entry(entry, cx))
            .collect();
        session
    }

    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    pub fn kind(&self) -> AgentKind {
        self.metadata.kind
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_working(&self) -> bool {
        self.working
    }

    /// When the current turn started, while one is running.
    pub fn working_since(&self) -> Option<Instant> {
        self.working.then_some(self.working_since).flatten()
    }

    pub fn run_failed(&self) -> bool {
        self.run_failed
    }

    pub fn pending_questions(&self) -> &[PendingQuestion] {
        &self.pending_questions
    }

    pub fn auto_approve(&self) -> bool {
        self.auto_approve
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
        let before = self.metadata.settings.clone();
        change(&mut self.metadata.settings);
        if self.metadata.settings == before {
            return;
        }
        self.restart_on_next_send = self.run.is_some();
        self.save_metadata(cx);
        cx.emit(SessionEvent::SettingsChanged);
        cx.notify();
    }

    pub fn send_message(&mut self, text: String, cx: &mut Context<Self>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.restart_on_next_send && !self.working {
            self.run = None;
            self.restart_on_next_send = false;
        }
        if self.metadata.title.is_empty() {
            self.metadata.title = title_from(&text);
        }
        self.entries.push(Entry::User {
            text: text.clone().into(),
            at: now(),
        });
        self.text_block_open = false;
        if !self.working {
            self.working_since = Some(Instant::now());
        }
        self.working = true;
        self.run_failed = false;
        self.touch(cx);

        if let Some(run) = &self.run {
            let steer = SteerMessage {
                prompt: text.clone(),
                message_id: None,
            };
            if run.steering.try_send(steer).is_ok() {
                cx.notify();
                return;
            }
            self.run = None;
        }
        self.start_run(text, cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
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
        if let Some(responder) = pending.responder.take()
            && responder.send(answers).is_err()
        {
            log::debug!("agent stopped waiting for an answer");
        }
        cx.notify();
    }

    pub fn set_auto_approve(&mut self, auto_approve: bool, cx: &mut Context<Self>) {
        self.auto_approve = auto_approve;
        if auto_approve {
            let waiting: Vec<String> = self
                .pending_questions
                .iter()
                .filter(|pending| pending.is_permission())
                .map(|pending| pending.id().to_string())
                .collect();
            for id in waiting {
                self.answer_permission(&id, true, cx);
            }
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

    fn start_run(&mut self, prompt: String, cx: &mut Context<Self>) {
        let harness = self.harness.clone();
        if self.metadata.native_session_id.is_none() {
            self.metadata.context = None;
        }
        let settings = self.metadata.settings.clone();
        let request = RunRequest {
            prompt,
            model: settings.model,
            reasoning: settings.reasoning,
            model_options: settings.options,
            cwd: self.metadata.cwd.to_string_lossy().into_owned(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: self.auto_approve,
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

        let run = gpui_tokio::Tokio::spawn(cx, async move { harness.run(request, controls).await });
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
        self.push_notice(message, true, cx);
        self.run_ended(cx);
    }

    fn run_ended(&mut self, cx: &mut Context<Self>) {
        self.run = None;
        self.working = false;
        self.pending_questions.clear();
        self.cancel_running_tools();
        self.save_entries(cx);
        cx.notify();
    }

    fn push_question(
        &mut self,
        questions: Vec<UserInputQuestion>,
        responder: tokio::sync::oneshot::Sender<Vec<UserInputAnswer>>,
        cx: &mut Context<Self>,
    ) {
        let pending = PendingQuestion {
            questions,
            responder: Some(responder),
        };
        let id = pending.id().to_string();
        let allow_now = self.auto_approve && pending.is_permission();
        self.pending_questions.push(pending);
        if allow_now {
            self.answer_permission(&id, true, cx);
        }
        cx.notify();
    }

    fn apply_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::SessionStarted { session_id, .. } => {
                if !session_id.is_empty() {
                    self.set_native_session_id(session_id, cx);
                }
            }
            AgentEvent::TextDelta { text } => self.append_text(&text, false, cx),
            AgentEvent::ReasoningDelta { text } => self.append_text(&text, true, cx),
            AgentEvent::AssistantMessageCompleted { .. } | AgentEvent::Steered { .. } => {
                self.text_block_open = false;
            }
            AgentEvent::ToolCall { id, call } => {
                self.text_block_open = false;
                if let Some(tool) = self.tool_mut(&id) {
                    tool.call = call;
                } else {
                    self.entries.push(Entry::Tool(ToolEntry {
                        id,
                        call,
                        status: ToolStatus::Running,
                        output: None,
                    }));
                }
                self.save_entries(cx);
            }
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                ..
            } => {
                if let Some(tool) = self.tool_mut(&id) {
                    tool.status = if is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Completed
                    };
                    tool.output = output.map(|output| truncate_output(output).into());
                }
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
                self.working = false;
                self.text_block_open = false;
                self.cancel_running_tools();
                match status {
                    DoneStatus::Completed => {}
                    DoneStatus::Interrupted => {}
                    DoneStatus::Errored => {
                        self.run_failed = true;
                        self.push_notice(
                            error.unwrap_or_else(|| "The agent stopped with an error.".into()),
                            true,
                            cx,
                        )
                    }
                }
                self.touch(cx);
            }
            AgentEvent::GeneratedImage { path, .. } => {
                self.push_notice(format!("Generated an image: {path}"), false, cx)
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
            | AgentEvent::UserMessage { .. }
            | AgentEvent::Subagent { .. } => {}
        }
        cx.notify();
    }

    fn append_text(&mut self, text: &str, thinking: bool, cx: &mut Context<Self>) {
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
            let markdown = self.new_markdown(text.to_string(), cx);
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
        self.save_entries(cx);
    }

    fn push_notice(&mut self, text: String, is_error: bool, cx: &mut Context<Self>) {
        self.text_block_open = false;
        self.entries.push(Entry::Notice {
            text: text.into(),
            is_error,
        });
        self.save_entries(cx);
        cx.notify();
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolEntry> {
        self.entries.iter_mut().rev().find_map(|entry| match entry {
            Entry::Tool(tool) if tool.id == id => Some(tool),
            _ => None,
        })
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

    fn new_markdown(&self, text: String, cx: &mut Context<Self>) -> Entity<Markdown> {
        let languages = self.languages.clone();
        cx.new(|cx| Markdown::new(text.into(), Some(languages), None, cx))
    }

    fn deserialize_entry(&self, entry: SerializedEntry, cx: &mut Context<Self>) -> Entry {
        match entry {
            SerializedEntry::User { text, at } => Entry::User {
                text: text.into(),
                at,
            },
            SerializedEntry::Assistant { text, at } => Entry::Assistant {
                markdown: self.new_markdown(text, cx),
                at,
            },
            SerializedEntry::Thinking { text } => Entry::Thinking(self.new_markdown(text, cx)),
            SerializedEntry::Tool {
                id,
                call,
                status,
                output,
            } => Entry::Tool(ToolEntry {
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
        }
    }

    fn serialize_entries(&self, cx: &App) -> Vec<SerializedEntry> {
        self.entries
            .iter()
            .map(|entry| match entry {
                Entry::User { text, at } => SerializedEntry::User {
                    text: text.to_string(),
                    at: *at,
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
                },
                Entry::Notice { text, is_error } => SerializedEntry::Notice {
                    text: text.to_string(),
                    is_error: *is_error,
                },
            })
            .collect()
    }

    fn save_metadata(&mut self, cx: &mut Context<Self>) {
        cx.emit(SessionEvent::MetadataChanged);
        let metadata = self.metadata.clone();
        let directory = self.directory.clone();
        self.save_metadata_task = cx.background_spawn(async move {
            let path = metadata_path(&directory, &metadata.id);
            serde_json::to_vec_pretty(&metadata)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
    }

    fn save_entries(&mut self, cx: &mut Context<Self>) {
        let directory = self.directory.clone();
        self.save_entries_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let Some((id, entries)) = this
                .read_with(cx, |this, cx| {
                    (this.metadata.id.clone(), this.serialize_entries(cx))
                })
                .log_err()
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
    pub updated_at: i64,
    pub working: bool,
    pub needs_input: bool,
}

struct GlobalAgentStore(Entity<AgentStore>);

impl Global for GlobalAgentStore {}

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
        store
    }

    fn new(directory: Arc<Path>, languages: Arc<LanguageRegistry>, cx: &mut Context<Self>) -> Self {
        let load = cx.spawn({
            let directory = directory.clone();
            async move |this, cx| {
                let defaults_file = defaults_path(&directory);
                let (sessions, defaults) = cx
                    .background_spawn(async move {
                        let defaults = std::fs::read(&defaults_file).ok().and_then(|json| {
                            serde_json::from_slice::<HashMap<AgentKind, RunSettings>>(&json)
                                .log_err()
                        });
                        (load_all_metadata(&directory), defaults)
                    })
                    .await;
                let sessions = sessions.log_err().unwrap_or_default();
                this.update(cx, |this, cx| {
                    if let Some(defaults) = defaults {
                        this.defaults = defaults;
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

    pub fn models(&self, kind: AgentKind) -> &[Model] {
        match self.catalogs.get(&kind) {
            Some(catalog) if !catalog.models.is_empty() => &catalog.models,
            Some(catalog) => &catalog.fallback,
            None => &[],
        }
    }

    pub fn ensure_models(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        let harness = self.harness(kind);
        let catalog = self.catalogs.entry(kind).or_default();
        if catalog.fallback.is_empty() {
            catalog.fallback = harness.fallback_models();
        }
        if catalog.loaded || catalog._task.is_some() {
            return;
        }
        let discovery = gpui_tokio::Tokio::spawn(cx, async move { harness.models().await });
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
                let catalog = this.catalogs.entry(kind).or_default();
                catalog.models = models;
                catalog.loaded = true;
                catalog._task = None;
                cx.notify();
            })
            .log_err();
        });
        if let Some(catalog) = self.catalogs.get_mut(&kind) {
            catalog._task = Some(task);
        }
    }

    pub fn plan_usage(&self, kind: AgentKind) -> Option<&PlanUsageState> {
        self.plan_usage.get(&kind)
    }

    pub fn refresh_plan_usage(&mut self, kind: AgentKind, force: bool, cx: &mut Context<Self>) {
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
        let request = gpui_tokio::Tokio::spawn(cx, async move {
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
        self.save_defaults_task = cx.background_spawn(async move {
            serde_json::to_vec_pretty(&defaults)
                .map_err(anyhow::Error::from)
                .and_then(|json| write_atomically(&path, &json))
                .log_err();
        });
    }

    pub fn sessions_in(&self, roots: &[PathBuf], cx: &App) -> Vec<SessionSummary> {
        self.sessions
            .iter()
            .filter(|metadata| roots.contains(&metadata.cwd))
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
                    updated_at: metadata.updated_at,
                    working: live.is_some_and(|session| session.is_working()),
                    needs_input: live.is_some_and(|session| !session.pending_questions.is_empty()),
                }
            })
            .collect()
    }

    pub fn create_session(
        &mut self,
        kind: AgentKind,
        cwd: PathBuf,
        cx: &mut Context<Self>,
    ) -> Entity<AgentSession> {
        let created_at = now();
        let metadata = SessionMetadata {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            title: String::new(),
            cwd,
            created_at,
            updated_at: created_at,
            native_session_id: None,
            settings: self.defaults.get(&kind).cloned().unwrap_or_default(),
            context: None,
        };
        let languages = self.languages.clone();
        let directory = self.directory.clone();
        let harness = self.harness(kind);
        let session = cx.new(|cx| {
            AgentSession::new(
                directory,
                harness,
                metadata.clone(),
                Vec::new(),
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
                        Ok(json) => serde_json::from_slice::<Vec<SerializedEntry>>(&json)
                            .context("failed to parse the chat transcript"),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            Ok(Vec::new())
                        }
                        Err(error) => Err(error).context("failed to read the chat transcript"),
                    }
                })
                .await?;
            this.update(cx, |this, cx| {
                if let Some(session) = this.live.get(&metadata.id) {
                    return session.clone();
                }
                let languages = this.languages.clone();
                let harness = this.harness(metadata.kind);
                let session = cx.new(|cx| {
                    AgentSession::new(directory, harness, metadata, entries, languages, cx)
                });
                this.track(session.clone(), cx);
                session
            })
        })
    }

    pub fn delete_session(&mut self, id: &str, cx: &mut Context<Self>) {
        self.live.remove(id);
        self.sessions.retain(|metadata| metadata.id != id);
        let id = id.to_string();
        let directory = self.directory.clone();
        cx.background_spawn(async move {
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
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn fake_claude() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../agent_harness/tests/fixtures/fake-claude.sh")
    }

    fn new_store(directory: &Path, cx: &mut TestAppContext) -> Entity<AgentStore> {
        cx.executor().allow_parking();
        // SAFETY: tests in this crate only ever set this variable to the same fixture.
        unsafe { std::env::set_var("CLAUDE_CODE_EXECUTABLE", fake_claude()) };
        cx.update(gpui_tokio::init);
        let languages = Arc::new(LanguageRegistry::test(cx.background_executor.clone()));
        cx.new(|cx| AgentStore::new(directory.into(), languages, cx))
    }

    fn run_until(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
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
    async fn claude_reply_streams_into_the_transcript_and_is_saved(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        session.update(cx, |session, cx| {
            session.send_message("scenario:happy".into(), cx)
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
            session.send_message("scenario:askuser".into(), cx)
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
            session.send_message("first".into(), cx);
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
            session.send_message("second".into(), cx);
        });
        run_until(cx, |cx| {
            session.read_with(cx, |session, _| !session.is_working())
                && last_reply(&session, cx) != first
        });
        let second = last_reply(&session, cx);
        assert!(second.contains("--effort low"), "{second}");
        assert!(second.contains("--resume=sess-args"), "{second}");
    }

    fn session_id(session: &Entity<AgentSession>, cx: &mut TestAppContext) -> String {
        session.read_with(cx, |session, _| session.metadata().id.clone())
    }
}
