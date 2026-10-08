mod agent_chat_settings;
mod agent_panel;
mod chat_style;
mod chat_view;
mod model_picker;
mod notifications;
mod saved_chats;
mod session;
mod slash_commands;
mod usage_rings;

pub use agent_chat_settings::AgentChatSettings;
pub use agent_panel::AgentPanel;
pub use chat_view::ChatView;

use futures::FutureExt as _;
use gpui::{App, actions};
use serde::{Deserialize, Serialize};

actions!(
    agent_chat,
    [
        /// Toggles focus on the agent panel.
        ToggleFocus,
        /// Starts a new Claude Code chat.
        NewClaudeChat,
        /// Starts a new Codex chat.
        NewCodexChat,
        /// Starts a new OpenCode chat.
        NewOpencodeChat,
        /// Continues a chat started in Claude Code, Codex or OpenCode.
        ContinueSavedChat,
        /// Sends the message in the chat box.
        Send,
        /// Stops the agent's current turn.
        Stop,
        /// Highlights the previous command in the slash command menu.
        SelectPreviousCommand,
        /// Highlights the next command in the slash command menu.
        SelectNextCommand,
        /// Picks the highlighted command in the slash command menu.
        AcceptCommand,
        /// Closes the slash command menu.
        DismissCommands,
        /// Starts or stops dictation in the chat box.
        ToggleDictation,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    Opencode,
}

impl AgentKind {
    pub const ALL: [AgentKind; 3] = [AgentKind::Claude, AgentKind::Codex, AgentKind::Opencode];

    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Opencode => "OpenCode",
        }
    }

    pub(crate) fn has_plan_usage(self) -> bool {
        match self {
            AgentKind::Claude | AgentKind::Codex => true,
            AgentKind::Opencode => false,
        }
    }
}

/// `shell_env_loaded` resolves once the login shell's PATH is applied, so agent CLIs can be found.
pub fn init(shell_env_loaded: Option<futures::channel::oneshot::Receiver<()>>, cx: &mut App) {
    if let Some(loaded) = shell_env_loaded {
        cx.set_global(session::ShellEnvLoaded(loaded.shared()));
    }
    agent_panel::init(cx);
}
