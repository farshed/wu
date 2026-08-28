//! Contains helper functions for constructing URLs to various Zed-related pages.
//!
//! These URLs will adapt to the configured server URL in order to construct
//! links appropriate for the environment (e.g., by linking to a local copy of
//! zed.dev in development).

use gpui::App;
use release_channel::ReleaseChannel;
use settings::Settings;

use crate::ClientSettings;

fn server_url(cx: &App) -> &str {
    &ClientSettings::get_global(cx).server_url
}

fn docs_url(cx: &App) -> String {
    let server_url = server_url(cx);
    match ReleaseChannel::try_global(cx).unwrap_or_default() {
        ReleaseChannel::Stable => {
            format!("{server_url}/docs")
        }
        ReleaseChannel::Preview => {
            format!("{server_url}/docs/preview")
        }
        ReleaseChannel::Dev | ReleaseChannel::Nightly => {
            format!("{server_url}/docs/nightly")
        }
    }
}

/// Returns the URL to Zed's terms of service.
pub fn terms_of_service(cx: &App) -> String {
    format!("{server_url}/terms-of-service", server_url = server_url(cx))
}

/// Returns the URL to Zed AI's privacy and security docs.
pub fn ai_privacy_and_security(cx: &App) -> String {
    format!(
        "{docs_url}/ai/privacy-and-security",
        docs_url = docs_url(cx)
    )
}
