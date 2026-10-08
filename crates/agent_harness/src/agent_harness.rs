//! Portions are MIT licensed, see LICENSE-THIRD-PARTY.

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio::sync::{mpsc, oneshot};
pub use tokio_util::sync::CancellationToken;

pub use proto::*;

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("harness binary not found: {0}")]
    NotInstalled(String),
    #[error("harness protocol error: {0}")]
    Protocol(String),
    #[error(transparent)]
    Discovery(#[from] CatalogFailure),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub struct SteerMessage {
    pub prompt: String,
    pub message_id: Option<String>,
    /// Absolute image file paths.
    pub attachments: Vec<String>,
    pub skills: Vec<SkillRef>,
}

pub struct RunControls {
    pub request_input: Box<
        dyn Fn(Vec<UserInputQuestion>) -> oneshot::Receiver<Vec<UserInputAnswer>> + Send + Sync,
    >,
    pub steering: mpsc::Receiver<SteerMessage>,
    pub interrupt: CancellationToken,
}

#[derive(Clone, Debug)]
pub struct ModelCatalog {
    pub models: Vec<Model>,
    pub source: &'static str,
}

#[derive(Clone, Debug)]
pub struct ModelContext {
    pub hash: String,
    pub binary_path: std::path::PathBuf,
    pub binary_version: Option<String>,
}

#[async_trait]
pub trait Harness: Send + Sync {
    fn id(&self) -> HarnessId;
    fn display_name(&self) -> &str;
    fn supports_steering(&self) -> bool;
    fn steering_mode(&self) -> SteeringMode;
    fn reasoning_levels(&self) -> &[ReasoningLevel];
    fn installed(&self) -> bool {
        true
    }
    fn executable_path(&self) -> Option<std::path::PathBuf> {
        None
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError>;
    fn model_context(&self) -> Result<Option<ModelContext>, HarnessError> {
        Ok(None)
    }
    fn fallback_models(&self) -> Vec<Model> {
        Vec::new()
    }
    async fn model_catalog(&self, _force: bool) -> Result<ModelCatalog, HarnessError> {
        self.models().await.map(|models| ModelCatalog {
            models,
            source: "live",
        })
    }

    async fn commands(&self, _cwd: &std::path::Path) -> Result<Vec<SlashCommand>, HarnessError> {
        Ok(Vec::new())
    }

    async fn skills(&self, _cwd: &std::path::Path) -> Result<Vec<Skill>, HarnessError> {
        Ok(Vec::new())
    }

    /// Chats saved for `cwd` by the agent's own app, newest first.
    async fn external_sessions(
        &self,
        _cwd: &std::path::Path,
    ) -> Result<Vec<ExternalSession>, HarnessError> {
        Ok(Vec::new())
    }

    /// A saved chat replayed as events, with the user's prompts as `UserMessage`.
    async fn external_history(
        &self,
        _cwd: &std::path::Path,
        _session_id: &str,
    ) -> Result<Vec<AgentEvent>, HarnessError> {
        Ok(Vec::new())
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError>;
}

pub mod accounts;
mod catalog;
mod catalog_failure;
pub use catalog_failure::{CatalogFailure, CatalogFailureCode};
pub mod claude;
pub mod codex;
pub(crate) mod executable;
pub(crate) mod jsonrpc;
mod model_context;
pub mod opencode;
pub mod process;
mod proto;
pub mod usage;
pub mod view;

pub fn leading_command(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim_start_matches([' ', '\t', '\r', '\n']);
    let indent = text[..text.len() - trimmed.len()]
        .rsplit(['\n', '\r'])
        .next()
        .unwrap_or_default();
    if indent.contains('\t') || indent.len() >= 4 {
        return None;
    }
    let rest = trimmed.strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().filter(|name| !name.is_empty())?;
    Some((name, parts.next().unwrap_or_default().trim()))
}

/// npm-shim CLIs are `#!/usr/bin/env node` scripts whose `node` sits beside them.
pub fn compose_child_path(cmd: &mut process::Command, exe: &std::path::Path) {
    compose_path(
        cmd.as_std_mut(),
        exe.parent().filter(|d| !d.as_os_str().is_empty()),
    );
}

fn compose_path<'a>(
    cmd: &mut std::process::Command,
    executable_dir: impl IntoIterator<Item = &'a std::path::Path>,
) {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for dir in executable_dir {
        paths.push(dir.to_path_buf());
    }
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| !p.as_os_str().is_empty() && seen.insert(p.clone()));
    if let Ok(joined) = std::env::join_paths(paths) {
        cmd.env("PATH", joined);
    }
}

#[derive(Clone, Default)]
pub(crate) struct StderrTail(std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>);

impl StderrTail {
    const KEEP_LINES: usize = 6;
    const KEEP_BYTES: usize = 700;

    pub(crate) fn push(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let mut tail = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tail.push_back(line.chars().take(Self::KEEP_BYTES).collect());
        while tail.len() > Self::KEEP_LINES {
            tail.pop_front();
        }
    }

    pub(crate) fn snapshot(&self) -> Option<String> {
        let tail = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tail.is_empty() {
            return None;
        }
        let mut joined = tail.iter().cloned().collect::<Vec<_>>().join("\n");
        let mut start = joined.len().saturating_sub(Self::KEEP_BYTES * 2);
        while !joined.is_char_boundary(start) {
            start += 1;
        }
        joined.drain(..start);
        Some(joined)
    }
}

pub(crate) fn describe_exit(status: Option<std::process::ExitStatus>) -> String {
    let Some(status) = status else {
        return "still running".into();
    };
    if let Some(code) = status.code() {
        return format!("exit code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {signal}");
        }
    }
    "unknown exit".into()
}

fn redact_secrets(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let markers = ["bearer ", "basic ", "sk-", "ghp_", "xox", "api_key="];
    let mut result = String::new();
    let mut offset = 0;
    while let Some((start, marker)) = markers
        .iter()
        .filter_map(|marker| {
            lower[offset..]
                .find(marker)
                .map(|at| (offset + at, *marker))
        })
        .min_by_key(|(at, _)| *at)
    {
        let credential = if marker.ends_with(' ') || marker.ends_with('=') {
            start + marker.len()
        } else {
            start
        };
        let credential = credential + text[credential..].len()
            - text[credential..]
                .trim_start_matches(|c: char| c.is_whitespace() || c == '\"' || c == '\'')
                .len();
        let end = text[credential..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '\"' | '\'' | ',' | ';' | '&' | '<' | '>')
            })
            .map_or(text.len(), |at| credential + at);
        result.push_str(&text[offset..credential]);
        result.push_str("[REDACTED]");
        offset = end.max(start + marker.len());
    }
    result.push_str(&text[offset..]);
    result
}

#[cfg(test)]
#[test]
fn crash_diagnostics_redact_credentials_but_keep_context() {
    let raw = "request failed: Bearer secret-one Basic secret-two sk-private ghp-private ghp_private xoxp-private api_key=private&code=401 café";
    let clean = redact_secrets(raw);
    assert_eq!(
        clean,
        "request failed: Bearer [REDACTED] Basic [REDACTED] [REDACTED] ghp-private [REDACTED] [REDACTED] api_key=[REDACTED]&code=401 café"
    );
    assert_eq!(
        redact_secrets("Authorization: bEaReR token"),
        "Authorization: bEaReR [REDACTED]"
    );
    assert_eq!(
        redact_secrets("Bearer   hidden api_key=\"secret\""),
        "Bearer   [REDACTED] api_key=\"[REDACTED]\""
    );
    let tail = StderrTail::default();
    tail.push(raw);
    let message = crash_message("agent", None, &tail);
    assert!(message.ends_with(&clean));
    assert!(!message.contains("secret-one"));
}

pub(crate) fn crash_message(
    name: &str,
    status: Option<std::process::ExitStatus>,
    stderr: &StderrTail,
) -> String {
    let status = describe_exit(status);
    match stderr.snapshot() {
        Some(tail) => format!(
            "{name} exited unexpectedly ({status}): {}",
            redact_secrets(&tail)
        ),
        None => format!("{name} exited unexpectedly ({status})"),
    }
}

pub use claude::ClaudeHarness;
pub use codex::CodexHarness;
pub use opencode::OpencodeHarness;

#[cfg_attr(not(unix), allow(unused_variables))]
pub(crate) async fn shutdown_child(child: &mut process::Child, kill_grace: std::time::Duration) {
    #[cfg(not(unix))]
    {
        child.start_kill().ok();
        child.wait().await.ok();
    }
    #[cfg(unix)]
    {
        let target = process::signal_target(child);
        if matches!(child.try_wait(), Ok(Some(_))) {
            if let Some(group) = target.filter(|pid| *pid < 0) {
                send_signal(&group, Signal::Kill);
            }
            return;
        }
        if let Some(pid) = target {
            send_signal(&pid, Signal::Term);
            if tokio::time::timeout(kill_grace, child.wait()).await.is_ok() {
                if pid < 0 {
                    send_signal(&pid, Signal::Kill);
                }
                return;
            }
        }
        if let Some(pid) = target {
            send_signal(&pid, Signal::Kill);
        }
        child.start_kill().ok();
        if let Err(error) = child.wait().await {
            log::warn!("failed to reap agent process: {error}");
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
pub(crate) enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
pub(crate) fn send_signal(pid: &i32, signal: Signal) {
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: kill(2) targets an owned child or its private process group.
    unsafe {
        libc::kill(*pid, sig);
    }
}

#[cfg(test)]
mod stderr_tests {
    #[test]
    fn stderr_tail_truncates_at_utf8_boundaries() {
        let tail = super::StderrTail::default();
        tail.push(&"界".repeat(700));
        tail.push(&"界".repeat(700));
        tail.push("last stderr line");
        let snapshot = tail.snapshot().unwrap();
        assert!(snapshot.len() <= 1400);
        assert!(snapshot.ends_with("last stderr line"));
    }
}
