pub use settings::{AgentChatModelPickerLayout, AgentChatSendKey};
use settings::{RegisterSetting, Settings};

#[derive(Clone, Debug, RegisterSetting)]
pub struct AgentChatSettings {
    pub send_with: AgentChatSendKey,
    pub compact_transcript: bool,
    pub model_picker: AgentChatModelPickerLayout,
    pub dictation: bool,
    pub sound_when_done: bool,
    pub sound_when_needs_input: bool,
    pub sound_on_error: bool,
    pub notifications: bool,
    pub notify_only_in_background: bool,
}

impl Settings for AgentChatSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.agent_chat.clone().unwrap_or_default();
        Self {
            send_with: content.send_with.unwrap_or_default(),
            compact_transcript: content.compact_transcript.unwrap_or(false),
            model_picker: content.model_picker.unwrap_or_default(),
            dictation: content.dictation.unwrap_or(true),
            sound_when_done: content.sound_when_done.unwrap_or(true),
            sound_when_needs_input: content.sound_when_needs_input.unwrap_or(true),
            sound_on_error: content.sound_on_error.unwrap_or(true),
            notifications: content.notifications.unwrap_or(true),
            notify_only_in_background: content.notify_only_in_background.unwrap_or(true),
        }
    }
}
