pub mod catalog;
mod normalize;
mod subagents;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use crate::{
    AgentEvent, DoneStatus, ExternalSession, HarnessId, Model, ModelOption, ModelOptionChoice,
    PermissionMode, ReasoningLevel, RunRequest, Skill, SkillRef, SlashCommand, SteeringMode,
    UserInputAnswer, UserInputQuestion,
};

use crate::jsonrpc::{Incoming, RpcClient};
use crate::process::{Child, Command, Stdio};
use crate::{Harness, HarnessError, RunControls, SteerMessage};
use catalog::{REASONING_LEVELS, sandbox_mode, sandbox_policy_value, static_models, to_effort};
use normalize::{
    ChildRoute, Phase, ReasoningStream, delta_text, item_id, item_type, notification_thread_id,
    route_child_notification, turn_error_message, turn_id, usage_event,
};

pub fn resolve_codex_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CODEX_EXECUTABLE").filter(|p| !p.is_empty()) {
        return crate::executable::validate_native_override(&PathBuf::from(p)).ok();
    }
    let mut extra = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        extra.push(home.join(".local").join("bin").join("codex"));
        extra.push(home.join(".codex").join("bin").join("codex"));
        extra.push(home.join(".npm-global").join("bin").join("codex"));
    }
    extra.push(PathBuf::from("/opt/homebrew/bin/codex"));
    extra.push(PathBuf::from("/usr/local/bin/codex"));
    crate::executable::find_on_paths("codex", extra)
}

pub struct CodexHarness {
    models_cache: crate::catalog::Catalog,
    executable: Option<PathBuf>,
    interrupt_grace: Duration,
    kill_grace: Duration,
}

const MAX_LISTED_THREADS: usize = 500;

impl Default for CodexHarness {
    fn default() -> Self {
        Self {
            models_cache: crate::catalog::Catalog::default(),
            executable: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
        }
    }
}

impl CodexHarness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    /// `interrupt_grace` runs from `turn/interrupt` to SIGTERM, `kill_grace` from SIGTERM to SIGKILL.
    pub fn with_graces(mut self, interrupt_grace: Duration, kill_grace: Duration) -> Self {
        self.interrupt_grace = interrupt_grace;
        self.kill_grace = kill_grace;
        self
    }

    fn resolve_executable(&self) -> Result<PathBuf, HarnessError> {
        if let Some(p) = &self.executable {
            return crate::executable::validate_native_override(p);
        }
        if let Some(p) = std::env::var_os("CODEX_EXECUTABLE")
            && !p.is_empty()
        {
            return crate::executable::validate_native_override(&PathBuf::from(p));
        }
        resolve_codex_executable().ok_or_else(|| {
            HarnessError::NotInstalled(
                "codex (searched PATH, ~/.local/bin, \
                 ~/.codex/bin, ~/.npm-global/bin, /opt/homebrew/bin, /usr/local/bin, \
                 and fnm/nvm/volta/pnpm/bun install dirs; Windows also checks USERPROFILE \
                 and explicit NVM_SYMLINK/VOLTA_HOME/PNPM_HOME; set CODEX_EXECUTABLE \
                 to override)"
                    .into(),
            )
        })
    }

    fn current_model_context(&self) -> Result<crate::ModelContext, HarnessError> {
        crate::model_context::context(self.id(), &self.resolve_executable()?, &[])
    }

    async fn probe_app_server<T, F, Fut>(
        &self,
        cwd: Option<&std::path::Path>,
        timeout_message: &str,
        body: F,
    ) -> Result<T, HarnessError>
    where
        F: FnOnce(RpcClient, PathBuf) -> Fut,
        Fut: std::future::Future<Output = Result<T, HarnessError>>,
    {
        let exe = self.resolve_executable()?;
        let mut cmd = Command::new(&exe);
        cmd.arg("app-server");
        cmd.current_dir(cwd.map_or_else(crate::executable::scratch_dir, std::path::Path::to_path_buf));
        crate::compose_child_path(&mut cmd, &exe);
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
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            shutdown_child(&mut child, self.kill_grace).await;
            return Err(HarnessError::Protocol("codex child has no stdio".into()));
        };
        let (client, _incoming) = RpcClient::new(stdin, stdout);
        let probe = async {
            client
                .request(
                    "initialize",
                    json!({
                        "clientInfo": {
                            "name": "wu",
                            "title": "Wu",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                        "capabilities": { "experimentalApi": true },
                    }),
                )
                .await?;
            client.notify("initialized", None);
            body(client.clone(), exe).await
        };
        let result = tokio::time::timeout(Duration::from_secs(10), probe).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol(timeout_message.into())),
        }
    }

    async fn list_threads(
        &self,
        cwd: &std::path::Path,
    ) -> Result<Vec<ExternalSession>, HarnessError> {
        self.probe_app_server(
            Some(cwd),
            "chat listing timed out",
            |client, _exe| async move {
                let mut sessions = Vec::new();
                let mut seen_cursors = HashSet::new();
                let mut cursor: Option<String> = None;
                loop {
                    let mut params = json!({
                        "cwd": cwd,
                        "sortKey": "updated_at",
                        "limit": 100,
                        "sourceKinds": ["cli", "vscode", "exec", "appServer", "unknown"],
                    });
                    if let Some(cursor) = cursor.as_deref() {
                        params["cursor"] = Value::String(cursor.to_owned());
                    }
                    let page = client.request("thread/list", params).await?;
                    sessions.extend(normalize::external_sessions(&page));
                    match page.get("nextCursor").and_then(Value::as_str) {
                        Some(next)
                            if sessions.len() < MAX_LISTED_THREADS
                                && seen_cursors.insert(next.to_owned()) =>
                        {
                            cursor = Some(next.to_owned());
                        }
                        _ => return Ok(sessions),
                    }
                }
            },
        )
        .await
    }

    async fn discover_skills(&self, cwd: &std::path::Path) -> Result<Vec<Skill>, HarnessError> {
        self.probe_app_server(
            Some(cwd),
            "skill discovery timed out",
            |client, _exe| async move {
                let result = client
                    .request("skills/list", json!({ "cwds": [cwd], "forceReload": true }))
                    .await?;
                Ok(parse_skills(&result))
            },
        )
        .await
    }

    async fn discover_models(&self) -> Result<Vec<Model>, HarnessError> {
        self.probe_app_server(None, "model discovery timed out", |client, exe| async move {
            let mut models = Vec::new();
            let mut model_ids = HashSet::new();
            let mut seen_cursors = HashSet::new();
            let mut cursor: Option<String> = None;
            let mut default_model_id: Option<String> = None;
            loop {
                let mut params = json!({ "limit": 20, "includeHidden": false });
                if let Some(cursor) = cursor.as_deref() {
                    params["cursor"] = Value::String(cursor.to_owned());
                }
                let page = client.request("model/list", params).await?;
                if legacy_model_page(&page) {
                    tracing::warn!(binary_path = %exe.display(), binary_version = ?crate::executable::binary_version(&exe), "Model discovery response lacks hidden flags; CLI may be outdated");
                }
                let (page_models, next_cursor) = parse_model_list_page(&page);
                for (model, is_default) in page_models {
                    if model_ids.insert(model.id.clone()) {
                        if is_default && default_model_id.is_none() {
                            default_model_id = Some(model.id.clone());
                        }
                        models.push(model);
                    }
                }
                let Some(next) = next_cursor.filter(|next| !next.is_empty()) else {
                    break;
                };
                if !seen_cursors.insert(next.clone()) {
                    break;
                }
                cursor = Some(next);
            }

            if let Some(default_id) = default_model_id
                && let Some(index) = models.iter().position(|model| model.id == default_id)
                && index != 0
            {
                let default_model = models.remove(index);
                models.insert(0, default_model);
            }
            if models.is_empty() {
                return Err(crate::CatalogFailure {
                    code: crate::CatalogFailureCode::Failed,
                    message: "Codex returned an empty model catalog".into(),
                }
                .into());
            }
            Ok(models)
        })
        .await
    }
}

fn reasoning_level(value: &str) -> Option<ReasoningLevel> {
    Some(match value {
        "minimal" => ReasoningLevel::Minimal,
        "low" => ReasoningLevel::Low,
        "medium" => ReasoningLevel::Medium,
        "high" => ReasoningLevel::High,
        "xhigh" => ReasoningLevel::XHigh,
        "max" => ReasoningLevel::Max,
        "ultra" => ReasoningLevel::Ultra,
        "ultracode" => ReasoningLevel::Ultracode,
        "ultrathink" => ReasoningLevel::Ultrathink,
        _ => return None,
    })
}

/// Codex accepts `priority` and `fast` for the same tier; saved settings use `fast`.
fn normalized_service_tier(value: &str) -> &str {
    match value {
        "priority" => "fast",
        other => other,
    }
}

fn service_tier_label(value: &str) -> String {
    match value {
        "fast" | "priority" => "Fast".into(),
        "flex" => "Flex".into(),
        "ultrafast" => "Ultra Fast".into(),
        other => other.to_owned(),
    }
}

fn model_service_tier(item: &Value) -> Option<ModelOption> {
    let mut choices = vec![ModelOptionChoice {
        id: "default".into(),
        label: "Standard".into(),
    }];
    let mut seen = HashSet::from(["default".to_owned()]);
    for tier in item
        .get("serviceTiers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(wire_id) = tier.get("id").and_then(Value::as_str) else {
            continue;
        };
        let id = normalized_service_tier(wire_id).to_owned();
        if !seen.insert(id.clone()) {
            continue;
        }
        let label = tier
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| service_tier_label(wire_id));
        choices.push(ModelOptionChoice { id, label });
    }
    for tier in item
        .get("additionalSpeedTiers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(wire_id) = tier.as_str() else {
            continue;
        };
        let id = normalized_service_tier(wire_id).to_owned();
        if seen.insert(id.clone()) {
            choices.push(ModelOptionChoice {
                id,
                label: service_tier_label(wire_id),
            });
        }
    }
    if choices.len() == 1 {
        return None;
    }
    let default_choice = item
        .get("defaultServiceTier")
        .and_then(Value::as_str)
        .map(normalized_service_tier)
        .filter(|id| seen.contains(*id))
        .unwrap_or("default")
        .to_owned();
    Some(ModelOption {
        id: "serviceTier".into(),
        label: "Service Tier".into(),
        choices,
        default_choice,
    })
}

fn legacy_model_page(page: &Value) -> bool {
    page.get("data")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            !items.is_empty() && items.iter().all(|item| item.get("hidden").is_none())
        })
}

fn parse_model_list_page(result: &Value) -> (Vec<(Model, bool)>, Option<String>) {
    let mut models = Vec::new();
    for item in result
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if item.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(id) = item
            .get("model")
            .and_then(Value::as_str)
            .or_else(|| item.get("id").and_then(Value::as_str))
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let label = item
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or(id)
            .to_owned();
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_owned);
        let description = match item
            .get("upgrade")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            Some(upgrade) => Some(format!(
                "{}(upgrade: {upgrade})",
                description.map(|d| format!("{d} ")).unwrap_or_default()
            )),
            None => description,
        };
        let reasoning_levels = item
            .get("supportedReasoningEfforts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|effort| {
                effort
                    .get("reasoningEffort")
                    .and_then(Value::as_str)
                    .or_else(|| effort.as_str())
                    .and_then(reasoning_level)
            })
            .collect();
        let options = model_service_tier(item).into_iter().collect();
        models.push((
            Model {
                id: id.to_owned(),
                label,
                description,
                reasoning_levels,
                options,
            },
            item.get("isDefault").and_then(Value::as_bool) == Some(true),
        ));
    }
    let next_cursor = result
        .get("nextCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    (models, next_cursor)
}

#[async_trait]
impl Harness for CodexHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Codex
    }
    fn display_name(&self) -> &str {
        // Must match the registry's lazy descriptor so the catalog entry stays stable.
        "Codex"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        REASONING_LEVELS
    }
    fn installed(&self) -> bool {
        self.resolve_executable().is_ok()
    }
    fn executable_path(&self) -> Option<PathBuf> {
        self.resolve_executable().ok()
    }
    fn model_context(&self) -> Result<Option<crate::ModelContext>, HarnessError> {
        self.current_model_context().map(Some)
    }
    fn fallback_models(&self) -> Vec<Model> {
        static_models()
    }
    async fn model_catalog(&self, force: bool) -> Result<crate::ModelCatalog, HarnessError> {
        self.current_model_context()?.log();
        self.models_cache
            .get_with(
                force,
                || self.current_model_context().map(|context| context.key()),
                || self.discover_models(),
            )
            .await
    }
    async fn commands(&self, _cwd: &std::path::Path) -> Result<Vec<SlashCommand>, HarnessError> {
        Ok(vec![
            SlashCommand {
                name: "compact".into(),
                description: "Compact this conversation's context".into(),
                input_hint: None,
            },
            SlashCommand {
                name: "review".into(),
                description: "Review uncommitted changes, or supply review instructions".into(),
                input_hint: Some("optional instructions".into()),
            },
        ])
    }

    async fn skills(&self, cwd: &std::path::Path) -> Result<Vec<Skill>, HarnessError> {
        self.discover_skills(cwd).await
    }

    async fn external_sessions(
        &self,
        cwd: &std::path::Path,
    ) -> Result<Vec<ExternalSession>, HarnessError> {
        self.list_threads(cwd).await
    }

    async fn external_history(
        &self,
        cwd: &std::path::Path,
        session_id: &str,
    ) -> Result<Vec<AgentEvent>, HarnessError> {
        self.probe_app_server(
            Some(cwd),
            "reading the chat timed out",
            |client, _exe| async move {
                let read = client
                    .request(
                        "thread/read",
                        json!({ "threadId": session_id, "includeTurns": true }),
                    )
                    .await?;
                Ok(normalize::history_events(&read["thread"]))
            },
        )
        .await
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_executable()?;
        match self.model_catalog(false).await {
            Ok(catalog) => Ok(catalog.models),
            Err(error) if !crate::CatalogFailure::classify(&error).allows_stale() => Err(error),
            Err(error) => {
                tracing::warn!(%error, source = "static", "Model discovery failed");
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

impl CodexHarness {
    async fn run_with_mode(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let native = command_request(&request.prompt, "")?;
        if native
            .as_ref()
            .is_some_and(|(method, _)| *method == "thread/compact/start")
            && request.resume.is_none()
            && request.fork.is_none()
        {
            return Err(HarnessError::Protocol(
                "/compact needs an existing Codex conversation".into(),
            ));
        }
        if native.is_some() && (!request.attachments.is_empty() || !request.skills.is_empty()) {
            return Err(command_with_extras_error());
        }
        let exe = self.resolve_executable()?;
        let mut cmd = Command::new(&exe);
        cmd.arg("app-server");
        crate::compose_child_path(&mut cmd, &exe);
        if request.cwd.is_empty() {
            cmd.current_dir(crate::executable::scratch_dir());
        } else {
            cmd.current_dir(&request.cwd);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
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
            .ok_or_else(|| HarnessError::Protocol("codex child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("codex child has no stdout".into()))?;
        let stderr_tail = crate::StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "agent_harness::codex", "stderr: {line}");
                    tail.push(&line);
                }
            });
        }

        let (client, incoming) = RpcClient::new(stdin, stdout);
        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        tokio::spawn(run_session(Session {
            child,
            client,
            incoming,
            event_tx,
            controls,
            request,
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

struct Session {
    child: Child,
    client: RpcClient,
    incoming: mpsc::Receiver<Incoming>,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    controls: RunControls,
    request: RunRequest,
    interrupt_grace: Duration,
    kill_grace: Duration,
    stderr_tail: crate::StderrTail,
}

/// The `turn/start` response and turn notifications can arrive in either order.
#[derive(Default)]
struct TurnRouter {
    active: Option<String>,
    completed: VecDeque<String>,
}

impl TurnRouter {
    fn is_completed(&self, id: &str) -> bool {
        self.completed.iter().any(|c| c == id)
    }

    fn note_started(&mut self, id: String) {
        if id.is_empty() || self.is_completed(&id) {
            return;
        }
        // A new turn means the old one is over even if its completion was lost.
        if let Some(prev) = self.active.take()
            && prev != id
        {
            self.remember_completed(prev);
        }
        self.active = Some(id);
    }

    fn note_completed(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.remember_completed(id.to_owned());
        if self.active.as_deref() == Some(id) {
            self.active = None;
        }
    }

    fn adopt_started(&mut self, id: String) {
        self.active = (!id.is_empty() && !self.is_completed(&id)).then_some(id);
    }

    fn remember_completed(&mut self, id: String) {
        self.completed.push_back(id);
        while self.completed.len() > 32 {
            self.completed.pop_front();
        }
    }
}

fn new_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn rotate(id: &mut String) -> (String, String) {
    let prev = std::mem::replace(id, new_message_id());
    (prev, id.clone())
}

async fn send(tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>, ev: AgentEvent) -> bool {
    tx.send(Ok(ev)).await.is_ok()
}

async fn send_final(tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>, ev: AgentEvent) {
    if !send(tx, ev).await {
        tracing::debug!(target: "agent_harness::codex", "event receiver gone, dropping final event");
    }
}

// The text item must stay first: `start_turn` routes commands from `/input/0/text`.
fn prompt_input(text: &str, attachments: &[String], skills: &[SkillRef]) -> Value {
    let mut input = vec![json!({"type": "text", "text": text})];
    let mut seen = HashSet::new();
    for skill in skills {
        if seen.insert(skill) {
            input.push(json!({"type": "skill", "name": skill.name, "path": skill.path}));
        }
    }
    for path in attachments {
        input.push(json!({"type": "localImage", "path": path}));
    }
    Value::Array(input)
}

fn parse_skills(result: &Value) -> Vec<Skill> {
    let mut seen = HashSet::new();
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|group| {
            group
                .get("skills")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|skill| {
            let name = skill.get("name")?.as_str()?;
            let path = skill
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty() && !path.chars().any(char::is_control));
            if name.is_empty()
                || name.chars().any(|c| c.is_control() || c.is_whitespace())
                || !seen.insert((name.to_owned(), path.map(str::to_owned)))
            {
                return None;
            }
            Some(Skill {
                name: name.to_owned(),
                path: path.map(str::to_owned),
                description: skill
                    .pointer("/interface/shortDescription")
                    .and_then(Value::as_str)
                    .or_else(|| skill.get("description").and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_owned(),
                enabled: skill
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            })
        })
        .collect()
}

fn command_request(
    text: &str,
    thread_id: &str,
) -> Result<Option<(&'static str, Value)>, HarnessError> {
    let Some((name, args)) = crate::leading_command(text) else {
        return Ok(None);
    };
    match name {
        "compact" if args.is_empty() => Ok(Some((
            "thread/compact/start",
            json!({"threadId": thread_id}),
        ))),
        "compact" => Err(HarnessError::Protocol(
            "/compact takes no arguments; send other text separately".into(),
        )),
        "review" => Ok(Some((
            "review/start",
            json!({
                "threadId": thread_id, "delivery": "inline",
                "target": if args.is_empty() { json!({"type":"uncommittedChanges"}) }
                    else { json!({"type":"custom", "instructions":args}) },
            }),
        ))),
        "model" | "permissions" | "approvals" | "new" | "clear" | "resume" | "fork" | "status"
        | "diff" | "mention" | "mcp" | "skills" | "plan" | "fast" | "logout" | "quit" | "exit"
        | "init" | "rename" | "feedback" | "ps" | "stop" | "clean" | "archive" | "delete" => {
            Err(HarnessError::Protocol(format!(
                "/{name} is not available here. Available commands: /compact and /review."
            )))
        }
        _ => Ok(None),
    }
}

fn command_with_extras_error() -> HarnessError {
    HarnessError::Protocol(
        "Codex commands cannot include skills or attachments; send them in a separate prompt"
            .into(),
    )
}

async fn start_turn(client: &RpcClient, params: Value) -> Result<String, HarnessError> {
    let text = params
        .pointer("/input/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thread_id = params["threadId"].as_str().unwrap_or_default();
    let native = command_request(text, thread_id)?;
    if native.is_some()
        && params["input"]
            .as_array()
            .is_some_and(|input| input.len() > 1)
    {
        return Err(command_with_extras_error());
    }
    let (method, params) = native.unwrap_or(("turn/start", params));
    let started = client.request(method, params).await?;
    Ok(started["turn"]["id"].as_str().unwrap_or("").to_owned())
}

async fn run_session(session: Session) {
    let Session {
        mut child,
        client,
        mut incoming,
        event_tx,
        controls,
        request,
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

    let permission = request.permission.for_harness(HarnessId::Codex);
    let sandbox = permission_sandbox(permission);
    let approval_policy = if permission.skips_prompts() {
        "never"
    } else {
        "on-request"
    };
    let approvals_reviewer = (permission == PermissionMode::ApproveForMe).then_some("auto_review");
    let effort = to_effort(request.reasoning);
    let service_tier = request
        .model_options
        .get("serviceTier")
        .and_then(Value::as_str)
        .filter(|t| *t != "default")
        .map(str::to_owned);

    let start_params = {
        let mut p = serde_json::Map::new();
        p.insert("cwd".into(), Value::String(request.cwd.clone()));
        p.insert("approvalPolicy".into(), approval_policy.into());
        if let Some(reviewer) = approvals_reviewer {
            p.insert("approvalsReviewer".into(), reviewer.into());
        }
        p.insert("sandbox".into(), sandbox_mode(sandbox).into());
        if let Some(model) = &request.model {
            p.insert("model".into(), Value::String(model.clone()));
        }
        if let Some(tier) = &service_tier {
            p.insert("serviceTier".into(), Value::String(tier.clone()));
        }
        p
    };

    let setup = async {
        client
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "wu",
                        "title": "Wu",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": { "experimentalApi": true },
                }),
            )
            .await?;
        client.notify("initialized", None);

        let thread = if let Some(resume) = &request.resume {
            let mut p = start_params.clone();
            p.insert("threadId".into(), Value::String(resume.clone()));
            match client.request("thread/resume", Value::Object(p)).await {
                Ok(thread) => thread,
                Err(e) => {
                    if command_request(&request.prompt, resume)?.is_some() {
                        return Err(e);
                    }
                    tracing::debug!(
                        target: "agent_harness::codex",
                        "thread/resume failed (starting fresh): {e}"
                    );
                    client
                        .request("thread/start", Value::Object(start_params.clone()))
                        .await?
                }
            }
        } else if let Some(fork) = &request.fork {
            let mut p = start_params.clone();
            p.insert("threadId".into(), Value::String(fork.clone()));
            client.request("thread/fork", Value::Object(p)).await?
        } else {
            client
                .request("thread/start", Value::Object(start_params.clone()))
                .await?
        };
        let thread_id = thread["thread"]["id"].as_str().unwrap_or("").to_owned();
        let mut children = subagents::Subagents::new(thread_id.clone());
        children.restore(&thread["thread"]);
        Ok::<_, HarnessError>((thread_id, children))
    };
    let (thread_id, mut children) = tokio::select! {
        res = setup => match res {
            Ok(thread_id) => thread_id,
            Err(e) => {
                send_final(
                    &event_tx,
                    AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(e.to_string()),
                        session_id: None,
                    },
                )
                .await;
                shutdown_child(&mut child, kill_grace).await;
                return;
            }
        },
        _ = interrupt.cancelled() => {
            send_final(
                &event_tx,
                AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: None,
                },
            )
            .await;
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    };

    let turn_params = |text: &str, attachments: &[String], skills: &[SkillRef]| -> Value {
        let mut p = serde_json::Map::new();
        p.insert("threadId".into(), Value::String(thread_id.clone()));
        p.insert("input".into(), prompt_input(text, attachments, skills));
        p.insert("approvalPolicy".into(), approval_policy.into());
        p.insert("sandboxPolicy".into(), sandbox_policy_value(sandbox));
        // Without this Codex streams no reasoning summaries and looks idle for minutes.
        p.insert("summary".into(), "auto".into());
        if let Some(model) = &request.model {
            p.insert("model".into(), Value::String(model.clone()));
        }
        if let Some(effort) = effort {
            p.insert("effort".into(), effort.into());
        }
        if let Some(tier) = &service_tier {
            p.insert("serviceTier".into(), Value::String(tier.clone()));
        }
        Value::Object(p)
    };

    let mut assistant_message_id = new_message_id();
    if !send(
        &event_tx,
        AgentEvent::SessionStarted {
            harness: HarnessId::Codex,
            model: request.model.clone().unwrap_or_default(),
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: thread_id.clone(),
            assistant_message_id: assistant_message_id.clone(),
        },
    )
    .await
    {
        shutdown_child(&mut child, kill_grace).await;
        return;
    }

    let mut router = TurnRouter::default();
    match start_turn(
        &client,
        turn_params(&request.prompt, &request.attachments, &request.skills),
    )
    .await
    {
        Ok(id) => router.adopt_started(id),
        Err(e) => {
            send_final(
                &event_tx,
                AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(e.to_string()),
                    session_id: Some(thread_id.clone()),
                },
            )
            .await;
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    }

    let mut streamed_text: HashSet<String> = HashSet::new();
    let mut delivered_review: Option<String> = None;
    let mut reasoning_streams: HashMap<String, ReasoningStream> = HashMap::new();
    let mut pending_usage: Option<AgentEvent> = None;
    let mut queued_steers: VecDeque<SteerMessage> = VecDeque::new();
    let mut compacting = false;
    let mut compacted_tokens: Option<u64> = None;
    let mut steering_open = true;
    let mut interrupted = false;
    let mut interrupt_sent = false;
    let mut done_current = false;
    let mut current_native = command_request(&request.prompt, &thread_id)
        .ok()
        .flatten()
        .is_some();
    let mut done_after_interrupt = false;
    let mut escalation: Option<tokio::task::JoinHandle<()>> = None;

    'main: loop {
        tokio::select! {
            inc = incoming.recv() => match inc {
                Some(Incoming::Notification { method, params }) => {
                // Must run first: a child's turn/completed would otherwise settle the parent turn.
                if let Some(nthread) = notification_thread_id(&method, &params)
                    && !nthread.is_empty()
                    && nthread != thread_id
                {
                    match route_child_notification(&method) {
                        ChildRoute::Parent => {}
                        ChildRoute::Consumed => continue,
                        ChildRoute::Subagent => {
                            for event in children.notification(&nthread, &method, &params) {
                                if !send(&event_tx, event).await {
                                    break 'main;
                                }
                            }
                            continue;
                        }
                    }
                }
                match method.as_str() {
                    "turn/started" => router.note_started(turn_id(&params)),

                    "item/agentMessage/delta" => {
                        streamed_text.insert(item_id(&params));
                        if let Some(text) = delta_text(&params)
                            && !send(&event_tx, AgentEvent::TextDelta { text }).await
                        {
                            break 'main;
                        }
                    }

                    "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta"
                    | "item/reasoning/summaryPartAdded" => {
                        for event in reasoning_streams.entry(thread_id.clone()).or_default()
                            .map(&method, &params)
                        {
                            if !send(&event_tx, event).await {
                                break 'main;
                            }
                        }
                    }

                    "item/started" | "item/completed" => {
                        let phase = if method == "item/started" {
                            Phase::Started
                        } else {
                            Phase::Completed
                        };
                        let item = params.get("item").unwrap_or(&Value::Null);
                        if item_type(item) == "contextCompaction" {
                            let event = if phase == Phase::Started {
                                compacted_tokens = None;
                                AgentEvent::Compacting { active: true }
                            } else {
                                AgentEvent::Compacted { tokens: compacted_tokens.take(), manual: current_native }
                            };
                            compacting = phase == Phase::Started;
                            if !send(&event_tx, event).await { break 'main; }
                        }
                        if phase == Phase::Completed
                            && item_type(item) == "exitedReviewMode"
                            && let Some(text) = item.get("review").and_then(Value::as_str)
                        {
                            delivered_review = Some(text.trim().to_owned());
                            if !send(&event_tx, AgentEvent::TextDelta { text: text.into() }).await {
                                break 'main;
                            }
                        }
                        if matches!(item_type(item), "agentMessage" | "agent_message") {
                            if phase == Phase::Completed {
                                let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                                // Newer Codex versions repeat the review as a plain message.
                                let repeats_review =
                                    delivered_review.as_deref() == Some(text.trim());
                                if !streamed_text.contains(id)
                                    && !text.is_empty()
                                    && !repeats_review
                                    && !send(&event_tx, AgentEvent::TextDelta { text: text.into() }).await
                                {
                                    break 'main;
                                }
                                // Consecutive assistant messages carry no separator of their own.
                                if !send(
                                    &event_tx,
                                    AgentEvent::TextDelta {
                                        text: "\n\n".into(),
                                    },
                                )
                                .await
                                {
                                    break 'main;
                                }
                                let (prev, _next) = rotate(&mut assistant_message_id);
                                if !send(
                                    &event_tx,
                                    AgentEvent::AssistantMessageCompleted {
                                        assistant_message_id: prev,
                                    },
                                )
                                .await
                                {
                                    break 'main;
                                }
                            }
                        } else {
                            for ev in children.parent_item(phase, item) {
                                if !send(&event_tx, ev).await {
                                    break 'main;
                                }
                            }
                        }
                    }

                    "thread/tokenUsage/updated" => {
                        let context_usage = normalize::context_usage_event(&params);
                        if compacting
                            && let Some(AgentEvent::ContextUsage { tokens: Some(tokens), .. }) = &context_usage
                        {
                            compacted_tokens = Some(*tokens);
                        }
                        if let Some(usage) = context_usage
                            && !send(&event_tx, usage).await { break 'main; }
                        if let Some(usage) = usage_event(&params) {
                            pending_usage = Some(usage);
                        }
                    }

                    "turn/plan/updated" => {
                        for ev in normalize::plan_update_events(&params) {
                            if !send(&event_tx, ev).await { break 'main; }
                        }
                    }

                    "turn/completed" => {
                        let id = turn_id(&params);
                        router.note_completed(&id);
                        streamed_text.clear();
                        delivered_review = None;
                        if let Some(usage) = pending_usage.take()
                            && !send(&event_tx, usage).await
                        {
                            break 'main;
                        }
                        let error = turn_error_message(&params).or_else(|| {
                            (params
                                .pointer("/turn/status")
                                .and_then(Value::as_str)
                                == Some("failed"))
                            .then(|| "Codex turn failed".to_owned())
                        });
                        let status = if interrupted {
                            DoneStatus::Interrupted
                        } else if error.is_some() {
                            DoneStatus::Errored
                        } else {
                            DoneStatus::Completed
                        };
                        done_current = true;
                        if !send(
                            &event_tx,
                            AgentEvent::Done {
                                status,
                                result: None,
                                error,
                                session_id: Some(thread_id.clone()),
                            },
                        )
                        .await
                        {
                            break 'main;
                        }
                        if interrupted {
                            done_after_interrupt = true;
                            break 'main;
                        }
                        current_native = false;
                        if let Some(msg) = queued_steers.pop_front() {
                            current_native = command_request(&msg.prompt, &thread_id).ok().flatten().is_some();
                            if !steer_as_new_turn(
                                &client,
                                turn_params(&msg.prompt, &msg.attachments, &msg.skills),
                                &mut router,
                                &event_tx,
                                &mut assistant_message_id,
                                &mut done_current,
                            )
                            .await
                            {
                                break 'main;
                            }
                        } else if !steering_open {
                            break 'main;
                        }
                    }

                    "turn/failed" => {
                        router.note_completed(&turn_id(&params));
                        if let Some(usage) = pending_usage.take()
                            && !send(&event_tx, usage).await
                        {
                            break 'main;
                        }
                        done_current = true;
                        if interrupted {
                            done_after_interrupt = true;
                        }
                        send_final(
                            &event_tx,
                            AgentEvent::Done {
                                status: if interrupted {
                                    DoneStatus::Interrupted
                                } else {
                                    DoneStatus::Errored
                                },
                                result: None,
                                error: Some(
                                    turn_error_message(&params)
                                        .unwrap_or_else(|| "Codex turn failed".into()),
                                ),
                                session_id: Some(thread_id.clone()),
                            },
                        )
                        .await;
                        break 'main;
                    }

                    "turn/aborted" => {
                        router.note_completed(&turn_id(&params));
                        done_current = true;
                        if interrupted {
                            done_after_interrupt = true;
                        }
                        send_final(
                            &event_tx,
                            AgentEvent::Done {
                                status: DoneStatus::Interrupted,
                                result: None,
                                error: None,
                                session_id: Some(thread_id.clone()),
                            },
                        )
                        .await;
                        break 'main;
                    }

                    "error" => {
                        let message = params
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .or_else(|| params.get("message").and_then(Value::as_str))
                            .unwrap_or("Codex error")
                            .to_owned();
                        if !send(&event_tx, AgentEvent::Error { message }).await {
                            break 'main;
                        }
                    }

                    _ => {}
                }
                }

                Some(Incoming::Request { id, method, params }) => {
                    handle_server_request(
                        &client,
                        id,
                        &method,
                        &params,
                        permission.skips_prompts(),
                        &request_input,
                    );
                }

                Some(Incoming::Eof) | None => break 'main,
            },

            steer = steering.recv(), if steering_open && !interrupted => match steer {
                Some(msg) => {
                    // Steered acks must stay FIFO, so nothing may overtake a queued command.
                    if !done_current && (!queued_steers.is_empty() || current_native || !matches!(command_request(&msg.prompt, &thread_id), Ok(None))) {
                        queued_steers.push_back(msg);
                        continue 'main;
                    }
                    if let Some(expected) = router.active.clone() {
                        let steer_params = json!({
                            "threadId": thread_id,
                            "expectedTurnId": expected,
                            "input": prompt_input(&msg.prompt, &msg.attachments, &msg.skills),
                        });
                        match client.request("turn/steer", steer_params).await {
                            Ok(_) => {
                                let (prev, next) = rotate(&mut assistant_message_id);
                                if !send(
                                    &event_tx,
                                    AgentEvent::Steered {
                                        assistant_message_id: Some(prev),
                                        next_assistant_message_id: Some(next),
                                    },
                                )
                                .await
                                {
                                    break 'main;
                                }
                            }
                            // Usually the turn ended before the steer landed; redeliver as the next turn.
                            Err(e) => {
                                tracing::debug!(
                                    target: "agent_harness::codex",
                                    "turn/steer rejected (queued as next turn): {e}"
                                );
                                if router.active.as_deref() == Some(expected.as_str())
                                    && !router.is_completed(&expected)
                                {
                                    queued_steers.push_back(msg);
                                } else {
                                    current_native = command_request(&msg.prompt, &thread_id).ok().flatten().is_some();
                                    if !steer_as_new_turn(
                                        &client, turn_params(&msg.prompt, &msg.attachments, &msg.skills), &mut router, &event_tx,
                                        &mut assistant_message_id, &mut done_current,
                                    ).await { break 'main; }
                                }
                            }
                        }
                    } else {
                        current_native = command_request(&msg.prompt, &thread_id).ok().flatten().is_some();
                        if !steer_as_new_turn(
                            &client, turn_params(&msg.prompt, &msg.attachments, &msg.skills), &mut router, &event_tx,
                            &mut assistant_message_id, &mut done_current,
                        ).await { break 'main; }
                    }
                }
                None => {
                    steering_open = false;
                    if done_current && router.active.is_none() && queued_steers.is_empty() {
                        break 'main;
                    }
                }
            },

            _ = interrupt.cancelled(), if !interrupt_sent => {
                interrupt_sent = true;
                interrupted = true;
                if let Some(turn) = router.active.clone() {
                    let client = client.clone();
                    let thread = thread_id.clone();
                    tokio::spawn(async move {
                        if let Err(e) = client
                            .request("turn/interrupt", json!({ "threadId": thread, "turnId": turn }))
                            .await
                        {
                            tracing::debug!(
                                target: "agent_harness::codex",
                                "turn/interrupt failed (escalation will reap): {e}"
                            );
                        }
                    });
                    escalation = crate::process::escalate_interrupt(&child, interrupt_grace, kill_grace);
                } else {
                    break 'main;
                }
            },

            _ = event_tx.closed() => break 'main,
        }
    }

    if !event_tx.is_closed() {
        if interrupted && !done_after_interrupt {
            send_final(
                &event_tx,
                AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some(thread_id.clone()),
                },
            )
            .await;
        } else if !interrupted && !done_current {
            let status = child.try_wait().ok().flatten();
            send_final(
                &event_tx,
                AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crate::crash_message(
                        "codex app-server",
                        status,
                        &stderr_tail,
                    )),
                    session_id: Some(thread_id.clone()),
                },
            )
            .await;
        }
    }

    shutdown_child(&mut child, kill_grace).await;
    if let Some(handle) = escalation {
        handle.abort();
    }
}

/// Returns false when the session loop should end.
async fn steer_as_new_turn(
    client: &RpcClient,
    params: Value,
    router: &mut TurnRouter,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    assistant_message_id: &mut String,
    done_current: &mut bool,
) -> bool {
    match start_turn(client, params).await {
        Ok(id) => {
            router.adopt_started(id);
            *done_current = false;
            let (prev, next) = rotate(assistant_message_id);
            send(
                event_tx,
                AgentEvent::Steered {
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                },
            )
            .await
        }
        Err(e) => {
            send_final(
                event_tx,
                AgentEvent::Error {
                    message: format!("Steering failed: {e}"),
                },
            )
            .await;
            false
        }
    }
}

type RequestInputFn = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

/// Every request must get a reply or the app server stalls waiting for it.
fn permission_sandbox(permission: PermissionMode) -> crate::SandboxLevel {
    match permission {
        PermissionMode::ReadOnly => crate::SandboxLevel::ReadOnly,
        PermissionMode::FullAccess => crate::SandboxLevel::DangerFullAccess,
        _ => crate::SandboxLevel::WorkspaceWrite,
    }
}

fn handle_server_request(
    client: &RpcClient,
    id: Value,
    method: &str,
    params: &Value,
    skip_prompts: bool,
    request_input: &Arc<RequestInputFn>,
) {
    // A content question, so never auto-approved.
    if method == "item/tool/requestUserInput" {
        let questions = user_input_questions(params);
        if questions.is_empty() {
            client.respond(&id, json!({ "answers": {} }));
            return;
        }
        let client = client.clone();
        let request_input = Arc::clone(request_input);
        tokio::spawn(async move {
            let asked: Vec<UserInputQuestion> = questions.iter().map(|(_, q)| q.clone()).collect();
            let answers = (request_input)(asked).await.unwrap_or_default();
            let mut by_id = serde_json::Map::new();
            for (wire_id, q) in &questions {
                let labels: Vec<Value> = answers
                    .iter()
                    .find(|a| a.question_id == q.id)
                    .map(|a| a.labels.iter().cloned().map(Value::String).collect())
                    .unwrap_or_default();
                by_id.insert(wire_id.clone(), json!({ "answers": labels }));
            }
            client.respond(&id, json!({ "answers": by_id }));
        });
        return;
    }
    let is_approval = matches!(
        method,
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
    );
    if !is_approval {
        tracing::debug!(
            target: "agent_harness::codex",
            "unhandled server request: {method}"
        );
        client.respond_error(&id, -32601, &format!("unsupported method: {method}"));
        return;
    }
    if skip_prompts {
        client.respond(&id, json!({ "decision": "accept" }));
        return;
    }

    let question = approval_question(method, params);
    let client = client.clone();
    let request_input = Arc::clone(request_input);
    tokio::spawn(async move {
        let answers = (request_input)(vec![question.clone()])
            .await
            .unwrap_or_default();
        let accept = answers.iter().any(|a| {
            a.question_id == question.id
                && a.labels
                    .iter()
                    .any(|l| l.eq_ignore_ascii_case(crate::claude::PERMISSION_ALLOW))
        });
        client.respond(
            &id,
            json!({ "decision": if accept { "accept" } else { "decline" } }),
        );
    });
}

/// Answers must be keyed by the wire id, not the generated question id.
fn user_input_questions(params: &Value) -> Vec<(String, UserInputQuestion)> {
    params
        .get("questions")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(ix, q)| {
            let field = |keys: [&str; 3]| {
                keys.iter()
                    .find_map(|k| q.get(*k).and_then(Value::as_str))
                    .unwrap_or("")
                    .to_owned()
            };
            let wire_id = {
                let id = field(["id", "questionId", "question_id"]);
                if id.is_empty() { format!("q{ix}") } else { id }
            };
            let question = UserInputQuestion {
                id: new_message_id(),
                header: {
                    let h = field(["header", "title", "label"]);
                    if h.is_empty() {
                        "Codex question".into()
                    } else {
                        h
                    }
                },
                question: field(["question", "prompt", "text"]),
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
                prefill: None,
                multiline: false,
                multi_select: ["multiSelect", "multi_select"]
                    .iter()
                    .find_map(|k| q.get(*k).and_then(Value::as_bool))
                    .unwrap_or(false),
            };
            (wire_id, question)
        })
        .collect()
}

fn approval_question(method: &str, params: &Value) -> UserInputQuestion {
    let (header, question) = if method.contains("commandExecution") {
        let command = match params.get("command") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        (
            crate::claude::PERMISSION_HEADER.to_owned(),
            if command.is_empty() {
                "Codex wants to run a command. Allow it?".to_owned()
            } else {
                format!("Codex wants to run `{command}`. Allow it?")
            },
        )
    } else {
        let paths: Vec<&str> = params
            .get("changes")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|c| c.get("path").and_then(Value::as_str))
            .collect();
        (
            crate::claude::PERMISSION_HEADER.to_owned(),
            if paths.is_empty() {
                "Codex wants to modify files. Allow it?".to_owned()
            } else {
                format!("Codex wants to modify {}. Allow it?", paths.join(", "))
            },
        )
    };
    UserInputQuestion {
        id: new_message_id(),
        header,
        question,
        options: vec![
            crate::claude::PERMISSION_ALLOW.into(),
            crate::claude::PERMISSION_DENY.into(),
        ],
        prefill: None,
        multiline: false,
        multi_select: false,
    }
}

use crate::shutdown_child;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_modes_pick_the_sandbox() {
        assert_eq!(
            permission_sandbox(PermissionMode::ReadOnly),
            crate::SandboxLevel::ReadOnly
        );
        assert_eq!(
            permission_sandbox(PermissionMode::Auto),
            crate::SandboxLevel::WorkspaceWrite
        );
        assert_eq!(
            permission_sandbox(PermissionMode::ApproveForMe),
            crate::SandboxLevel::WorkspaceWrite
        );
        assert_eq!(
            permission_sandbox(PermissionMode::FullAccess),
            crate::SandboxLevel::DangerFullAccess
        );
        assert_eq!(
            PermissionMode::AcceptEdits.for_harness(HarnessId::Codex),
            PermissionMode::Auto
        );
    }
    use serde_json::json;

    #[test]
    fn current_schema_and_legacy_visibility_are_compatible() {
        let page = json!({"data":[{"model":"current", "hidden":false, "isDefault":true,
            "description":"Current model", "upgrade":"next", "upgradeInfo":{"retirementAt":"2026-12-01"},
            "availabilityNux":{"message":"Available"}, "serviceTiers":["default","fast"],
            "defaultServiceTier":"default", "inputModalities":["text","image"]}], "nextCursor":"next-page"});
        let (models, next) = parse_model_list_page(&page);
        assert_eq!(
            models[0].0.description.as_deref(),
            Some("Current model (upgrade: next)")
        );
        assert!(models[0].1);
        assert_eq!(next.as_deref(), Some("next-page"));
        assert!(!legacy_model_page(&page));
        assert!(legacy_model_page(&json!({"data":[{"model":"old"}]})));
        assert!(!legacy_model_page(&json!({"data":[]})));
    }

    #[test]
    fn model_page_skips_hidden_and_unknown_efforts() {
        let page = json!({
            "data": [
                {
                    "id": "hidden",
                    "model": "hidden",
                    "displayName": "Hidden",
                    "hidden": true,
                    "supportedReasoningEfforts": [{ "reasoningEffort": "high" }]
                },
                {
                    "id": "gpt-6-astra",
                    "model": "gpt-6-astra",
                    "displayName": "GPT-6-Astra",
                    "description": "  Most capable  ",
                    "hidden": false,
                    "supportedReasoningEfforts": [
                        { "reasoningEffort": "high" },
                        { "reasoningEffort": "future" }
                    ],
                    "serviceTiers": [{ "id": "priority", "name": "Fast" }],
                    "additionalSpeedTiers": ["fast"],
                    "defaultServiceTier": null,
                    "isDefault": true
                }
            ],
            "nextCursor": "next"
        });
        let (models, cursor) = parse_model_list_page(&page);
        assert_eq!(cursor.as_deref(), Some("next"));
        assert_eq!(models.len(), 1);
        let (astra, is_default) = &models[0];
        assert_eq!(astra.id, "gpt-6-astra");
        assert_eq!(astra.description.as_deref(), Some("Most capable"));
        assert_eq!(astra.reasoning_levels, vec![ReasoningLevel::High]);
        assert!(*is_default);
        assert_eq!(astra.options[0].choices.len(), 2);
        assert_eq!(astra.options[0].choices[1].id, "fast");
    }

    #[test]
    fn approval_questions_are_allow_or_deny() {
        let q = approval_question(
            "item/commandExecution/requestApproval",
            &json!({"itemId": "c1", "command": "rm -rf /tmp/x"}),
        );
        assert_eq!(q.header, crate::claude::PERMISSION_HEADER);
        assert!(q.question.contains("rm -rf /tmp/x"));
        assert_eq!(q.options, vec!["Allow".to_string(), "Deny".to_string()]);
        assert!(!q.multi_select);

        let q = approval_question(
            "item/fileChange/requestApproval",
            &json!({"changes": [{"path": "/a.rs"}, {"path": "/b.rs"}]}),
        );
        assert_eq!(q.header, crate::claude::PERMISSION_HEADER);
        assert!(q.question.contains("/a.rs, /b.rs"));

        let q = approval_question(
            "item/commandExecution/requestApproval",
            &json!({"command": ["git", "push", "--force"]}),
        );
        assert!(q.question.contains("git push --force"));
    }

    #[test]
    fn turn_router_never_revives_completed_turns() {
        let mut r = TurnRouter::default();
        r.note_completed("t-1");
        r.adopt_started("t-1".into());
        assert_eq!(r.active, None);
        r.note_started("t-1".into());
        assert_eq!(r.active, None);

        r.note_started("t-2".into());
        assert_eq!(r.active.as_deref(), Some("t-2"));
        r.note_started("t-3".into());
        assert_eq!(r.active.as_deref(), Some("t-3"));
        assert!(r.is_completed("t-2"));
    }

    #[test]
    fn prompt_input_puts_text_first_and_dedupes_skills() {
        let skill = SkillRef {
            name: "review".into(),
            path: "/repo/SKILL.md".into(),
        };
        let input = prompt_input("look", &["/tmp/a.png".into()], &[skill.clone(), skill]);
        assert_eq!(
            input,
            json!([
                {"type": "text", "text": "look"},
                {"type": "skill", "name": "review", "path": "/repo/SKILL.md"},
                {"type": "localImage", "path": "/tmp/a.png"},
            ])
        );
    }
}
