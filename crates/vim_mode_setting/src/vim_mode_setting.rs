//! Contains the [`VimModeSetting`] used to enable/disable Vim mode.
//!
//! This is in its own crate as we want other crates to be able to enable or
//! disable Vim mode without having to depend on the `vim` crate in its
//! entirety.

use gpui::App;
use settings::{RegisterSetting, Settings, SettingsContent};

#[derive(RegisterSetting)]
pub struct VimModeSetting(pub bool);

impl Settings for VimModeSetting {
    fn from_settings(content: &SettingsContent) -> Self {
        Self(content.vim_mode.unwrap())
    }
}

impl VimModeSetting {
    pub fn is_enabled(cx: &App) -> bool {
        Self::try_get(cx)
            .map(|vim_mode| vim_mode.0)
            .unwrap_or(false)
    }
}

