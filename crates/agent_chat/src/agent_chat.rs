mod agent_panel;
mod chat_style;
mod chat_view;
mod model_picker;
mod session;
mod usage_rings;

pub use agent_panel::AgentPanel;
pub use chat_view::ChatView;

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
        /// Sends the message in the chat box.
        Send,
        /// Stops the agent's current turn.
        Stop,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
        }
    }
}

pub fn init(cx: &mut App) {
    agent_panel::init(cx);
}
