pub mod catalog;
mod discovery;
mod normalize;
mod wire;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SlashCommand,
    SteeringMode, UserInputAnswer, UserInputQuestion,
};

use crate::process::{Child, ChildStdin, Command, Stdio};
use crate::{Harness, HarnessError, RunControls, shutdown_child};
use catalog::{apply_ultrathink, to_effort};
use normalize::Normalizer;
use wire::{ControlRequestFrame, Frame, allow_response, control_response_line, deny_response};

pub const PERMISSION_ALLOW: &str = "Allow";
pub const PERMISSION_DENY: &str = "Deny";
pub const PERMISSION_HEADER: &str = "Permission";

fn permission_question(tool_name: &str, input: &Value) -> UserInputQuestion {
    let call = normalize::decode_tool_use(tool_name, input);
    let question = match &call {
        crate::ToolCall::Exec { command } => format!("Run `{command}`?"),
        crate::ToolCall::WriteFile { path, .. } => format!("Write {path}?"),
        crate::ToolCall::EditFile { path, .. } => format!("Edit {path}?"),
        crate::ToolCall::ReadFile { path } => format!("Read {path}?"),
        crate::ToolCall::WebFetch { url, .. } => format!("Fetch {url}?"),
        crate::ToolCall::WebSearch { query } => format!("Search the web for \"{query}\"?"),
        crate::ToolCall::Mcp { server, tool, .. } => format!("Use {server}: {tool}?"),
        _ => format!("Use {tool_name}?"),
    };
    UserInputQuestion {
        id: uuid::Uuid::new_v4().to_string(),
        header: PERMISSION_HEADER.into(),
        question,
        options: vec![PERMISSION_ALLOW.into(), PERMISSION_DENY.into()],
        multi_select: false,
        prefill: None,
        multiline: false,
    }
}

fn resolve_claude_executable() -> Option<PathBuf> {
    let mut extra = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        extra.push(home.join(".claude").join("local").join("claude"));
        extra.push(home.join(".local").join("bin").join("claude"));
    }
    extra.push(PathBuf::from("/opt/homebrew/bin/claude"));
    extra.push(PathBuf::from("/usr/local/bin/claude"));
    crate::executable::find_on_paths("claude", extra)
}

fn option_is_on(options: &serde_json::Map<String, Value>, key: &str) -> bool {
    match options.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "on" || s == "true",
        _ => false,
    }
}

pub struct ClaudeHarness {
    executable: Option<PathBuf>,
    interrupt_grace: Duration,
    kill_grace: Duration,
    initialize: discovery::InitializeCache,
    models_cache: crate::catalog::Catalog,
}

impl Default for ClaudeHarness {
    fn default() -> Self {
        Self {
            executable: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            initialize: discovery::InitializeCache::default(),
            models_cache: crate::catalog::Catalog::default(),
        }
    }
}

impl ClaudeHarness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    pub fn with_graces(mut self, interrupt_grace: Duration, kill_grace: Duration) -> Self {
        self.interrupt_grace = interrupt_grace;
        self.kill_grace = kill_grace;
        self
    }

    fn resolve_executable(&self) -> Result<PathBuf, HarnessError> {
        if let Some(p) = &self.executable {
            return crate::executable::validate_native_override(p);
        }
        if let Some(p) = std::env::var_os("CLAUDE_CODE_EXECUTABLE")
            && !p.is_empty()
        {
            return crate::executable::validate_native_override(&PathBuf::from(p));
        }
        resolve_claude_executable().ok_or_else(|| {
            HarnessError::NotInstalled(
                "claude (searched PATH, ~/.claude/local, \
                 ~/.local/bin, /opt/homebrew/bin, /usr/local/bin, and \
                 fnm/nvm/volta/pnpm/bun install dirs; Windows also checks USERPROFILE \
                 and explicit NVM_SYMLINK/VOLTA_HOME/PNPM_HOME; set \
                 CLAUDE_CODE_EXECUTABLE to override)"
                    .into(),
            )
        })
    }

    fn build_command(&self, exe: &PathBuf, request: &RunRequest) -> Command {
        let mut cmd = Command::new(exe);
        crate::compose_child_path(&mut cmd, exe);
        cmd.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            // The CLI rejects stream-json output without --verbose.
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
            // Newer models emit no thinking text unless a summary is requested.
            "--thinking-display",
            "summarized",
            // Undocumented: routes `can_use_tool` requests to the stdio control channel.
            "--permission-prompt-tool",
            "stdio",
        ]);
        if let Some(model) = &request.model {
            let one_m = request
                .model_options
                .get("contextWindow")
                .and_then(Value::as_str)
                == Some("1m");
            cmd.arg("--model");
            cmd.arg(if one_m {
                format!("{model}[1m]")
            } else {
                model.clone()
            });
        }
        if let Some(effort) = to_effort(request.reasoning, request.model.as_deref()) {
            cmd.args(["--effort", effort]);
        }
        if request.auto_approve {
            cmd.args([
                "--permission-mode",
                "bypassPermissions",
                "--dangerously-skip-permissions",
            ]);
        } else {
            cmd.args(["--permission-mode", "default"]);
        }
        if let Some(resume) = &request.resume {
            cmd.arg(format!("--resume={resume}"));
        }
        let mut settings = serde_json::Map::new();
        if option_is_on(&request.model_options, "fastMode") {
            settings.insert("fastMode".into(), Value::Bool(true));
        }
        if option_is_on(&request.model_options, "thinking") {
            settings.insert("alwaysThinkingEnabled".into(), Value::Bool(true));
        }
        if request.reasoning == Some(ReasoningLevel::Ultracode) {
            settings.insert("ultracode".into(), Value::Bool(true));
        }
        if !settings.is_empty() {
            cmd.arg("--settings");
            cmd.arg(Value::Object(settings).to_string());
        }
        if !request.cwd.is_empty() {
            cmd.current_dir(&request.cwd);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    fn discovery_context(&self) -> Result<crate::ModelContext, HarnessError> {
        crate::model_context::context(self.id(), &self.resolve_executable()?, &[])
    }

    async fn initialize(&self) -> Result<Value, HarnessError> {
        self.initialize
            .get(
                || self.discovery_context().map(|context| context.key()),
                || self.probe_initialize(None),
            )
            .await
    }

    async fn probe_initialize(&self, cwd: Option<&std::path::Path>) -> Result<Value, HarnessError> {
        let exe = self.resolve_executable()?;
        let mut cmd = Command::new(&exe);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        crate::compose_child_path(&mut cmd, &exe);
        cmd.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            // The CLI rejects stream-json output without --verbose.
            "--verbose",
        ]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(crate::executable::binary_hint(&exe))
            } else {
                HarnessError::Io(e)
            }
        })?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            shutdown_child(&mut child, self.kill_grace).await;
            return Err(HarnessError::Protocol("claude child has no stdio".into()));
        };
        const PROBE_ID: &str = "wu-initialize-probe";
        let discovery = async {
            let request = serde_json::json!({
                "type": "control_request",
                "request_id": PROBE_ID,
                "request": { "subtype": "initialize" },
            });
            stdin
                .write_all(format!("{request}\n").as_bytes())
                .await
                .map_err(HarnessError::Io)?;
            stdin.flush().await.map_err(HarnessError::Io)?;
            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines.next_line().await.map_err(HarnessError::Io)? {
                let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if frame.get("type").and_then(Value::as_str) != Some("control_response") {
                    continue;
                }
                let response = frame.get("response").cloned().unwrap_or(Value::Null);
                if response.get("request_id").and_then(Value::as_str) != Some(PROBE_ID) {
                    continue;
                }
                if response.get("subtype").and_then(Value::as_str) == Some("error") {
                    let msg = response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("initialize control request failed");
                    return Err(HarnessError::Protocol(msg.into()));
                }
                return Ok(response);
            }
            Err(HarnessError::Protocol(
                "claude exited before answering the initialize control request".into(),
            ))
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("Claude initialize timed out".into())),
        }
    }
}

fn parse_initialize_commands(response: &Value) -> Vec<SlashCommand> {
    response
        .pointer("/response/commands")
        .and_then(Value::as_array)
        .map(|commands| commands.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|command| {
            let name = command.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            Some(SlashCommand {
                name: name.to_owned(),
                description: command
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input_hint: command
                    .get("argumentHint")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|hint| !hint.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect()
}

#[async_trait]
impl Harness for ClaudeHarness {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Claude Code"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::XHigh,
            ReasoningLevel::Max,
        ]
    }
    fn installed(&self) -> bool {
        // Must match launch resolution, including CLAUDE_CODE_EXECUTABLE.
        self.resolve_executable().is_ok()
    }
    fn executable_path(&self) -> Option<PathBuf> {
        self.resolve_executable().ok()
    }
    fn model_context(&self) -> Result<Option<crate::ModelContext>, HarnessError> {
        self.discovery_context().map(Some)
    }
    fn fallback_models(&self) -> Vec<Model> {
        catalog::configured_models()
    }
    async fn model_catalog(&self, force: bool) -> Result<crate::ModelCatalog, HarnessError> {
        self.discovery_context()?.log();
        self.models_cache
            .get_with_timeout(
                force,
                Duration::from_secs(35),
                || self.discovery_context().map(|context| context.key()),
                || async {
                    let response = self.initialize().await?;
                    catalog::with_discovered_models(catalog::configured_models(), &response)
                },
            )
            .await
    }
    async fn commands(&self, cwd: &std::path::Path) -> Result<Vec<SlashCommand>, HarnessError> {
        let response = self.probe_initialize(Some(cwd)).await?;
        Ok(parse_initialize_commands(&response))
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_executable()?;
        match self.model_catalog(false).await {
            Ok(catalog) => Ok(catalog.models),
            Err(error) => {
                tracing::warn!(%error, source = "static", "Claude model discovery failed");
                Ok(self.fallback_models())
            }
        }
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.run_with_mode(request, controls).await
    }
}

impl ClaudeHarness {
    async fn run_with_mode(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let exe = self.resolve_executable()?;
        let mut cmd = self.build_command(&exe, &request);
        let normalizer = if let Some(session_id) = &request.resume {
            let config = std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| crate::executable::home_or_current_dir().join(".claude"));
            Normalizer::for_resume(&config, session_id).await
        } else {
            Normalizer::new()
        };
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(crate::executable::binary_hint(&exe))
            } else {
                HarnessError::Io(e)
            }
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("claude child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("claude child has no stdout".into()))?;
        let stderr_tail = crate::StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    log::debug!("stderr: {line}");
                    tail.push(&line);
                }
            });
        }

        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<StdinMsg>();
        tokio::spawn(stdin_writer(stdin, stdin_rx));

        let (images, image_problems) = load_image_blocks(&request.attachments).await;
        let first = wire::user_message_line(
            &apply_ultrathink(request.reasoning, &request.prompt),
            &images,
        );
        stdin_tx.send(StdinMsg::Line(first)).ok();

        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        for message in image_problems {
            if event_tx.try_send(Ok(AgentEvent::Error { message })).is_err() {
                log::debug!("claude run consumer gone before an image problem was reported");
            }
        }
        tokio::spawn(run_session(Session {
            normalizer,
            auto_approve: request.auto_approve,
            child,
            stdout_lines: BufReader::new(stdout).lines(),
            stdin_tx,
            event_tx,
            controls,
            reasoning: request.reasoning,
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            stderr_tail,
        }));

        Ok(futures::stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        })
        .boxed())
    }
}

enum StdinMsg {
    Line(String),
    Close,
}

// The API rejects larger inline images.
const MAX_INLINE_IMAGE_BYTES: usize = 5 * 1024 * 1024;

fn image_media_type(path: &std::path::Path, bytes: &[u8]) -> Option<&'static str> {
    let by_extension = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    };
    let by_content = match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, rest @ ..] if rest.starts_with(b"WEBP") => {
            Some("image/webp")
        }
        _ => None,
    };
    by_content.or(by_extension)
}

async fn load_image_blocks(paths: &[String]) -> (Vec<wire::ImageBlock>, Vec<String>) {
    use base64::Engine as _;
    let mut blocks = Vec::new();
    let mut problems = Vec::new();
    for path in paths {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                problems.push(format!("{name} wasn't sent: {error}"));
                continue;
            }
        };
        if bytes.len().div_ceil(3) * 4 > MAX_INLINE_IMAGE_BYTES {
            problems.push(format!("{name} wasn't sent: Claude only accepts images up to 5 MB."));
            continue;
        }
        let Some(media_type) = image_media_type(std::path::Path::new(path), &bytes) else {
            problems.push(format!(
                "{name} wasn't sent: only PNG, JPEG, GIF and WebP images are supported."
            ));
            continue;
        };
        blocks.push(wire::ImageBlock {
            media_type,
            base64_data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
    }
    (blocks, problems)
}

async fn stdin_writer(mut stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<StdinMsg>) {
    while let Some(msg) = rx.recv().await {
        match msg {
            StdinMsg::Line(line) => {
                let write = async {
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await
                };
                if let Err(e) = write.await {
                    log::debug!("stdin write failed (tolerated): {e}");
                    return;
                }
            }
            StdinMsg::Close => {
                if let Err(error) = stdin.shutdown().await {
                    log::debug!("claude stdin shutdown failed: {error}");
                }
                return;
            }
        }
    }
}

struct Session {
    normalizer: Normalizer,
    auto_approve: bool,
    child: Child,
    stdout_lines: tokio::io::Lines<BufReader<crate::process::ChildStdout>>,
    stdin_tx: mpsc::UnboundedSender<StdinMsg>,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    controls: RunControls,
    reasoning: Option<ReasoningLevel>,
    interrupt_grace: Duration,
    kill_grace: Duration,
    stderr_tail: crate::StderrTail,
}

async fn run_session(session: Session) {
    let Session {
        normalizer: mut norm,
        auto_approve,
        mut child,
        mut stdout_lines,
        stdin_tx,
        event_tx,
        controls,
        reasoning,
        interrupt_grace,
        kill_grace,
        stderr_tail,
    } = session;
    let RunControls {
        request_input,
        mut steering,
        interrupt,
    } = controls;
    let request_input = Arc::new(request_input);

    let mut pending_steers = std::collections::VecDeque::new();
    let mut open_tools = std::collections::HashSet::new();
    let mut steering_open = true;
    let mut interrupted = false;
    let mut interrupt_sent = false;
    let mut any_done = false;
    // Steers can be absorbed without a replay, so a held turn end must time out.
    const HELD_DONE_SETTLE: Duration = Duration::from_secs(5);
    let mut held_done: Option<(AgentEvent, tokio::time::Instant)> = None;
    let mut done_after_interrupt = false;
    let mut escalation: Option<tokio::task::JoinHandle<()>> = None;

    'main: loop {
        tokio::select! {
            line = stdout_lines.next_line() => match line {
                Ok(Some(line)) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Some((_, deadline)) = held_done.as_mut() {
                        *deadline = tokio::time::Instant::now() + HELD_DONE_SETTLE;
                    }
                    let frame = match wire::parse_frame(line) {
                        Ok(frame) => frame,
                        Err(e) => {
                            log::debug!("unparseable frame (skipped): {e}");
                            continue;
                        }
                    };
                    if let Frame::ControlRequest(req) = frame {
                        handle_control_request(req, auto_approve, &request_input, &stdin_tx);
                        continue;
                    }
                    if let Frame::User(ref user) = frame {
                        // Superseded steers are never replayed, so a replay confirms every earlier steer.
                        if user.parent_tool_use_id.is_none()
                            && let Some(at) = user
                                .uuid
                                .as_ref()
                                .and_then(|id| pending_steers.iter().position(|p| p == id))
                        {
                            for _ in 0..=at {
                                pending_steers.pop_front();
                                let (prev, next) = norm.rotate_for_steer();
                                if event_tx.send(Ok(AgentEvent::Steered {
                                    assistant_message_id: Some(prev), next_assistant_message_id: Some(next),
                                })).await.is_err() { break 'main; }
                            }
                        }
                    }
                    for ev in norm.normalize(frame, interrupted) {
                        match &ev {
                            AgentEvent::ToolCall { id, .. } => {
                                open_tools.insert(id.clone());
                            }
                            AgentEvent::ToolResult { id, .. } => {
                                open_tools.remove(id);
                            }
                            AgentEvent::Done { .. } => open_tools.clear(),
                            _ => {}
                        }
                        let is_done = matches!(ev, AgentEvent::Done { .. });
                        // A `now` steer ends the turn it interrupts with a result frame.
                        if is_done && !interrupted && !pending_steers.is_empty() {
                            held_done =
                                Some((ev, tokio::time::Instant::now() + HELD_DONE_SETTLE));
                            continue;
                        }
                        if is_done {
                            held_done = None;
                        }
                        if event_tx.send(Ok(ev)).await.is_err() {
                            break 'main;
                        }
                        if is_done {
                            any_done = true;
                            if interrupted {
                                done_after_interrupt = true;
                                break 'main;
                            }
                        }
                    }
                }
                Ok(None) => break 'main,
                Err(e) => {
                    if event_tx.send(Err(HarnessError::Io(e))).await.is_err() {
                        log::debug!("claude run consumer gone before a stdout error was delivered");
                    }
                    break 'main;
                }
            },

            steer = steering.recv(), if steering_open && !interrupted => match steer {
                Some(msg) => {
                    let id = uuid::Uuid::new_v4().to_string();
                    let (images, image_problems) = load_image_blocks(&msg.attachments).await;
                    for message in image_problems {
                        if event_tx.send(Ok(AgentEvent::Error { message })).await.is_err() {
                            break 'main;
                        }
                    }
                    let line = wire::steer_message_line(
                        &apply_ultrathink(reasoning, &msg.prompt),
                        &images,
                        &id,
                        open_tools.is_empty(),
                    );
                    pending_steers.push_back(id);
                    if stdin_tx.send(StdinMsg::Line(line)).is_err() { break 'main; }
                }
                None => {
                    steering_open = false;
                    if stdin_tx.send(StdinMsg::Close).is_err() {
                        log::debug!("claude stdin writer gone before stdin was closed");
                    }
                }
            },

            _ = interrupt.cancelled(), if !interrupt_sent => {
                interrupt_sent = true;
                interrupted = true;
                if stdin_tx.send(StdinMsg::Line(wire::interrupt_request_line("int_1"))).is_err() {
                    log::debug!("claude stdin closed before the interrupt was sent");
                }
                escalation = crate::process::escalate_interrupt(&child, interrupt_grace, kill_grace);
            },

            _ = tokio::time::sleep_until(
                held_done.as_ref().map_or_else(tokio::time::Instant::now, |(_, d)| *d)
            ), if held_done.is_some() => {
                while pending_steers.pop_front().is_some() {
                    let (prev, next) = norm.rotate_for_steer();
                    if event_tx.send(Ok(AgentEvent::Steered {
                        assistant_message_id: Some(prev), next_assistant_message_id: Some(next),
                    })).await.is_err() { break 'main; }
                }
                if let Some((done, _)) = held_done.take() {
                    if event_tx.send(Ok(done)).await.is_err() {
                        break 'main;
                    }
                    any_done = true;
                }
            },

            _ = event_tx.closed() => break 'main,
        }
    }

    if let Some((done, _)) = held_done.take()
        && !event_tx.is_closed()
        && event_tx.send(Ok(done)).await.is_ok()
    {
        any_done = true;
    }

    if !event_tx.is_closed() {
        if interrupted && !done_after_interrupt {
            if event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: norm.session_id.clone(),
                }))
                .await
                .is_err()
            {
                log::debug!("claude run consumer gone before the interrupted Done was delivered");
            }
        } else if !interrupted && !any_done {
            let status = child.try_wait().ok().flatten();
            if event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crate::crash_message("claude", status, &stderr_tail)),
                    session_id: norm.session_id.clone(),
                }))
                .await
                .is_err()
            {
                log::debug!("claude run consumer gone before the crash Done was delivered");
            }
        }
    }

    shutdown_child(&mut child, kill_grace).await;
    if let Some(handle) = escalation {
        handle.abort();
    }
}

type RequestInputFn = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

/// The CLI blocks until every `can_use_tool` request gets a response.
fn handle_control_request(
    req: ControlRequestFrame,
    auto_approve: bool,
    request_input: &Arc<RequestInputFn>,
    stdin_tx: &mpsc::UnboundedSender<StdinMsg>,
) {
    if req.request.subtype != "can_use_tool" {
        log::debug!("unhandled control_request subtype: {}", req.request.subtype);
        return;
    }
    if req.request.tool_name != "AskUserQuestion" {
        if auto_approve {
            let line = control_response_line(&req.request_id, allow_response(req.request.input));
            stdin_tx.send(StdinMsg::Line(line)).ok();
            return;
        }
        let request_input = Arc::clone(request_input);
        let stdin_tx = stdin_tx.clone();
        tokio::spawn(async move {
            let question = permission_question(&req.request.tool_name, &req.request.input);
            // A dropped sender must deny, never allow.
            let answers = (request_input)(vec![question.clone()])
                .await
                .unwrap_or_default();
            let accept = answers.iter().any(|answer| {
                answer.question_id == question.id
                    && answer
                        .labels
                        .iter()
                        .any(|label| label.eq_ignore_ascii_case(PERMISSION_ALLOW))
            });
            let response = if accept {
                allow_response(req.request.input)
            } else {
                deny_response("The user declined this action.")
            };
            let line = control_response_line(&req.request_id, response);
            stdin_tx.send(StdinMsg::Line(line)).ok();
        });
        return;
    }
    let request_input = Arc::clone(request_input);
    let stdin_tx = stdin_tx.clone();
    tokio::spawn(async move {
        let request_id = req.request_id;
        let input = req.request.input;
        let questions = parse_questions(&input);
        // Don't emit InputRequested here: the engine's input bridge owns that lifecycle.
        let answers = (request_input)(questions.clone()).await.unwrap_or_default();
        let updated = updated_input_with_answers(&input, &questions, &answers);
        let line = control_response_line(&request_id, allow_response(updated));
        if stdin_tx.send(StdinMsg::Line(line)).is_err() {
            log::debug!("claude stdin writer gone before the AskUserQuestion answer was sent");
        }
    });
}

fn parse_questions(input: &Value) -> Vec<UserInputQuestion> {
    let raw = input.get("questions").and_then(Value::as_array);
    raw.map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|q| {
            let field =
                |keys: [&str; 2]| keys.iter().find_map(|k| q.get(*k).and_then(Value::as_str));
            UserInputQuestion {
                id: uuid::Uuid::new_v4().to_string(),
                header: field(["header", "title"]).unwrap_or("Question").into(),
                question: field(["question", "prompt"]).unwrap_or("").into(),
                prefill: None,
                multiline: false,
                multi_select: ["multiSelect", "multi_select"]
                    .iter()
                    .find_map(|k| q.get(*k).and_then(Value::as_bool))
                    .unwrap_or(false),
                options: q
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| a.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|op| match op {
                        Value::String(s) => s.clone(),
                        other => other
                            .get("label")
                            .or_else(|| other.get("value"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                    })
                    .collect(),
            }
        })
        .collect()
}

fn updated_input_with_answers(
    input: &Value,
    questions: &[UserInputQuestion],
    answers: &[UserInputAnswer],
) -> Value {
    let mut updated = match input {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    let mut by_question = serde_json::Map::new();
    for q in questions {
        let labels: Vec<String> = answers
            .iter()
            .find(|a| a.question_id == q.id)
            .map(|a| a.labels.clone())
            .unwrap_or_default();
        let value = if q.multi_select {
            Value::Array(labels.into_iter().map(Value::String).collect())
        } else {
            Value::String(labels.into_iter().next().unwrap_or_default())
        };
        by_question.insert(q.question.clone(), value);
    }
    updated.insert("answers".into(), Value::Object(by_question));
    Value::Object(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_questions_tolerantly() {
        let input = json!({
            "questions": [
                {
                    "header": "Choice",
                    "question": "Pick one",
                    "options": ["A", {"label": "B", "description": "second"}],
                    "multiSelect": false
                },
                { "title": "Alt", "prompt": "Pick many", "multi_select": true }
            ]
        });
        let qs = parse_questions(&input);
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].header, "Choice");
        assert_eq!(qs[0].options, vec!["A".to_string(), "B".to_string()]);
        assert!(!qs[0].multi_select);
        assert_eq!(qs[1].header, "Alt");
        assert_eq!(qs[1].question, "Pick many");
        assert!(qs[1].multi_select);
    }

    #[test]
    fn answers_key_by_question_text() {
        let input =
            json!({"questions": [{"header": "H", "question": "Pick one", "options": ["A", "B"]}]});
        let qs = parse_questions(&input);
        let answers = vec![UserInputAnswer {
            question_id: qs[0].id.clone(),
            labels: vec!["B".into()],
        }];
        let updated = updated_input_with_answers(&input, &qs, &answers);
        assert_eq!(updated["answers"]["Pick one"], json!("B"));
        assert!(updated["questions"].is_array());
    }

    #[test]
    fn image_media_type_prefers_file_contents_over_extension() {
        let path = std::path::Path::new;
        assert_eq!(
            image_media_type(path("renamed.png"), b"\xFF\xD8\xFFjpeg"),
            Some("image/jpeg")
        );
    }

    #[test]
    fn image_media_type_falls_back_to_extension_then_magic_bytes() {
        let path = std::path::Path::new;
        assert_eq!(image_media_type(path("a.PNG"), b""), Some("image/png"));
        assert_eq!(image_media_type(path("a.jpg"), b""), Some("image/jpeg"));
        assert_eq!(image_media_type(path("a.gif"), b""), Some("image/gif"));
        assert_eq!(image_media_type(path("a.webp"), b""), Some("image/webp"));
        assert_eq!(
            image_media_type(path("paste"), b"\x89PNG\r\n"),
            Some("image/png")
        );
        assert_eq!(
            image_media_type(path("paste"), b"RIFF\0\0\0\0WEBPVP8"),
            Some("image/webp")
        );
        assert_eq!(image_media_type(path("a.svg"), b"<svg"), None);
    }
}
