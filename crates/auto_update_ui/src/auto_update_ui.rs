<<<<<<< c9825ea7226ebcbbbb1c4d442d6965319762b71d
use std::sync::Arc;

use auto_update::{AutoUpdater, release_notes_url};
=======
use auto_update::{AutoUpdater, release_notes_url};
use db::kvp::Dismissable;
use editor::{Editor, MultiBuffer};
>>>>>>> d0e8038659bb444568cfa09d69517ce1d534fb6a
use gpui::{
    App, DismissEvent, EventEmitter, FocusHandle, Focusable, TaskExt, Window, actions,
    prelude::*,
};
<<<<<<< c9825ea7226ebcbbbb1c4d442d6965319762b71d
use release_channel::ReleaseChannel;
use semver::Version;
use ui::{AnnouncementToast, ListBulletItem, prelude::*};
=======
use markdown_preview::markdown_preview_view::{MarkdownPreviewMode, MarkdownPreviewView};
use project::DisableAiSettings;
use release_channel::{AppVersion, ReleaseChannel};
use semver::Version;
use serde::Deserialize;
use settings::Settings as _;
use smol::io::AsyncReadExt;
use ui::{AnnouncementToast, DeltaIllustration, ListBulletItem, prelude::*};
use util::{ResultExt as _, maybe};
>>>>>>> d0e8038659bb444568cfa09d69517ce1d534fb6a
use workspace::{
    Workspace,
    notifications::{
        Notification, NotificationId, SuppressEvent, show_app_notification,
        simple_message_notification::MessageNotification,
    },
};
use wu_actions::ShowUpdateNotification;

actions!(
    auto_update,
    [
        /// Opens the release notes for the current version in a new tab.
        ViewReleaseNotesLocally
    ]
);

pub fn init(cx: &mut App) {
    notify_if_app_was_updated(cx);
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        workspace.register_action(|workspace, _: &ViewReleaseNotesLocally, window, cx| {
            view_release_notes_locally(workspace, window, cx);
        });

        if matches!(ReleaseChannel::global(cx), ReleaseChannel::Dev) {
            workspace.register_action(|_workspace, _: &ShowUpdateNotification, _window, cx| {
                show_update_notification(cx);
            });
        }
    })
    .detach();
}

fn view_release_notes_locally(
    _workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(url) = release_notes_url(cx) {
        cx.open_url(&url);
    }
}

#[derive(Clone)]
struct AnnouncementContent {
    heading: SharedString,
    description: SharedString,
    bullet_items: Vec<SharedString>,
    primary_action_label: SharedString,
    secondary_action_label: SharedString,
    primary_action_url: SharedString,
    secondary_action_url: SharedString,
}

<<<<<<< c9825ea7226ebcbbbb1c4d442d6965319762b71d
fn announcement_for_version(_version: &Version, _cx: &App) -> Option<AnnouncementContent> {
    None
=======
struct DeltaAnnouncement;

impl Dismissable for DeltaAnnouncement {
    const KEY: &'static str = "delta_announcement_dismissed";
}

fn announcement_for_version(version: &Version, cx: &App) -> Option<AnnouncementContent> {
    let version_with_delta = Version::new(1, 22, 0);
    if *version < version_with_delta
        || DisableAiSettings::get_global(cx).disable_ai
        || DeltaAnnouncement::dismissed(cx)
    {
        return None;
    }

    Some(AnnouncementContent {
        heading: "Introducing Delta".into(),
        description:
            "Built on DeltaDB, so your threads and code stay in sync across machines and teammates."
                .into(),
        bullet_items: vec![
            "Made by the Zed team, with the same quality and performance".into(),
            "Work with teammates and agents in the same thread, live or later".into(),
            "Pick up your thread on the web or your phone, without committing or pushing".into(),
        ],
        primary_action_label: "Try Delta".into(),
        secondary_action_label: "Learn More".into(),
        primary_action_url: "https://delta.dev/".into(),
        secondary_action_url: "https://delta.dev/docs/getting-started".into(),
    })
>>>>>>> d0e8038659bb444568cfa09d69517ce1d534fb6a
}

struct AnnouncementToastNotification {
    focus_handle: FocusHandle,
    content: AnnouncementContent,
}

impl AnnouncementToastNotification {
    fn new(content: AnnouncementContent, cx: &mut App) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content,
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        DeltaAnnouncement::set_dismissed(true, cx);
    }
}

impl Focusable for AnnouncementToastNotification {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for AnnouncementToastNotification {}
impl EventEmitter<SuppressEvent> for AnnouncementToastNotification {}
impl Notification for AnnouncementToastNotification {}

impl Render for AnnouncementToastNotification {
<<<<<<< c9825ea7226ebcbbbb1c4d442d6965319762b71d
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        AnnouncementToast::new()
=======
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toast = AnnouncementToast::new()
            .illustration(DeltaIllustration::new())
>>>>>>> d0e8038659bb444568cfa09d69517ce1d534fb6a
            .heading(self.content.heading.clone())
            .description(self.content.description.clone())
            .bullet_items(
                self.content
                    .bullet_items
                    .iter()
                    .map(|item| ListBulletItem::new(item.clone())),
            )
            .primary_action_label(self.content.primary_action_label.clone())
            .secondary_action_label(self.content.secondary_action_label.clone())
            .primary_on_click(cx.listener({
                let url = self.content.primary_action_url.clone();
<<<<<<< c9825ea7226ebcbbbb1c4d442d6965319762b71d
                let callback = self.content.primary_action_callback.clone();
                move |this, _, window, cx| {
                    if let Some(callback) = &callback {
                        callback(window, cx);
                    }
                    if let Some(url) = &url {
                        cx.open_url(url);
                    }
=======
                move |this, _, _window, cx| {
                    telemetry::event!("Delta Announcement Main Click");
                    cx.open_url(&url);
>>>>>>> d0e8038659bb444568cfa09d69517ce1d534fb6a
                    this.dismiss(cx);
                }
            }))
            .secondary_on_click(cx.listener({
                let url = self.content.secondary_action_url.clone();
                move |_, _, _window, cx| {
<<<<<<< c9825ea7226ebcbbbb1c4d442d6965319762b71d
                    if let Some(url) = &url {
                        cx.open_url(url);
                    }
                }
            }))
            .dismiss_on_click(cx.listener(|this, _, _window, cx| {
=======
                    telemetry::event!("Delta Announcement Secondary Click");
                    cx.open_url(&url);
                }
            }))
            .dismiss_on_click(cx.listener(|this, _, _window, cx| {
                telemetry::event!("Delta Announcement Dismiss");
>>>>>>> d0e8038659bb444568cfa09d69517ce1d534fb6a
                this.dismiss(cx);
            }));

        div()
            .self_end()
            .flex_none()
            .w(rems_from_px(400_f32))
            .max_w((window.viewport_size().width - window.rem_size() * 1.5).max(px(0.)))
            .child(toast)
    }
}

struct UpdateNotification;

fn show_update_notification(cx: &mut App) {
    let Some(updater) = AutoUpdater::get(cx) else {
        return;
    };

    let mut version = updater.read(cx).current_version();
    version.pre = semver::Prerelease::EMPTY;
    version.build = semver::BuildMetadata::EMPTY;
    let app_name = ReleaseChannel::global(cx).display_name();

    if let Some(content) = announcement_for_version(&version, cx) {
        show_app_notification(
            NotificationId::unique::<UpdateNotification>(),
            cx,
            move |cx| cx.new(|cx| AnnouncementToastNotification::new(content.clone(), cx)),
        );
    } else {
        show_app_notification(
            NotificationId::unique::<UpdateNotification>(),
            cx,
            move |cx| {
                let workspace_handle = cx.entity().downgrade();
                cx.new(|cx| {
                    MessageNotification::new(format!("Updated to {app_name} {}", version), cx)
                        .primary_message("View Release Notes")
                        .primary_on_click(move |window, cx| {
                            if let Some(workspace) = workspace_handle.upgrade() {
                                workspace.update(cx, |workspace, cx| {
                                    crate::view_release_notes_locally(workspace, window, cx);
                                })
                            }
                            cx.emit(DismissEvent);
                        })
                        .show_suppress_button(false)
                })
            },
        );
    }
}

/// Shows a notification across all workspaces if an update was previously automatically installed
/// and this notification had not yet been shown.
pub fn notify_if_app_was_updated(cx: &mut App) {
    let Some(updater) = AutoUpdater::get(cx) else {
        return;
    };

    let should_show_notification = updater.read(cx).should_show_update_notification(cx);

    cx.spawn(async move |cx| {
        let should_show_notification = should_show_notification.await?;

        if should_show_notification {
            cx.update(|cx| {
                show_update_notification(cx);
                updater.update(cx, |updater, cx| {
                    updater
                        .set_should_show_update_notification(false, cx)
                        .detach_and_log_err(cx);
                });
            });
        }
        anyhow::Ok(())
    })
    .detach();
}
