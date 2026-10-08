use agent_harness::{PermissionMode, ReasoningLevel};
use settings::{
    AgentChatDefaultEffort, AgentChatWhileWorking, ClaudeCodePermissionDefault,
    CodexPermissionDefault, ContextTokens, OpencodePermissionDefault, RegisterSetting, Settings,
};
pub use settings::{AgentChatModelPickerLayout, AgentChatSendKey};

use crate::AgentKind;

#[derive(Clone, Debug, RegisterSetting)]
pub struct AgentChatSettings {
    pub enabled: bool,
    pub send_with: AgentChatSendKey,
    pub compact_transcript: bool,
    pub model_picker: AgentChatModelPickerLayout,
    pub dictation: bool,
    pub sound_when_done: bool,
    pub sound_when_needs_input: bool,
    pub sound_on_error: bool,
    pub notifications: bool,
    pub notify_only_in_background: bool,
    pub auto_compact_tokens: Option<u64>,
    pub default_effort: Option<ReasoningLevel>,
    default_permissions: [(AgentKind, PermissionMode); 3],
    agents: Vec<AgentKind>,
    pub steer_while_working: bool,
    pub confirm_quit_while_working: bool,
}

impl AgentChatSettings {
    pub fn default_permission(&self, kind: AgentKind) -> PermissionMode {
        self.default_permissions
            .iter()
            .find(|(agent, _)| *agent == kind)
            .map_or(PermissionMode::Auto, |(_, mode)| *mode)
    }

    pub fn agents(&self) -> &[AgentKind] {
        &self.agents
    }
}

impl Settings for AgentChatSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.agent_chat.clone().unwrap_or_default();
        let permission = content.default_permission.clone().unwrap_or_default();
        let shown = content.agents.clone().unwrap_or_default();
        let mut agents: Vec<AgentKind> = [
            (AgentKind::Claude, shown.claude_code),
            (AgentKind::Codex, shown.codex),
            (AgentKind::Opencode, shown.opencode),
        ]
        .into_iter()
        .filter(|(_, shown)| shown.unwrap_or(true))
        .map(|(kind, _)| kind)
        .collect();
        if agents.is_empty() {
            agents = AgentKind::ALL.to_vec();
        }
        Self {
            enabled: content.enabled.unwrap_or(true),
            send_with: content.send_with.unwrap_or_default(),
            compact_transcript: content.compact_transcript.unwrap_or(false),
            model_picker: content.model_picker.unwrap_or_default(),
            dictation: content.dictation.unwrap_or(true),
            sound_when_done: content.sound_when_done.unwrap_or(true),
            sound_when_needs_input: content.sound_when_needs_input.unwrap_or(true),
            sound_on_error: content.sound_on_error.unwrap_or(true),
            notifications: content.notifications.unwrap_or(false),
            notify_only_in_background: content.notify_only_in_background.unwrap_or(true),
            auto_compact_tokens: content.auto_compact.unwrap_or(false).then(|| {
                content
                    .auto_compact_limit
                    .unwrap_or(ContextTokens(200_000))
                    .clamped()
            }),
            default_effort: effort(content.default_effort.unwrap_or_default()),
            default_permissions: [
                (
                    AgentKind::Claude,
                    claude_permission(permission.claude_code.unwrap_or_default()),
                ),
                (
                    AgentKind::Codex,
                    codex_permission(permission.codex.unwrap_or_default()),
                ),
                (
                    AgentKind::Opencode,
                    opencode_permission(permission.opencode.unwrap_or_default()),
                ),
            ],
            agents,
            steer_while_working: content.while_working.unwrap_or_default()
                == AgentChatWhileWorking::Steer,
            confirm_quit_while_working: content.confirm_quit_while_working.unwrap_or(true),
        }
    }
}

fn effort(effort: AgentChatDefaultEffort) -> Option<ReasoningLevel> {
    match effort {
        AgentChatDefaultEffort::LastUsed => None,
        AgentChatDefaultEffort::Minimal => Some(ReasoningLevel::Minimal),
        AgentChatDefaultEffort::Low => Some(ReasoningLevel::Low),
        AgentChatDefaultEffort::Medium => Some(ReasoningLevel::Medium),
        AgentChatDefaultEffort::High => Some(ReasoningLevel::High),
        AgentChatDefaultEffort::XHigh => Some(ReasoningLevel::XHigh),
        AgentChatDefaultEffort::Max => Some(ReasoningLevel::Max),
    }
}

fn claude_permission(mode: ClaudeCodePermissionDefault) -> PermissionMode {
    match mode {
        ClaudeCodePermissionDefault::Ask => PermissionMode::Ask,
        ClaudeCodePermissionDefault::AcceptEdits => PermissionMode::AcceptEdits,
        ClaudeCodePermissionDefault::Auto => PermissionMode::Auto,
        ClaudeCodePermissionDefault::DontAsk => PermissionMode::DontAsk,
        ClaudeCodePermissionDefault::Bypass => PermissionMode::FullAccess,
    }
}

fn codex_permission(mode: CodexPermissionDefault) -> PermissionMode {
    match mode {
        CodexPermissionDefault::ReadOnly => PermissionMode::ReadOnly,
        CodexPermissionDefault::Auto => PermissionMode::Auto,
        CodexPermissionDefault::ApproveForMe => PermissionMode::ApproveForMe,
        CodexPermissionDefault::FullAccess => PermissionMode::FullAccess,
    }
}

fn opencode_permission(mode: OpencodePermissionDefault) -> PermissionMode {
    match mode {
        OpencodePermissionDefault::Ask => PermissionMode::Ask,
        OpencodePermissionDefault::AcceptEdits => PermissionMode::AcceptEdits,
        OpencodePermissionDefault::Auto => PermissionMode::Auto,
        OpencodePermissionDefault::FullAccess => PermissionMode::FullAccess,
    }
}
