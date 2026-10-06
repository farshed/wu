//! Never refresh or write credentials: the running CLI owns its single-use refresh token.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::HarnessId;

pub const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

#[derive(Clone, Debug, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    pub used_fraction: f32,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanUsage {
    pub windows: Vec<UsageWindow>,
    pub plan_label: Option<String>,
}

impl PlanUsage {
    /// The most-used window.
    pub fn used_fraction(&self) -> Option<f32> {
        self.windows
            .iter()
            .map(|window| window.used_fraction.clamp(0.0, 1.0))
            .reduce(f32::max)
    }
}

pub struct UsageRequest {
    pub url: &'static str,
    pub headers: Vec<(&'static str, String)>,
    plan_label: Option<String>,
}

impl UsageRequest {
    pub fn parse(&self, harness: HarnessId, body: &Value) -> Option<PlanUsage> {
        let mut usage = match harness {
            HarnessId::ClaudeCode => claude_usage_windows(body),
            HarnessId::Codex => codex_usage_snapshot(body),
            HarnessId::Opencode => None,
        }?;
        if usage.plan_label.is_none() {
            usage.plan_label = self.plan_label.clone();
        }
        Some(usage)
    }
}

pub async fn usage_request(harness: HarnessId) -> Result<UsageRequest, String> {
    match harness {
        HarnessId::ClaudeCode => {
            let credentials = read_claude_credentials()
                .await?
                .ok_or_else(|| "Sign in to Claude Code to see usage".to_string())?;
            claude_usage_request(&credentials)
        }
        HarnessId::Codex => {
            let auth = read_json(&codex_home().join("auth.json"))
                .ok_or_else(|| "Sign in to Codex to see usage".to_string())?;
            codex_usage_request(&auth)
        }
        HarnessId::Opencode => Err(OPENCODE_NO_USAGE.into()),
    }
}

pub const OPENCODE_NO_USAGE: &str = "OpenCode doesn't report plan usage";

pub(crate) fn claude_usage_request(credentials: &Value) -> Result<UsageRequest, String> {
    let oauth = credentials
        .get("claudeAiOauth")
        .ok_or_else(|| "Usage is only reported for Claude subscriptions".to_string())?;
    let access_token = str_field(oauth, "accessToken")
        .ok_or_else(|| "Sign in to Claude Code to see usage".to_string())?;
    let plan_label = claude_plan(
        str_field(oauth, "subscriptionType")
            .map(|kind| format!("claude_{kind}"))
            .as_deref(),
        str_field(oauth, "rateLimitTier").as_deref(),
    );
    Ok(UsageRequest {
        url: CLAUDE_USAGE_URL,
        headers: vec![
            ("Authorization", format!("Bearer {access_token}")),
            ("anthropic-beta", "oauth-2025-04-20".into()),
            ("Content-Type", "application/json".into()),
        ],
        plan_label,
    })
}

pub(crate) fn codex_usage_request(auth: &Value) -> Result<UsageRequest, String> {
    let Some(tokens) = auth.get("tokens") else {
        return Err(if str_field(auth, "OPENAI_API_KEY").is_some() {
            "Usage isn't reported for API keys".into()
        } else {
            "Sign in to Codex to see usage".into()
        });
    };
    let access_token = str_field(tokens, "access_token")
        .ok_or_else(|| "Sign in to Codex to see usage".to_string())?;
    Ok(UsageRequest {
        url: CODEX_USAGE_URL,
        headers: vec![
            ("Authorization", format!("Bearer {access_token}")),
            (
                "chatgpt-account-id",
                str_field(tokens, "account_id").unwrap_or_default(),
            ),
        ],
        plan_label: None,
    })
}

pub fn status_message(status: u16) -> String {
    match status {
        401 | 403 => "Sign in again to see usage".into(),
        429 => "Usage is rate limited, try again soon".into(),
        _ => format!("Usage unavailable (HTTP {status})"),
    }
}

fn env_dir(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn codex_home() -> PathBuf {
    env_dir("CODEX_HOME").unwrap_or_else(|| crate::executable::home_or_current_dir().join(".codex"))
}

fn read_json(file: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Must match the service name Claude Code itself writes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn claude_keychain_service(config_dir: Option<&Path>) -> String {
    match config_dir {
        None => "Claude Code-credentials".to_string(),
        Some(dir) => {
            let digest = Sha256::digest(dir.to_string_lossy().as_bytes());
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("Claude Code-credentials-{}", &hex[..8])
        }
    }
}

/// Keychain first, matching Claude Code's own precedence.
async fn read_claude_credentials() -> Result<Option<Value>, String> {
    let config_dir = env_dir("CLAUDE_CONFIG_DIR");
    let file = config_dir
        .clone()
        .unwrap_or_else(|| crate::executable::home_or_current_dir().join(".claude"))
        .join(".credentials.json");
    #[cfg(target_os = "macos")]
    {
        let service = claude_keychain_service(config_dir.as_deref());
        match keychain::read_credentials(&service).await {
            Ok(Some(credentials)) => return Ok(Some(credentials)),
            Ok(None) => {}
            Err(warning) => return read_json(&file).map(Some).ok_or(warning),
        }
    }
    Ok(read_json(&file))
}

#[cfg(target_os = "macos")]
pub(crate) mod keychain {
    use super::*;
    use std::time::Duration;

    // An unanswered Keychain consent dialog blocks `security` indefinitely.
    const EXEC_TIMEOUT: Duration = Duration::from_secs(15);

    async fn exec(args: &[&str]) -> (bool, String) {
        // Absolute path: a PATH-planted `security` must never see secrets.
        let run = tokio::process::Command::new("/usr/bin/security")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output();
        match tokio::time::timeout(EXEC_TIMEOUT, run).await {
            Ok(Ok(out)) => (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).to_string(),
            ),
            _ => (false, String::new()),
        }
    }

    pub(crate) fn account() -> String {
        std::env::var("USER").unwrap_or_else(|_| "unknown".into())
    }

    pub(crate) async fn item_exists(service: &str) -> bool {
        exec(&["find-generic-password", "-s", service]).await.0
    }

    pub(crate) async fn read_credentials(service: &str) -> Result<Option<Value>, String> {
        // This probe needs no authorization, so a later failure means access was denied.
        if !item_exists(service).await {
            return Ok(None);
        }
        let (ok, stdout) = exec(&["find-generic-password", "-s", service, "-w"]).await;
        if !ok {
            return Err("macOS Keychain denied access to the Claude Code login".into());
        }
        serde_json::from_str(stdout.trim())
            .map(Some)
            .map_err(|_| "The Claude Code Keychain entry could not be read".into())
    }
}

fn parse_when(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value? {
        Value::Number(n) => DateTime::<Utc>::from_timestamp(n.as_i64()?, 0),
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        _ => None,
    }
}

pub(crate) fn claude_plan(org_type: Option<&str>, tier: Option<&str>) -> Option<String> {
    let base = match org_type {
        Some("claude_max") => "Max",
        Some("claude_pro") => "Pro",
        Some("claude_team") => "Team",
        Some("claude_enterprise") => "Enterprise",
        _ => return None,
    };
    let mult = tier.and_then(|t| {
        let stem = t.strip_suffix('x')?;
        let digits: String = stem
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let preceded = stem.len() > digits.len()
            && stem.as_bytes().get(stem.len() - digits.len() - 1) == Some(&b'_');
        (!digits.is_empty() && preceded).then_some(digits)
    });
    Some(match mult {
        Some(mult) => format!("{base} {mult}×"),
        None => base.to_string(),
    })
}

pub(crate) fn codex_plan(plan: Option<&str>) -> Option<String> {
    let plan = plan?;
    let mut chars = plan.chars();
    let first = chars.next()?;
    Some(format!(
        "ChatGPT {}{}",
        first.to_uppercase(),
        chars.as_str()
    ))
}

fn codex_window_label(span_seconds: i64) -> &'static str {
    const DAY: i64 = 86_400;
    if span_seconds >= 28 * DAY {
        "Month"
    } else if span_seconds >= 5 * DAY {
        "Week"
    } else {
        "Session"
    }
}

fn codex_usage_snapshot(body: &Value) -> Option<PlanUsage> {
    let rl = body.get("rate_limit")?;
    let mut windows = Vec::new();
    for key in ["primary_window", "secondary_window"] {
        if let Some(w) = rl.get(key)
            && let Some(used) = w.get("used_percent").and_then(|v| v.as_f64())
        {
            let span = w
                .get("limit_window_seconds")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            windows.push(UsageWindow {
                label: codex_window_label(span).to_string(),
                used_fraction: (used / 100.0) as f32,
                resets_at: parse_when(w.get("reset_at")),
            });
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some(PlanUsage {
        windows,
        plan_label: codex_plan(str_field(body, "plan_type").as_deref()),
    })
}

fn claude_usage_windows(body: &Value) -> Option<PlanUsage> {
    let mut windows = Vec::new();
    for (key, label) in [("five_hour", "Session"), ("seven_day", "Week")] {
        if let Some(w) = body.get(key)
            && let Some(utilization) = w.get("utilization").and_then(|v| v.as_f64())
        {
            windows.push(UsageWindow {
                label: label.to_string(),
                used_fraction: (utilization / 100.0) as f32,
                resets_at: parse_when(w.get("resets_at")),
            });
        }
    }
    (!windows.is_empty()).then_some(PlanUsage {
        windows,
        plan_label: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_windows_parse_session_and_week() {
        let usage = claude_usage_windows(&json!({
            "five_hour": {"utilization": 42.0, "resets_at": "2026-10-04T18:00:00Z"},
            "seven_day": {"utilization": 7.5, "resets_at": null}
        }))
        .unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "Session");
        assert!((usage.windows[0].used_fraction - 0.42).abs() < 1e-6);
        assert!(usage.windows[0].resets_at.is_some());
        assert_eq!(usage.windows[1].label, "Week");
        assert!((usage.used_fraction().unwrap() - 0.42).abs() < 1e-6);
    }

    #[test]
    fn codex_windows_label_by_span_and_carry_the_plan() {
        let usage = codex_usage_snapshot(&json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {"used_percent": 12, "limit_window_seconds": 18000, "reset_at": 1790000000},
                "secondary_window": {"used_percent": 80, "limit_window_seconds": 604800}
            }
        }))
        .unwrap();
        assert_eq!(usage.plan_label.as_deref(), Some("ChatGPT Plus"));
        assert_eq!(usage.windows[0].label, "Session");
        assert_eq!(usage.windows[1].label, "Week");
        assert!((usage.used_fraction().unwrap() - 0.8).abs() < 1e-6);
        assert_eq!(codex_window_label(2_592_000), "Month");
    }

    #[test]
    fn claude_plans_include_tier_multipliers() {
        assert_eq!(
            claude_plan(Some("claude_max"), Some("default_claude_max_20x")).as_deref(),
            Some("Max 20×")
        );
        assert_eq!(
            claude_plan(Some("claude_pro"), None).as_deref(),
            Some("Pro")
        );
        assert_eq!(claude_plan(Some("other"), None), None);
    }

    #[test]
    fn relocated_config_dirs_get_their_own_keychain_service() {
        assert_eq!(claude_keychain_service(None), "Claude Code-credentials");
        let relocated = claude_keychain_service(Some(Path::new("/tmp/claude")));
        assert!(relocated.starts_with("Claude Code-credentials-"));
        assert_eq!(relocated.len(), "Claude Code-credentials-".len() + 8);
    }
}
