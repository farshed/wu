//! Drives `opencode serve` over HTTP + SSE (not ACP, whose turn end is unreliable in opencode 1.18).

mod discovery;
mod feed;
mod server;
#[cfg(test)]
mod tests;
mod v2;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{
    AgentEvent, DoneStatus, ExternalSession, HarnessId, Model, PermissionMode, ReasoningLevel,
    RunRequest, Skill, SkillRef, SlashCommand, SteeringMode, UserInputAnswer, UserInputQuestion,
};
use crate::{Harness, HarnessError, RunControls};
use discovery::{
    ProviderCatalog, REASONING_LEVELS, agent_option, command_names, commands_from_wire,
    context_windows, models_from_providers, pick_variant, skills_from_wire,
};
use feed::{
    ChildRun, PendingSpawn, SessionFeed, bind_child, context_usage_event, map_questions,
    part_delta_events, part_snapshot_events, tag, task_completion,
};
use server::{BusMessage, Protocol, Server, ServerVersion, bus_task, post_error_message};

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
const RETRY_REPORT_ATTEMPT: u64 = 3;
const RETRY_ABORT_ATTEMPT: u64 = 8;
const DEFAULT_STALL_BOUND: Duration = Duration::from_secs(60);
const STALL_ENV: &str = "WU_OPENCODE_STALL_MS";
const COMMAND_CACHE_TTL: Duration = Duration::from_secs(10);

const STALL_HINT: &str = "The model provider is likely unreachable or rejecting requests. \
     Check the model/provider setup (`opencode auth list`, opencode.json) or the opencode \
     log (~/.local/share/opencode/log).";

const INSTALL_HINT: &str = "opencode (searched PATH, ~/.opencode/bin, ~/.local/bin, ~/.bun/bin, \
     ~/.npm-global/bin, /opt/homebrew/bin, /usr/local/bin, and fnm/nvm/volta/pnpm/bun install \
     dirs; install with `curl -fsSL https://opencode.ai/install | bash`, then \
     `opencode auth login`; set OPENCODE_EXECUTABLE to override)";

fn startup_timeout() -> Duration {
    std::env::var(server::STARTUP_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_STARTUP_TIMEOUT)
}

fn stall_bound() -> Option<Duration> {
    match std::env::var(STALL_ENV).map(|value| value.parse::<u64>()) {
        Ok(Ok(0)) => None,
        Ok(Ok(milliseconds)) => Some(Duration::from_millis(milliseconds)),
        _ => Some(DEFAULT_STALL_BOUND),
    }
}

fn resolve_opencode_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OPENCODE_EXECUTABLE").filter(|path| !path.is_empty()) {
        return crate::executable::validate_native_override(&PathBuf::from(path)).ok();
    }
    let mut extra = Vec::new();
    if let Some(home) = crate::executable::home_dir() {
        extra.push(home.join(".opencode").join("bin").join("opencode"));
        extra.push(home.join(".local").join("bin").join("opencode"));
        extra.push(home.join(".bun").join("bin").join("opencode"));
        extra.push(home.join(".npm-global").join("bin").join("opencode"));
    }
    extra.push(PathBuf::from("/opt/homebrew/bin/opencode"));
    extra.push(PathBuf::from("/usr/local/bin/opencode"));
    crate::executable::find_on_paths("opencode", extra)
}

const SESSION_RULE_PERMISSIONS: [&str; 4] = ["edit", "bash", "webfetch", "task"];

/// Permissions that must ask instead of being allowed silently; `task` gates subagents, which never inherit asks.
fn forced_asks(mode: PermissionMode) -> &'static [&'static str] {
    match mode {
        PermissionMode::Ask => &["edit", "bash", "webfetch", "task"],
        PermissionMode::AcceptEdits => &["bash", "webfetch", "task"],
        _ => &[],
    }
}

fn auto_approves(mode: PermissionMode, permission: &str) -> bool {
    match mode {
        PermissionMode::FullAccess => true,
        PermissionMode::AcceptEdits => permission == "edit",
        _ => false,
    }
}

fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pattern_index, mut text_index) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while text_index < text.len() {
        match pattern.get(pattern_index) {
            Some('*') => {
                backtrack = Some((pattern_index, text_index));
                pattern_index += 1;
            }
            Some(&character) if character == '?' || character == text[text_index] => {
                pattern_index += 1;
                text_index += 1;
            }
            _ => match backtrack {
                Some((star, matched)) => {
                    pattern_index = star + 1;
                    text_index = matched + 1;
                    backtrack = Some((star, matched + 1));
                }
                None => return false,
            },
        }
    }
    pattern[pattern_index..]
        .iter()
        .all(|character| *character == '*')
}

/// Session rules outrank config and agent rules; they restate the agent's own effective rules so denies survive.
fn session_rules(agent_rules: &Value, forced: &[&str]) -> Vec<Value> {
    let rules: Vec<(&str, &str, &str)> = agent_rules
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rule| {
            Some((
                rule.get("permission")?.as_str()?,
                rule.get("pattern")?.as_str()?,
                rule.get("action")?.as_str()?,
            ))
        })
        .collect();
    let mut result = Vec::new();
    for permission in SESSION_RULE_PERMISSIONS {
        let matching: Vec<_> = rules
            .iter()
            .filter(|(rule_permission, _, _)| wildcard_matches(rule_permission, permission))
            .collect();
        // Children copy every parent deny, so denies an earlier catch-all overrides must not be restated.
        let effective_start = matching
            .iter()
            .rposition(|(_, pattern, _)| *pattern == "*")
            .unwrap_or(0);
        for (_, pattern, action) in &matching[effective_start..] {
            let action = if *action == "allow" && forced.contains(&permission) {
                "ask"
            } else {
                action
            };
            result.push(json!({ "permission": permission, "pattern": pattern, "action": action }));
        }
    }
    result
}

fn primary_agent_rules<'a>(agents: &'a Value, requested: Option<&str>) -> Option<&'a Value> {
    let agents = agents.as_array()?;
    let agent = match requested {
        Some(name) => agents
            .iter()
            .find(|agent| agent.get("name").and_then(Value::as_str) == Some(name))?,
        None => agents.iter().find(|agent| {
            agent.get("mode").and_then(Value::as_str) != Some("subagent")
                && agent.get("hidden").and_then(Value::as_bool) != Some(true)
        })?,
    };
    agent.get("permission").filter(|rules| rules.is_array())
}

fn permission_question(properties: &Value) -> UserInputQuestion {
    let permission = properties
        .get("permission")
        .or_else(|| properties.get("action"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let patterns: Vec<&str> = properties
        .get("patterns")
        .or_else(|| properties.get("resources"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let metadata = |key: &str| {
        properties
            .pointer(&format!("/metadata/{key}"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let first_pattern = patterns.first().copied();
    let specific = match permission {
        "bash" => metadata("command").map(|command| format!("Run `{command}`?")),
        "edit" => metadata("filepath")
            .or(first_pattern)
            .map(|path| format!("Edit {path}?")),
        "read" => metadata("filepath")
            .or(first_pattern)
            .map(|path| format!("Read {path}?")),
        "webfetch" => metadata("url")
            .or(first_pattern)
            .map(|url| format!("Fetch {url}?")),
        "websearch" => metadata("query")
            .or(first_pattern)
            .map(|query| format!("Search the web for \"{query}\"?")),
        "task" => first_pattern.map(|agent| format!("Start a {agent} subagent?")),
        "external_directory" => (!patterns.is_empty())
            .then(|| format!("Access {} outside the project?", patterns.join(", "))),
        _ => None,
    };
    let question = specific.unwrap_or_else(|| match (permission, patterns.is_empty()) {
        ("", _) => "OpenCode wants permission to continue. Allow it?".to_owned(),
        (permission, true) => format!("Allow {permission}?"),
        (permission, false) => format!("Allow {permission} for {}?", patterns.join(", ")),
    });
    UserInputQuestion {
        id: uuid::Uuid::new_v4().to_string(),
        header: crate::claude::PERMISSION_HEADER.into(),
        question,
        options: vec![
            crate::claude::PERMISSION_ALLOW.into(),
            crate::claude::PERMISSION_DENY.into(),
        ],
        multi_select: false,
        prefill: None,
        multiline: false,
    }
}

fn plan_prompt(prompt: &str, skills: &[SkillRef], attachments: &[String]) -> (String, bool) {
    let mut names: Vec<&str> = Vec::new();
    for skill in skills {
        if !names.contains(&skill.name.as_str()) {
            names.push(&skill.name);
        }
    }
    if names.is_empty() {
        return (prompt.to_owned(), false);
    }
    if let [only] = names.as_slice()
        && attachments.is_empty()
        && let Some(rest) = prompt.trim_start().strip_prefix('$')
    {
        let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if &rest[..name_end] == *only {
            return (format!("/{only}{}", &rest[name_end..]), true);
        }
    }
    let list = names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    (
        format!("{prompt}\n\nBefore you start, load these skills with the `skill` tool: {list}."),
        false,
    )
}

async fn recent_models() -> Vec<String> {
    let state = crate::model_context::root(
        "XDG_STATE_HOME",
        crate::executable::home_or_current_dir()
            .join(".local")
            .join("state"),
    );
    let Ok(raw) = tokio::fs::read(state.join("opencode").join("model.json")).await else {
        return Vec::new();
    };
    serde_json::from_slice::<Value>(&raw)
        .ok()
        .and_then(|state| state.get("recent").cloned())
        .and_then(|recent| recent.as_array().cloned())
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some(format!(
                "{}/{}",
                entry.get("providerID")?.as_str()?,
                entry.get("modelID")?.as_str()?
            ))
        })
        .collect()
}

fn default_model_id(
    providers: &ProviderCatalog,
    configured: Option<&str>,
    recent: &[String],
    models: &[Model],
) -> Option<String> {
    let offered = |id: &str| models.iter().any(|model| model.id == id);
    if let Some(configured) = configured.filter(|id| offered(id)) {
        return Some(configured.to_owned());
    }
    if let Some(recent) = recent.iter().find(|id| offered(id)) {
        return Some(recent.clone());
    }
    let defaults = providers.defaults.as_ref()?;
    let first_provider = match providers.connected.as_deref() {
        Some([first, ..]) => first.clone(),
        _ => providers
            .all
            .iter()
            .flatten()
            .find_map(|provider| provider.id.clone())?,
    };
    let id = format!(
        "{first_provider}/{}",
        defaults.get(&first_provider)?.as_str()?
    );
    offered(&id).then_some(id)
}

struct CachedCommands {
    directory: PathBuf,
    fetched_at: Instant,
    wire: Value,
}

pub struct OpencodeHarness {
    executable: Option<PathBuf>,
    base_url: Option<String>,
    interrupt_grace: Duration,
    kill_grace: Duration,
    startup_timeout: Duration,
    models_cache: crate::catalog::Catalog,
    /// Serializes discovery boots; several cold opencode starts at once are slower than one.
    probe: tokio::sync::Mutex<Option<CachedCommands>>,
}

impl Default for OpencodeHarness {
    fn default() -> Self {
        Self {
            executable: None,
            base_url: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            startup_timeout: startup_timeout(),
            models_cache: crate::catalog::Catalog::default(),
            probe: tokio::sync::Mutex::new(None),
        }
    }
}

impl OpencodeHarness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    /// Drives an already running server instead of spawning one, without authentication.
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base_url = Some(base.into());
        self
    }

    pub fn with_graces(mut self, interrupt_grace: Duration, kill_grace: Duration) -> Self {
        self.interrupt_grace = interrupt_grace;
        self.kill_grace = kill_grace;
        self
    }

    fn resolve_executable(&self) -> Result<PathBuf, HarnessError> {
        if let Some(path) = &self.executable {
            return crate::executable::validate_native_override(path);
        }
        resolve_opencode_executable().ok_or_else(|| HarnessError::NotInstalled(INSTALL_HINT.into()))
    }

    fn discovery_context(&self) -> Result<crate::ModelContext, HarnessError> {
        if let Some(base) = &self.base_url {
            return Ok(crate::ModelContext {
                hash: format!("attached:{base}"),
                binary_path: PathBuf::from(base),
                binary_version: None,
            });
        }
        crate::model_context::context(self.id(), &self.resolve_executable()?, &[])
    }

    fn boot(&self, cwd: Option<&str>) -> Result<ServerBoot, HarnessError> {
        if let Some(cwd) = cwd
            && !Path::new(cwd).is_dir()
        {
            return Err(HarnessError::Protocol(format!(
                "project folder not found: {cwd}"
            )));
        }
        if let Some(base) = &self.base_url {
            let server = Server::attached(base.clone())?;
            return Ok(Box::pin(async move { Ok(server) }));
        }
        let executable = self.resolve_executable()?;
        let cwd = cwd.map(str::to_owned);
        let startup = self.startup_timeout;
        Ok(Box::pin(async move {
            Server::spawn(&executable, cwd.as_deref(), startup).await
        }))
    }

    async fn probe_models(&self) -> Result<Vec<Model>, HarnessError> {
        let _probe = self.probe.lock().await;
        let mut server = self.boot(None)?.await?;
        let result = async {
            let providers = server.provider_catalog(None).await?;
            let mut models = models_from_providers(&providers);
            let default_model = match server.protocol().await {
                Protocol::V1 => {
                    let configured = match server.get_json("/config", None).await {
                        Ok(config) => config
                            .get("model")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        Err(error) => {
                            tracing::debug!(target: "agent_harness::opencode", "config unavailable: {error}");
                            None
                        }
                    };
                    let recent = if self.base_url.is_none() {
                        recent_models().await
                    } else {
                        Vec::new()
                    };
                    default_model_id(&providers, configured.as_deref(), &recent, &models)
                }
                Protocol::V2 => {
                    let agents = server.get_json("/api/agent", None).await?;
                    let option = agent_option(&agents);
                    for model in &mut models {
                        model.options.push(option.clone());
                    }
                    None
                }
            };
            if let Some(default_model) = default_model
                && let Some(index) = models.iter().position(|model| model.id == default_model)
            {
                let model = models.remove(index);
                models.insert(0, model);
            }
            if models.is_empty() {
                return Err(HarnessError::Protocol(
                    "opencode advertised no models (`opencode auth login` to configure a provider)"
                        .into(),
                ));
            }
            Ok(models)
        }
        .await;
        server.shutdown(self.kill_grace).await;
        result
    }

    async fn project_commands(&self, cwd: &Path) -> Result<Value, HarnessError> {
        let mut cache = self.probe.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.directory == cwd
            && cached.fetched_at.elapsed() < COMMAND_CACHE_TTL
        {
            return Ok(cached.wire.clone());
        }
        let directory = utf8_directory(cwd)?;
        let mut server = self.boot(directory)?.await?;
        let result = server.commands_wire(directory).await;
        server.shutdown(self.kill_grace).await;
        let wire = result?;
        *cache = Some(CachedCommands {
            directory: cwd.to_path_buf(),
            fetched_at: Instant::now(),
            wire: wire.clone(),
        });
        Ok(wire)
    }
}

#[async_trait]
impl Harness for OpencodeHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Opencode
    }
    fn display_name(&self) -> &str {
        "OpenCode"
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
        self.base_url.is_some() || self.resolve_executable().is_ok()
    }
    fn executable_path(&self) -> Option<PathBuf> {
        self.resolve_executable().ok()
    }
    fn model_context(&self) -> Result<Option<crate::ModelContext>, HarnessError> {
        self.discovery_context().map(Some)
    }
    async fn model_catalog(&self, force: bool) -> Result<crate::ModelCatalog, HarnessError> {
        self.discovery_context()?.log();
        self.models_cache
            .get_with_timeout(
                force,
                self.startup_timeout * 3 + Duration::from_secs(1),
                || self.discovery_context().map(|context| context.key()),
                || self.probe_models(),
            )
            .await
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.model_catalog(false)
            .await
            .map(|catalog| catalog.models)
    }
    async fn commands(&self, cwd: &Path) -> Result<Vec<SlashCommand>, HarnessError> {
        Ok(commands_from_wire(&self.project_commands(cwd).await?))
    }
    async fn skills(&self, cwd: &Path) -> Result<Vec<Skill>, HarnessError> {
        Ok(skills_from_wire(&self.project_commands(cwd).await?))
    }
    async fn external_sessions(&self, cwd: &Path) -> Result<Vec<ExternalSession>, HarnessError> {
        let directory = utf8_directory(cwd)?;
        let mut server = self.boot(directory)?.await?;
        let result = server.saved_sessions(directory).await;
        server.shutdown(self.kill_grace).await;
        Ok(result?
            .map(|list| feed::external_sessions(&list))
            .unwrap_or_default())
    }
    async fn external_history(
        &self,
        cwd: &Path,
        session_id: &str,
    ) -> Result<Vec<AgentEvent>, HarnessError> {
        let directory = utf8_directory(cwd)?;
        let mut server = self.boot(directory)?.await?;
        let result = server.saved_messages(session_id, directory).await;
        server.shutdown(self.kill_grace).await;
        result?
            .map(|messages| feed::history_events(&messages))
            .ok_or_else(|| {
                HarnessError::Protocol("This OpenCode version can't share its saved chats.".into())
            })
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (prompt, initial_native_command_selected) =
            plan_prompt(&request.prompt, &request.skills, &request.attachments);
        let permission = request.permission.for_harness(HarnessId::Opencode);
        let cwd = (!request.cwd.is_empty()).then(|| request.cwd.clone());
        let server = self.boot(cwd.as_deref())?;
        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        tokio::spawn(run_session(Session {
            server,
            event_tx,
            controls,
            request: RunRequest { prompt, ..request },
            permission,
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            known_commands: None,
            initial_native_command_selected,
        }));
        Ok(
            futures::stream::unfold(event_rx, |mut receiver| async move {
                receiver.recv().await.map(|event| (event, receiver))
            })
            .boxed(),
        )
    }
}

type EventSender = mpsc::Sender<Result<AgentEvent, HarnessError>>;

type RequestInput = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

type ServerBoot =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Server, HarnessError>> + Send>>;

struct Session {
    server: ServerBoot,
    event_tx: EventSender,
    controls: RunControls,
    request: RunRequest,
    permission: PermissionMode,
    interrupt_grace: Duration,
    kill_grace: Duration,
    known_commands: Option<Value>,
    initial_native_command_selected: bool,
}

struct QueuedSteer {
    prompt: String,
    native_command_selected: bool,
    attachments: Vec<String>,
}

fn utf8_directory(cwd: &Path) -> Result<Option<&str>, HarnessError> {
    if cwd.as_os_str().is_empty() {
        return Ok(None);
    }
    cwd.to_str()
        .map(Some)
        .ok_or_else(|| HarnessError::Protocol("Project path is not UTF-8".into()))
}

fn new_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn rotate(id: &mut String) -> (String, String) {
    let previous = std::mem::replace(id, new_message_id());
    (previous, id.clone())
}

async fn send(event_tx: &EventSender, event: AgentEvent) -> bool {
    event_tx.send(Ok(event)).await.is_ok()
}

async fn send_final(event_tx: &EventSender, event: AgentEvent) {
    if !send(event_tx, event).await {
        tracing::debug!(target: "agent_harness::opencode", "event receiver gone, dropping final event");
    }
}

struct TurnState {
    active: bool,
    /// An idle settles this turn only after its own busy/retry, or after we aborted it.
    idle_ready: bool,
    idle_confirmations: u8,
    status_poll: Option<tokio::time::Instant>,
    status_backoff: Duration,
    saw_activity: bool,
    saw_content: bool,
    error: Option<String>,
    retry_reported: bool,
    aborted_for_retry: bool,
    stall_deadline: Option<tokio::time::Instant>,
    open_tools: std::collections::HashSet<String>,
    /// Aborted to deliver a steer, so its idle is a steer boundary and not the end of the run.
    preempted: bool,
}

impl TurnState {
    fn begin(stall: Option<Duration>) -> Self {
        Self {
            active: true,
            idle_ready: false,
            idle_confirmations: 0,
            status_poll: None,
            status_backoff: Duration::from_millis(100),
            saw_activity: false,
            saw_content: false,
            error: None,
            retry_reported: false,
            aborted_for_retry: false,
            stall_deadline: stall.map(|bound| tokio::time::Instant::now() + bound),
            open_tools: Default::default(),
            preempted: false,
        }
    }

    fn note_activity(&mut self) {
        self.saw_activity = true;
        self.stall_deadline = None;
    }
}

/// `generation` ties a late HTTP failure to the turn that sent it.
struct TurnFailure {
    generation: u64,
    message: String,
}

struct TurnSpec<'a> {
    model: Option<&'a (String, String)>,
    variant: Option<&'a str>,
    attachments: &'a [String],
}

struct PromptTarget<'a> {
    server: &'a Server,
    session_id: &'a str,
    directory: Option<&'a str>,
    routable: &'a [String],
    failure_tx: &'a mpsc::UnboundedSender<TurnFailure>,
}

async fn agent_session_rules(
    server: &Server,
    dir: Option<&str>,
    agent: Option<&str>,
    permission: PermissionMode,
) -> Result<Vec<Value>, HarnessError> {
    let agents = server.get_json("/agent", dir).await?;
    let rules = primary_agent_rules(&agents, agent).ok_or_else(|| {
        HarnessError::Protocol("opencode listed no agent permission rules".into())
    })?;
    Ok(session_rules(rules, forced_asks(permission)))
}

async fn find_session(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
) -> Result<Option<Value>, HarnessError> {
    match server.session_info(session_id, dir).await {
        Ok(info) => Ok(info),
        // 1.18 can fail its first directory-scoped request mid-migration, and the retry then succeeds.
        Err(error) => {
            tracing::debug!(target: "agent_harness::opencode", "session lookup failed, retrying once: {error}");
            tokio::time::sleep(Duration::from_millis(250)).await;
            server.session_info(session_id, dir).await
        }
    }
}

fn touches_session_rule_permissions(rules: &Value) -> bool {
    rules.as_array().into_iter().flatten().any(|rule| {
        rule.get("permission")
            .and_then(Value::as_str)
            .is_some_and(|permission| {
                SESSION_RULE_PERMISSIONS
                    .iter()
                    .any(|target| wildcard_matches(permission, target))
            })
    })
}

async fn run_session(session: Session) {
    let Session {
        server: boot,
        event_tx,
        controls,
        request,
        permission,
        interrupt_grace,
        kill_grace,
        known_commands,
        initial_native_command_selected,
    } = session;
    let RunControls {
        request_input,
        mut steering,
        interrupt,
    } = controls;
    let request_input = Arc::new(request_input);
    let directory = (!request.cwd.is_empty()).then(|| request.cwd.clone());
    let dir = directory.as_deref();
    let agent = request
        .model_options
        .get("agent")
        .and_then(Value::as_str)
        .filter(|agent| !agent.is_empty());

    let mut server = tokio::select! {
        result = boot => match result {
            Ok(server) => server,
            Err(error) => {
                steering.close();
                send_final(&event_tx, AgentEvent::Error { message: error.to_string() }).await;
                send_final(&event_tx, AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(error.to_string()),
                    session_id: None,
                }).await;
                return;
            }
        },
        _ = interrupt.cancelled() => {
            steering.close();
            send_final(&event_tx, AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            }).await;
            return;
        }
    };

    let setup = async {
        let protocol = server.protocol().await;
        let forced = forced_asks(permission);
        let mut rules_warning = None;
        let continues = request.resume.is_some() || request.fork.is_some();
        let rules = if protocol == Protocol::V1 && (!forced.is_empty() || continues) {
            match agent_session_rules(&server, dir, agent, permission).await {
                Ok(rules) => Some(rules),
                Err(error) => {
                    rules_warning = Some(format!(
                        "Couldn't read OpenCode's permission rules, so OpenCode's own rules apply: \
                         {error}"
                    ));
                    None
                }
            }
        } else {
            if protocol == Protocol::V2 && !forced.is_empty() {
                tracing::warn!(target: "agent_harness::opencode", "opencode 2.x has no session rules; its own config decides what asks");
            }
            None
        };
        let resume = match (&request.resume, &request.fork) {
            (None, Some(fork)) => Some(fork_session(&server, fork, dir).await?),
            (resume, _) => resume.clone(),
        };
        let resumed = match &resume {
            Some(resume) => find_session(&server, resume, dir).await?,
            None => None,
        };
        let session_id = match resumed {
            Some(info) => {
                let id = info
                    .get("id")
                    .and_then(Value::as_str)
                    .or(resume.as_deref())
                    .unwrap_or_default()
                    .to_owned();
                if protocol == Protocol::V2
                    && let Some(agent) = agent
                {
                    server
                        .post_json(
                            &format!("/api/session/{id}/agent"),
                            dir,
                            &json!({ "agent": agent }),
                        )
                        .await?;
                }
                let existing = info.get("permission").cloned().unwrap_or(Value::Null);
                if let Some(rules) = &rules {
                    let already_last = existing
                        .as_array()
                        .is_some_and(|existing| existing.ends_with(rules));
                    if !already_last
                        && (!forced.is_empty() || touches_session_rule_permissions(&existing))
                    {
                        server
                            .patch_json(
                                &format!("/session/{id}"),
                                dir,
                                &json!({ "permission": rules }),
                            )
                            .await?;
                    }
                }
                id
            }
            None => {
                let create_rules = rules.as_deref().filter(|_| !forced.is_empty());
                create_session(&server, dir, agent, create_rules).await?
            }
        };
        let providers = match server.provider_catalog(dir).await {
            Ok(providers) => providers,
            Err(error) => {
                tracing::debug!(target: "agent_harness::opencode", "provider catalog unavailable: {error}");
                ProviderCatalog::default()
            }
        };
        if protocol == Protocol::V2
            && let Some((provider, model_id)) = request
                .model
                .as_deref()
                .and_then(|model| model.split_once('/'))
        {
            let variant = pick_variant(&providers, provider, model_id, request.reasoning);
            server
                .set_model(&session_id, provider, model_id, variant.as_deref(), dir)
                .await?;
        }
        Ok::<_, HarnessError>((session_id, providers, rules_warning))
    };
    let (session_id, providers, rules_warning) = tokio::select! {
        result = setup => match result {
            Ok(value) => value,
            Err(error) => {
                steering.close();
                send_final(&event_tx, AgentEvent::Error { message: error.to_string() }).await;
                send_final(&event_tx, AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(error.to_string()),
                    session_id: None,
                }).await;
                server.shutdown(kill_grace).await;
                return;
            }
        },
        _ = interrupt.cancelled() => {
            steering.close();
            send_final(&event_tx, AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: None,
                session_id: None,
            }).await;
            server.shutdown(kill_grace).await;
            return;
        }
    };

    let model = request
        .model
        .as_deref()
        .and_then(|model| model.split_once('/'))
        .map(|(provider, model)| (provider.to_owned(), model.to_owned()));
    let variant = model.as_ref().and_then(|(provider, model_id)| {
        pick_variant(&providers, provider, model_id, request.reasoning)
    });
    let context_windows = context_windows(&providers);
    drop(providers);

    let mut assistant_message_id = new_message_id();
    if !send(
        &event_tx,
        AgentEvent::SessionStarted {
            harness: HarnessId::Opencode,
            model: request.model.clone().unwrap_or_default(),
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: session_id.clone(),
            assistant_message_id: assistant_message_id.clone(),
        },
    )
    .await
    {
        server.shutdown(kill_grace).await;
        return;
    }
    if let Some(message) = rules_warning
        && !send(&event_tx, AgentEvent::Error { message }).await
    {
        server.shutdown(kill_grace).await;
        return;
    }

    let command_wire = match known_commands {
        Some(wire) => wire,
        None => match server.commands_wire(dir).await {
            Ok(wire) => wire,
            Err(error) => {
                tracing::debug!(target: "agent_harness::opencode", "command catalog unavailable: {error}");
                Value::Null
            }
        },
    };
    let routable = command_names(&command_wire);
    let commands = commands_from_wire(&command_wire);
    if !commands.is_empty() && !send(&event_tx, AgentEvent::AvailableCommands { commands }).await {
        server.shutdown(kill_grace).await;
        return;
    }

    let (bus_tx, mut bus_rx) = mpsc::channel::<BusMessage>(256);
    let bus_handle = tokio::spawn(bus_task(
        server.client(),
        server.base.clone(),
        server.auth.clone(),
        server.protocol().await,
        bus_tx,
    ));

    macro_rules! end_before_turn {
        ($message:expr) => {{
            let message: String = $message;
            steering.close();
            send_final(
                &event_tx,
                AgentEvent::Error {
                    message: message.clone(),
                },
            )
            .await;
            send_final(
                &event_tx,
                AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(message),
                    session_id: Some(session_id.clone()),
                },
            )
            .await;
            bus_handle.abort();
            server.shutdown(kill_grace).await;
            return;
        }};
    }

    // The bus has no replay: a fast-failing turn can finish before a late subscription exists.
    let connect_wait = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(message) = bus_rx.recv().await {
            match message {
                BusMessage::Connected => return true,
                BusMessage::Disconnected => return false,
                BusMessage::Event(_) => {}
            }
        }
        false
    })
    .await;
    match connect_wait {
        Ok(true) => {}
        Ok(false) => end_before_turn!(format!(
            "Couldn't connect to opencode's event stream. {}",
            crate::crash_message(
                "opencode serve",
                server.child_exit_status(),
                &server.stderr_tail
            )
        )),
        Err(_) => {
            tracing::debug!(target: "agent_harness::opencode", "event bus not connected within 15s; prompting anyway")
        }
    }
    let stall = stall_bound();
    let (failure_tx, mut failure_rx) = mpsc::unbounded_channel();
    macro_rules! target {
        () => {
            PromptTarget {
                server: &server,
                session_id: &session_id,
                directory: dir,
                routable: &routable,
                failure_tx: &failure_tx,
            }
        };
    }
    let mut turn_generation = 0_u64;
    if let Err(error) = post_prompt(
        &target!(),
        &request.prompt,
        initial_native_command_selected,
        turn_generation,
        TurnSpec {
            model: model.as_ref(),
            variant: variant.as_deref(),
            attachments: &request.attachments,
        },
    )
    .await
    {
        end_before_turn!(error.to_string());
    }
    let mut turn = TurnState::begin(stall);

    let mut main_feed = SessionFeed::default();
    let mut children: HashMap<String, ChildRun> = HashMap::new();
    let mut pending_spawns: VecDeque<PendingSpawn> = VecDeque::new();
    let mut unbound_children: HashMap<String, String> = HashMap::new();
    let mut queued_steers: VecDeque<QueuedSteer> = VecDeque::new();
    let mut steering_open = true;
    let mut interrupt_requested = false;
    let mut pending_usage: Option<AgentEvent> = None;
    let mut done_sent = false;
    // Only idle ends an abort, so unlike the stall bound this is not disarmed by activity.
    let mut abort_deadline: Option<tokio::time::Instant> = None;

    // Closing first makes the chat's next send fail instead of vanishing into an ending run.
    macro_rules! finish {
        ($status:expr, $error:expr) => {{
            steering.close();
            send_final(
                &event_tx,
                AgentEvent::Done {
                    status: $status,
                    result: None,
                    error: $error,
                    session_id: Some(session_id.clone()),
                },
            )
            .await;
            done_sent = true;
        }};
    }

    macro_rules! fail_turn {
        ($message:expr) => {{
            let message: String = $message;
            send_final(
                &event_tx,
                AgentEvent::Error {
                    message: message.clone(),
                },
            )
            .await;
            settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
            finish!(DoneStatus::Errored, Some(message));
        }};
    }

    macro_rules! acknowledge_steers {
        ($count:expr, $label:lifetime) => {{
            for _ in 0..$count {
                let (previous, next) = rotate(&mut assistant_message_id);
                if !send(
                    &event_tx,
                    AgentEvent::Steered {
                        assistant_message_id: Some(previous),
                        next_assistant_message_id: Some(next),
                    },
                )
                .await
                {
                    break $label;
                }
            }
        }};
    }

    // A macro so `break`/`continue` act on the caller's loop.
    macro_rules! settle_idle {
        ($label:lifetime) => {{
            if !turn.active {
                continue $label;
            }
            turn.active = false;
            if let Some(usage) = pending_usage.take()
                && !interrupt_requested
                && !send(&event_tx, usage).await
            {
                break $label;
            }
            if interrupt_requested {
                settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                finish!(DoneStatus::Interrupted, None);
                break $label;
            }
            if let Some(first) = queued_steers.pop_front() {
                turn_generation = turn_generation.wrapping_add(1);
                let native_command_selected = first.native_command_selected;
                let mut batch = vec![first];
                if !native_command_selected {
                    while queued_steers
                        .front()
                        .is_some_and(|steer| !steer.native_command_selected)
                    {
                        if let Some(next) = queued_steers.pop_front() {
                            batch.push(next);
                        }
                    }
                }
                let steer = batch
                    .iter()
                    .map(|steer| steer.prompt.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let attachments: Vec<String> = batch
                    .iter()
                    .flat_map(|steer| steer.attachments.iter().cloned())
                    .collect();
                match post_prompt(
                    &target!(),
                    &steer,
                    native_command_selected,
                    turn_generation,
                    TurnSpec {
                        model: model.as_ref(),
                        variant: variant.as_deref(),
                        attachments: &attachments,
                    },
                )
                .await
                {
                    Ok(()) => {
                        acknowledge_steers!(batch.len(), $label);
                        turn = TurnState::begin(stall);
                        continue $label;
                    }
                    Err(error) => {
                        send_final(&event_tx, AgentEvent::Error {
                            message: error.to_string(),
                        }).await;
                        turn.error = Some(error.to_string());
                        turn.aborted_for_retry = true;
                    }
                }
            }
            let (previous, _next) = rotate(&mut assistant_message_id);
            if !send(&event_tx, AgentEvent::AssistantMessageCompleted {
                assistant_message_id: previous,
            }).await {
                break $label;
            }
            let errored = turn.aborted_for_retry
                || (turn.error.is_some() && !turn.saw_content);
            if errored || !steering_open {
                finish!(
                    if errored { DoneStatus::Errored } else { DoneStatus::Completed },
                    if errored { turn.error.clone() } else { None }
                );
                break $label;
            }
            send_final(&event_tx, AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(session_id.clone()),
            }).await;
            done_sent = true;
            // Closing here would race the chat's next queued prompt into a dying run.
            continue $label;
        }};
    }

    macro_rules! maybe_preempt {
        () => {{
            if turn.active
                && !turn.preempted
                && !interrupt_requested
                && !queued_steers.is_empty()
                && turn.open_tools.is_empty()
            {
                turn.preempted = true;
                turn.idle_ready = true;
                let abort = tokio::time::timeout(
                    Duration::from_secs(5),
                    server.abort_session(&session_id, dir),
                )
                .await;
                if !matches!(abort, Ok(Ok(_))) {
                    tracing::warn!(
                        target: "agent_harness::opencode",
                        "steer preempt abort failed; delivering at turn end"
                    );
                }
            }
        }};
    }

    'main: loop {
        let deadline = abort_deadline.or_else(|| {
            (turn.active && !turn.saw_activity)
                .then_some(turn.stall_deadline)
                .flatten()
        });
        let stall_sleep = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            biased;

            _ = event_tx.closed() => break 'main,

            _ = interrupt.cancelled(), if !interrupt_requested => {
                interrupt_requested = true;
                if turn.active {
                    turn.idle_ready = true;
                    let abort = tokio::time::timeout(
                        Duration::from_secs(5),
                        server.abort_session(&session_id, dir),
                    )
                    .await;
                    if !matches!(abort, Ok(Ok(_))) {
                        settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                        finish!(DoneStatus::Interrupted, None);
                        break 'main;
                    }
                    abort_deadline = Some(tokio::time::Instant::now() + interrupt_grace);
                } else {
                    finish!(DoneStatus::Interrupted, None);
                    break 'main;
                }
            }

            failure = failure_rx.recv() => {
                let Some(failure) = failure else { continue 'main; };
                if failure.generation != turn_generation || !turn.active || interrupt_requested {
                    tracing::debug!(
                        target: "agent_harness::opencode",
                        failed_generation = failure.generation,
                        active_generation = turn_generation,
                        "ignoring an HTTP failure from a retired turn"
                    );
                    continue 'main;
                }
                fail_turn!(failure.message);
                break 'main;
            }

            steer = steering.recv(), if steering_open => {
                match steer {
                    Some(steer) => {
                        let (prompt, native_command_selected) =
                            plan_prompt(&steer.prompt, &steer.skills, &steer.attachments);
                        if turn.active {
                            queued_steers.push_back(QueuedSteer {
                                prompt,
                                native_command_selected,
                                attachments: steer.attachments,
                            });
                            maybe_preempt!();
                        } else {
                            turn_generation = turn_generation.wrapping_add(1);
                            match post_prompt(
                                &target!(),
                                &prompt,
                                native_command_selected,
                                turn_generation,
                                TurnSpec {
                                    model: model.as_ref(),
                                    variant: variant.as_deref(),
                                    attachments: &steer.attachments,
                                },
                            )
                            .await
                            {
                                Ok(()) => {
                                    acknowledge_steers!(1, 'main);
                                    turn = TurnState::begin(stall);
                                }
                                Err(error) => {
                                    fail_turn!(error.to_string());
                                    break 'main;
                                }
                            }
                        }
                    }
                    None => {
                        steering_open = false;
                        if !turn.active {
                            break 'main;
                        }
                    }
                }
            }

            _ = stall_sleep => {
                if abort_deadline.is_some() {
                    settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                    finish!(DoneStatus::Interrupted, None);
                    break 'main;
                }
                if let Err(error) = server.abort_session(&session_id, dir).await {
                    tracing::debug!(target: "agent_harness::opencode", "abort after stall failed: {error}");
                }
                fail_turn!(format!(
                    "opencode made no progress for {}s after the prompt. {STALL_HINT}",
                    stall.unwrap_or(DEFAULT_STALL_BOUND).as_secs()
                ));
                break 'main;
            }

            _ = tokio::time::sleep_until(turn.status_poll.unwrap_or_else(tokio::time::Instant::now)),
                if turn.status_poll.is_some() && turn.active => {
                match tokio::time::timeout(Duration::from_secs(2), server.session_running(&session_id, dir)).await {
                    Ok(Ok(false)) => settle_idle!('main),
                    _ => {
                        turn.status_backoff = (turn.status_backoff * 2).min(Duration::from_secs(2));
                        turn.status_poll = Some(tokio::time::Instant::now() + turn.status_backoff);
                    }
                }
            }

            message = bus_rx.recv() => {
                let Some(message) = message else { break 'main };
                match message {
                    BusMessage::Connected => {
                        // A reconnect may have swallowed our idle; an unknown status keeps the turn running.
                        if turn.active
                            && !server
                                .session_running(&session_id, dir)
                                .await
                                .unwrap_or(true)
                        {
                            settle_idle!('main);
                        }
                    }
                    BusMessage::Disconnected => {
                        let crashed = server.child_exit_status();
                        let message = crate::crash_message("opencode serve", crashed, &server.stderr_tail);
                        if turn.active {
                            send_final(&event_tx, AgentEvent::Error { message: message.clone() }).await;
                        }
                        settle_children(&mut children, &event_tx, DoneStatus::Interrupted).await;
                        if interrupt_requested {
                            finish!(DoneStatus::Interrupted, None);
                        } else {
                            finish!(DoneStatus::Errored, Some(message));
                        }
                        break 'main;
                    }
                    BusMessage::Event(event) => {
                        if interrupt_requested {
                            // Only the idle acknowledgement matters once aborted; late content and usage drop.
                            if is_own_idle(&event, &session_id) {
                                settle_idle!('main);
                            }
                            continue;
                        }
                        let outcome = handle_bus_event(BusContext {
                            event: &event,
                            session_id: &session_id,
                            server: &server,
                            dir,
                            event_tx: &event_tx,
                            request_input: &request_input,
                            permission,
                            generation: turn_generation,
                            failure_tx: &failure_tx,
                            main_feed: &mut main_feed,
                            children: &mut children,
                            pending_spawns: &mut pending_spawns,
                            unbound_children: &mut unbound_children,
                            turn: &mut turn,
                            pending_usage: &mut pending_usage,
                            context_windows: &context_windows,
                        }).await;
                        match outcome {
                            BusOutcome::Continue => maybe_preempt!(),
                            BusOutcome::ConsumerGone => break 'main,
                            BusOutcome::TurnIdle => settle_idle!('main),
                            BusOutcome::TurnInterrupted
                                if (turn.preempted || turn.aborted_for_retry)
                                    && !interrupt_requested =>
                            {
                                settle_idle!('main)
                            }
                            BusOutcome::TurnInterrupted => {
                                interrupt_requested = true;
                                settle_idle!('main);
                            }
                        }
                    }
                }
            }
        }
    }

    if !done_sent {
        tracing::debug!(target: "agent_harness::opencode", "run loop ended without settling");
    }
    bus_handle.abort();
    server.shutdown(kill_grace).await;
}

fn is_own_idle(event: &Value, session_id: &str) -> bool {
    let payload = event.get("payload").unwrap_or(event);
    let kind = payload.get("type").and_then(Value::as_str);
    let ours = payload
        .pointer("/properties/sessionID")
        .and_then(Value::as_str)
        == Some(session_id);
    ours && (kind == Some("session.idle")
        || kind == Some("session.interrupted")
        || (kind == Some("session.status")
            && payload
                .pointer("/properties/status/type")
                .and_then(Value::as_str)
                == Some("idle")))
}

async fn fork_session(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
) -> Result<String, HarnessError> {
    if server.protocol().await == Protocol::V2 {
        return Err(HarnessError::Protocol(
            "OpenCode 2.x can't fork a session".into(),
        ));
    }
    let forked = server
        .post_json(&format!("/session/{session_id}/fork"), dir, &json!({}))
        .await?;
    forked
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| HarnessError::Protocol("opencode session fork returned no id".into()))
}

async fn create_session(
    server: &Server,
    dir: Option<&str>,
    agent: Option<&str>,
    rules: Option<&[Value]>,
) -> Result<String, HarnessError> {
    let missing_id = || HarnessError::Protocol("opencode session create returned no id".into());
    if server.protocol().await == Protocol::V2 {
        // 2.x ignores the directory header here and reads it from the body.
        let mut body = match dir {
            Some(dir) => json!({ "location": { "directory": dir } }),
            None => json!({}),
        };
        if let Some(agent) = agent {
            body["agent"] = json!(agent);
        }
        let created = server.post_json("/api/session", dir, &body).await?;
        return created
            .pointer("/data/id")
            .or_else(|| created.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(missing_id);
    }
    let body = match rules {
        Some(rules) => json!({ "permission": rules }),
        None => json!({}),
    };
    let mut retried = false;
    loop {
        let (status, text) = server.post_json_raw("/session", dir, &body).await?;
        if status.is_success() {
            return serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|created| created.get("id").and_then(Value::as_str).map(str::to_owned))
                .ok_or_else(missing_id);
        }
        // 1.18 can fail its first directory-scoped request mid-migration, and the retry then succeeds.
        if !retried && status.is_server_error() {
            retried = true;
            tracing::debug!(target: "agent_harness::opencode", "POST /session answered {status}; retrying once");
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        return Err(HarnessError::Protocol(post_error_message(
            "/session", status, &text,
        )));
    }
}

fn prompt_body(
    prompt: &str,
    model: Option<(&str, &str)>,
    variant: Option<&str>,
    attachments: &[String],
) -> Value {
    let mut parts = vec![json!({ "type": "text", "text": prompt })];
    for path in attachments {
        parts.push(json!({
            "type": "file",
            "mime": mime_for(path),
            "filename": file_name(path),
            "url": format!("file://{path}"),
        }));
    }
    let mut body = serde_json::Map::new();
    body.insert("parts".into(), Value::Array(parts));
    if let Some((provider, model)) = model {
        body.insert(
            "model".into(),
            json!({ "providerID": provider, "modelID": model }),
        );
    }
    if let Some(variant) = variant {
        body.insert("variant".into(), Value::String(variant.to_owned()));
    }
    Value::Object(body)
}

/// On 2.x the model and variant ride the session, not the prompt.
fn prompt_body_v2(prompt: &str, attachments: &[String]) -> Value {
    let files: Vec<Value> = attachments
        .iter()
        .map(|path| json!({ "uri": format!("file://{path}"), "name": file_name(path) }))
        .collect();
    json!({ "text": prompt, "files": files })
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn mime_for(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn command_body_v2(
    version: Option<&ServerVersion>,
    name: &str,
    text: &str,
    attachments: &[String],
) -> Value {
    if ServerVersion::at_least(version, (2, 0, 4)) {
        let mut body = json!({ "name": name, "text": text });
        if !attachments.is_empty() {
            body["files"] = prompt_body_v2(text, attachments)["files"].clone();
        }
        body
    } else {
        json!({ "command": name, "text": text })
    }
}

/// The server never parses slash text out of a prompt, so a known leading command goes to the command endpoint.
async fn post_prompt(
    target: &PromptTarget<'_>,
    prompt: &str,
    native_command_selected: bool,
    generation: u64,
    spec: TurnSpec<'_>,
) -> Result<(), HarnessError> {
    let server = target.server;
    let session_id = target.session_id;
    let protocol = server.protocol().await;
    if let Some((name, arguments)) =
        native_command_request(prompt, target.routable, native_command_selected)?
    {
        if !spec.attachments.is_empty() {
            return Err(HarnessError::Protocol(
                "OpenCode commands cannot include attachments; send them in a separate prompt"
                    .into(),
            ));
        }
        let (path, body) = match protocol {
            Protocol::V1 => (
                format!("/session/{session_id}/command"),
                command_body_v1(name, arguments, spec.model, spec.variant),
            ),
            Protocol::V2 => (
                format!("/api/session/{session_id}/command"),
                command_body_v2(server.version.get(), name, arguments, spec.attachments),
            ),
        };
        let server = server.handle();
        let directory = target.directory.map(str::to_owned);
        let failure_tx = target.failure_tx.clone();
        // The command endpoint blocks for the whole turn, so it gets no call timeout.
        tokio::spawn(async move {
            let failure = match server
                .post_request(&path, directory.as_deref(), &body)
                .await
            {
                Err(error) => Some(error.to_string()),
                Ok(request) => match request.send().await {
                    Ok(response) if response.status().is_success() => None,
                    Ok(response) => {
                        let status = response.status();
                        let body = response.text().await.unwrap_or_default();
                        Some(post_error_message(&path, status, &body))
                    }
                    Err(error) => Some(format!("opencode POST {path}: {error}")),
                },
            };
            if let Some(message) = failure {
                report_turn_failure(&failure_tx, generation, message);
            }
        });
        return Ok(());
    }
    let (path, body) = match protocol {
        Protocol::V1 => (
            format!("/session/{session_id}/prompt_async"),
            prompt_body(
                prompt,
                spec.model
                    .map(|(provider, model)| (provider.as_str(), model.as_str())),
                spec.variant,
                spec.attachments,
            ),
        ),
        Protocol::V2 => (
            format!("/api/session/{session_id}/prompt"),
            prompt_body_v2(prompt, spec.attachments),
        ),
    };
    let server = server.handle();
    let failure_tx = target.failure_tx.clone();
    let directory = target.directory.map(str::to_owned);
    // The bus owns turn completion, so a slow acknowledgement must not block the loop.
    tokio::spawn(async move {
        if let Err(error) = server.post_json(&path, directory.as_deref(), &body).await {
            report_turn_failure(&failure_tx, generation, error.to_string());
        }
    });
    Ok(())
}

fn report_turn_failure(
    failure_tx: &mpsc::UnboundedSender<TurnFailure>,
    generation: u64,
    message: String,
) {
    if failure_tx
        .send(TurnFailure {
            generation,
            message,
        })
        .is_err()
    {
        tracing::debug!(target: "agent_harness::opencode", "turn failed after the run ended");
    }
}

fn command_body_v1(
    name: &str,
    arguments: &str,
    model: Option<&(String, String)>,
    variant: Option<&str>,
) -> Value {
    let mut body = json!({ "command": name, "arguments": arguments });
    if let Some((provider, model)) = model {
        body["model"] = json!(format!("{provider}/{model}"));
    }
    if let Some(variant) = variant {
        body["variant"] = json!(variant);
    }
    body
}

/// A selected skill whose command vanished fails loudly; raw slash text falls back to a plain prompt.
fn native_command_request<'a>(
    prompt: &'a str,
    routable: &[String],
    selected: bool,
) -> Result<Option<(&'a str, &'a str)>, HarnessError> {
    let Some((name, arguments)) = crate::leading_command(prompt) else {
        return if selected {
            Err(HarnessError::Protocol(
                "The selected OpenCode command is no longer available in this project".into(),
            ))
        } else {
            Ok(None)
        };
    };
    if routable.iter().any(|command| command == name) {
        Ok(Some((name, arguments)))
    } else if selected {
        Err(HarnessError::Protocol(format!(
            "The selected OpenCode command /{name} is no longer available in this project"
        )))
    } else {
        Ok(None)
    }
}

enum BusOutcome {
    Continue,
    TurnIdle,
    TurnInterrupted,
    ConsumerGone,
}

struct BusContext<'a> {
    event: &'a Value,
    session_id: &'a str,
    server: &'a Server,
    dir: Option<&'a str>,
    event_tx: &'a EventSender,
    request_input: &'a Arc<RequestInput>,
    permission: PermissionMode,
    generation: u64,
    failure_tx: &'a mpsc::UnboundedSender<TurnFailure>,
    main_feed: &'a mut SessionFeed,
    children: &'a mut HashMap<String, ChildRun>,
    pending_spawns: &'a mut VecDeque<PendingSpawn>,
    unbound_children: &'a mut HashMap<String, String>,
    turn: &'a mut TurnState,
    pending_usage: &'a mut Option<AgentEvent>,
    context_windows: &'a HashMap<String, u64>,
}

async fn settle_children(
    children: &mut HashMap<String, ChildRun>,
    event_tx: &EventSender,
    status: DoneStatus,
) {
    for child in children.values_mut() {
        if child.done {
            continue;
        }
        child.done = true;
        send_final(
            event_tx,
            tag(
                &child.parent_tool_use_id,
                AgentEvent::Done {
                    status,
                    result: None,
                    error: None,
                    session_id: None,
                },
            ),
        )
        .await;
    }
}

fn owns_session(
    session: &str,
    session_id: &str,
    children: &HashMap<String, ChildRun>,
    unbound_children: &HashMap<String, String>,
) -> bool {
    session == session_id
        || children.get(session).is_some_and(|child| !child.done)
        || unbound_children.contains_key(session)
}

async fn handle_bus_event(context: BusContext<'_>) -> BusOutcome {
    let BusContext {
        event,
        session_id,
        server,
        dir,
        event_tx,
        request_input,
        permission,
        generation,
        failure_tx,
        main_feed,
        children,
        pending_spawns,
        unbound_children,
        turn,
        pending_usage,
        context_windows,
    } = context;
    // `/global/event` wraps the payload; a bare `/event` feed does not.
    let payload = event.get("payload").unwrap_or(event);
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "sync" || kind.is_empty() {
        return BusOutcome::Continue;
    }
    let properties = payload.get("properties").unwrap_or(&Value::Null);
    let event_session = properties
        .get("sessionID")
        .and_then(Value::as_str)
        .or_else(|| {
            properties
                .get("info")
                .and_then(|info| info.get("sessionID").or_else(|| info.get("id")))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            properties
                .get("part")
                .and_then(|part| part.get("sessionID"))
                .and_then(Value::as_str)
        });

    let is_ours = event_session == Some(session_id);
    if is_ours && kind == "session.interrupted" {
        return BusOutcome::TurnInterrupted;
    }
    let status = properties
        .get("status")
        .and_then(|status| status.get("type"))
        .and_then(Value::as_str);
    if is_ours && (kind == "session.idle" || (kind == "session.status" && status == Some("idle"))) {
        // One completion can arrive in both idle encodings; the second must not settle the next prompt.
        if turn.idle_ready || turn.error.is_some() {
            turn.idle_confirmations += 1;
            if turn.idle_confirmations >= 2 {
                return BusOutcome::TurnIdle;
            }
        }
        turn.status_poll = Some(tokio::time::Instant::now() + turn.status_backoff);
        return BusOutcome::Continue;
    }
    if is_ours && turn.active {
        turn.idle_confirmations = 0;
        turn.status_poll = None;
        turn.status_backoff = Duration::from_millis(100);
        turn.note_activity();
    }

    match kind {
        "session.status" if is_ours => {
            let status = properties.get("status").unwrap_or(&Value::Null);
            match status.get("type").and_then(Value::as_str) {
                Some("busy") => turn.idle_ready = true,
                Some("retry") => {
                    turn.idle_ready = true;
                    let attempt = status.get("attempt").and_then(Value::as_u64).unwrap_or(0);
                    let message = status
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("provider error");
                    if attempt >= RETRY_ABORT_ATTEMPT && !turn.aborted_for_retry {
                        turn.aborted_for_retry = true;
                        turn.error = Some(format!(
                            "the provider kept failing after {attempt} attempts: {message}"
                        ));
                        let chip = format!(
                            "Giving up after {attempt} provider retries: {message}. {STALL_HINT}"
                        );
                        if !send(event_tx, AgentEvent::Error { message: chip }).await {
                            return BusOutcome::ConsumerGone;
                        }
                        if let Err(error) = server.abort_session(session_id, dir).await {
                            tracing::debug!(target: "agent_harness::opencode", "abort after retries failed: {error}");
                        }
                    } else if attempt >= RETRY_REPORT_ATTEMPT && !turn.retry_reported {
                        turn.retry_reported = true;
                        let chip = format!(
                            "The provider is failing and opencode is retrying (attempt \
                             {attempt}): {message}"
                        );
                        if !send(event_tx, AgentEvent::Error { message: chip }).await {
                            return BusOutcome::ConsumerGone;
                        }
                    }
                }
                _ => {}
            }
            BusOutcome::Continue
        }
        "session.error" | "session.warning" => {
            // An error without a session id is a global provider failure that still concerns us.
            if event_session.is_some() && !is_ours {
                return BusOutcome::Continue;
            }
            let error = properties.get("error").unwrap_or(&Value::Null);
            let name = error.get("name").and_then(Value::as_str).unwrap_or("");
            if name == "MessageAbortedError" {
                return BusOutcome::Continue;
            }
            let message = error
                .pointer("/data/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if name.is_empty() {
                        "opencode reported an error".to_owned()
                    } else {
                        name.to_owned()
                    }
                });
            // opencode reports one failure twice, once bare and once with an exception prefix.
            let first_line = |text: &str| text.lines().next().unwrap_or(text).trim().to_owned();
            let line = first_line(&message);
            let duplicate = turn.error.as_deref().is_some_and(|previous| {
                let previous = first_line(previous);
                !line.is_empty() && (previous.contains(&line) || line.contains(&previous))
            });
            if kind == "session.error" {
                turn.error = Some(message.clone());
            }
            if !duplicate && !send(event_tx, AgentEvent::Error { message }).await {
                return BusOutcome::ConsumerGone;
            }
            BusOutcome::Continue
        }
        "session.created" => {
            let info = properties.get("info").unwrap_or(&Value::Null);
            // Only direct children bind; a grandchild renders inside its own parent.
            if info.get("parentID").and_then(Value::as_str) != Some(session_id) {
                return BusOutcome::Continue;
            }
            let Some(child_id) = info.get("id").and_then(Value::as_str) else {
                return BusOutcome::Continue;
            };
            if children.contains_key(child_id) {
                return BusOutcome::Continue;
            }
            let title = info
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !bind_child(children, pending_spawns, child_id, title) {
                unbound_children.insert(child_id.to_owned(), title.to_owned());
            }
            BusOutcome::Continue
        }
        "message.updated" => {
            let info = properties.get("info").unwrap_or(&Value::Null);
            let (Some(session), Some(message), Some(role)) = (
                info.get("sessionID").and_then(Value::as_str),
                info.get("id").and_then(Value::as_str),
                info.get("role").and_then(Value::as_str),
            ) else {
                return BusOutcome::Continue;
            };
            if session == session_id {
                main_feed
                    .message_is_assistant
                    .entry(message.to_owned())
                    .or_insert(role == "assistant");
                if role == "assistant"
                    && let Some(tokens) = info.get("tokens")
                {
                    if let Some(usage) = context_usage_event(info, context_windows)
                        && !send(event_tx, usage).await
                    {
                        return BusOutcome::ConsumerGone;
                    }
                    let input = tokens.get("input").and_then(Value::as_u64).unwrap_or(0);
                    let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
                    if input > 0 || output > 0 {
                        *pending_usage = Some(AgentEvent::Usage {
                            input_tokens: input,
                            output_tokens: output,
                        });
                    }
                }
                let events = replay_pending(main_feed, message, true, turn);
                return forward(event_tx, events).await;
            }
            if let Some(child) = children.get_mut(session) {
                child
                    .feed
                    .message_is_assistant
                    .entry(message.to_owned())
                    .or_insert(role == "assistant");
                // A new user message on a settled child is a steer resuming it.
                if role == "user" && child.done {
                    child.done = false;
                }
                let events = replay_pending(&mut child.feed, message, false, turn);
                let parent = child.parent_tool_use_id.clone();
                let tagged = events.into_iter().map(|ev| tag(&parent, ev)).collect();
                return forward(event_tx, tagged).await;
            }
            BusOutcome::Continue
        }
        "message.part.updated" => {
            let part = properties.get("part").unwrap_or(&Value::Null);
            let Some(session) = part.get("sessionID").and_then(Value::as_str) else {
                return BusOutcome::Continue;
            };
            if session == session_id {
                let mut events = part_snapshot_events(
                    main_feed,
                    part,
                    true,
                    Some((children, pending_spawns, unbound_children)),
                );
                mark_content(turn, &events);
                if let Some((child_session, failed)) = task_completion(part) {
                    let part_id = part.get("id").and_then(Value::as_str).unwrap_or("");
                    let child = if children.contains_key(&child_session) {
                        children.get_mut(&child_session)
                    } else {
                        children
                            .values_mut()
                            .find(|child| child.parent_tool_use_id == part_id)
                    };
                    if let Some(child) = child
                        && !child.done
                    {
                        child.done = true;
                        events.push(tag(
                            &child.parent_tool_use_id,
                            AgentEvent::Done {
                                status: if failed {
                                    DoneStatus::Errored
                                } else {
                                    DoneStatus::Completed
                                },
                                result: None,
                                error: None,
                                session_id: None,
                            },
                        ));
                    }
                }
                return forward(event_tx, events).await;
            }
            if let Some(child) = children.get_mut(session) {
                if child.done {
                    return BusOutcome::Continue;
                }
                let events = part_snapshot_events(&mut child.feed, part, false, None);
                let parent = child.parent_tool_use_id.clone();
                let tagged = events.into_iter().map(|ev| tag(&parent, ev)).collect();
                return forward(event_tx, tagged).await;
            }
            BusOutcome::Continue
        }
        "message.part.delta" => {
            let (Some(session), Some(part_id), Some(delta)) = (
                properties.get("sessionID").and_then(Value::as_str),
                properties.get("partID").and_then(Value::as_str),
                properties.get("delta").and_then(Value::as_str),
            ) else {
                return BusOutcome::Continue;
            };
            if properties.get("field").and_then(Value::as_str) != Some("text") {
                return BusOutcome::Continue;
            }
            if session == session_id {
                let events = part_delta_events(main_feed, properties, part_id, delta);
                mark_content(turn, &events);
                return forward(event_tx, events).await;
            }
            if let Some(child) = children.get_mut(session) {
                if child.done {
                    return BusOutcome::Continue;
                }
                let events = part_delta_events(&mut child.feed, properties, part_id, delta);
                let parent = child.parent_tool_use_id.clone();
                let tagged = events.into_iter().map(|ev| tag(&parent, ev)).collect();
                return forward(event_tx, tagged).await;
            }
            BusOutcome::Continue
        }
        "permission.asked" => {
            // The bus carries every session; never answer a request we don't own.
            let Some(session) = event_session
                .filter(|session| owns_session(session, session_id, children, unbound_children))
            else {
                return BusOutcome::Continue;
            };
            let Some(request_id) = properties.get("id").and_then(Value::as_str) else {
                return BusOutcome::Continue;
            };
            let server = server.handle();
            let session = session.to_owned();
            let request_id = request_id.to_owned();
            let directory = dir.map(str::to_owned);
            let failure_tx = failure_tx.clone();
            let kind = properties
                .get("permission")
                .or_else(|| properties.get("action"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if auto_approves(permission, kind) {
                tokio::spawn(async move {
                    answer_permission(
                        &server,
                        &session,
                        &request_id,
                        directory.as_deref(),
                        "once",
                        &failure_tx,
                        generation,
                    )
                    .await;
                });
                return BusOutcome::Continue;
            }
            let question = permission_question(properties);
            if !send(
                event_tx,
                AgentEvent::InputRequested {
                    request_id: request_id.clone(),
                    questions: vec![question.clone()],
                },
            )
            .await
            {
                return BusOutcome::ConsumerGone;
            }
            let answer = (request_input)(vec![question.clone()]);
            let event_tx = event_tx.clone();
            tokio::spawn(async move {
                // A dropped answer must deny, never allow.
                let answers = answer.await.unwrap_or_default();
                let allowed = answers.iter().any(|answer| {
                    answer.question_id == question.id
                        && answer.labels.iter().any(|label| {
                            label.eq_ignore_ascii_case(crate::claude::PERMISSION_ALLOW)
                        })
                });
                let reply = if allowed { "once" } else { "reject" };
                answer_permission(
                    &server,
                    &session,
                    &request_id,
                    directory.as_deref(),
                    reply,
                    &failure_tx,
                    generation,
                )
                .await;
                send_final(&event_tx, AgentEvent::InputResolved { request_id }).await;
            });
            BusOutcome::Continue
        }
        "question.asked" => {
            if !event_session.is_some_and(|session| {
                owns_session(session, session_id, children, unbound_children)
            }) {
                return BusOutcome::Continue;
            }
            let Some(request_id) = properties.get("id").and_then(Value::as_str) else {
                return BusOutcome::Continue;
            };
            let questions = map_questions(properties);
            if questions.is_empty() {
                return BusOutcome::Continue;
            }
            if !send(
                event_tx,
                AgentEvent::InputRequested {
                    request_id: request_id.to_owned(),
                    questions: questions.clone(),
                },
            )
            .await
            {
                return BusOutcome::ConsumerGone;
            }
            let answer = (request_input)(questions.clone());
            let server = server.handle();
            let directory = dir.map(str::to_owned);
            let request_id = request_id.to_owned();
            let event_tx = event_tx.clone();
            tokio::spawn(async move {
                let reply = match answer.await {
                    Ok(answers) => {
                        let ordered: Vec<Vec<String>> = questions
                            .iter()
                            .map(|question| {
                                answers
                                    .iter()
                                    .find(|answer| answer.question_id == question.id)
                                    .map(|answer| answer.labels.clone())
                                    .unwrap_or_default()
                            })
                            .collect();
                        server
                            .post_json(
                                &format!("/question/{request_id}/reply"),
                                directory.as_deref(),
                                &json!({ "answers": ordered }),
                            )
                            .await
                    }
                    Err(_) => {
                        server
                            .post_json(
                                &format!("/question/{request_id}/reject"),
                                directory.as_deref(),
                                &Value::Null,
                            )
                            .await
                    }
                };
                if let Err(error) = reply {
                    tracing::debug!(target: "agent_harness::opencode", "question reply failed: {error}");
                }
                send_final(&event_tx, AgentEvent::InputResolved { request_id }).await;
            });
            BusOutcome::Continue
        }
        _ => BusOutcome::Continue,
    }
}

/// opencode waits on an unanswered request forever, so a failed reply must end the turn.
async fn answer_permission(
    server: &Server,
    session: &str,
    request_id: &str,
    directory: Option<&str>,
    reply: &str,
    failure_tx: &mpsc::UnboundedSender<TurnFailure>,
    generation: u64,
) {
    let mut result = server
        .reply_permission(session, request_id, directory, reply)
        .await;
    if result.is_err() {
        tokio::time::sleep(Duration::from_millis(250)).await;
        result = server
            .reply_permission(session, request_id, directory, reply)
            .await;
    }
    if let Err(error) = result {
        report_turn_failure(
            failure_tx,
            generation,
            format!("Couldn't answer OpenCode's permission request: {error}"),
        );
    }
}

async fn forward(event_tx: &EventSender, events: Vec<AgentEvent>) -> BusOutcome {
    for event in events {
        if event_tx.send(Ok(event)).await.is_err() {
            return BusOutcome::ConsumerGone;
        }
    }
    BusOutcome::Continue
}

fn mark_content(turn: &mut TurnState, events: &[AgentEvent]) {
    for event in events {
        match event {
            AgentEvent::ToolCall { id, .. } => {
                turn.open_tools.insert(id.clone());
            }
            AgentEvent::ToolResult { id, .. } => {
                turn.open_tools.remove(id);
            }
            _ => {}
        }
    }
    if turn.active
        && events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::TextDelta { .. }
                    | AgentEvent::ReasoningDelta { .. }
                    | AgentEvent::ToolCall { .. }
            )
        })
    {
        turn.saw_content = true;
    }
}

fn replay_pending(
    feed: &mut SessionFeed,
    message: &str,
    is_main: bool,
    turn: &mut TurnState,
) -> Vec<AgentEvent> {
    let (held, kept): (Vec<Value>, Vec<Value>) = std::mem::take(&mut feed.parts_awaiting_role)
        .into_iter()
        .partition(|part| part.get("messageID").and_then(Value::as_str) == Some(message));
    feed.parts_awaiting_role = kept;
    let events: Vec<AgentEvent> = held
        .iter()
        .flat_map(|part| part_snapshot_events(feed, part, is_main, None))
        .collect();
    if is_main {
        mark_content(turn, &events);
    }
    events
}
