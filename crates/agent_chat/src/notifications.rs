use crate::{
    AgentChatSettings,
    agent_panel::open_chat,
    session::{AgentStore, StoreEvent},
};
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, FontWeight, IntoElement, ParentElement, Render,
    SharedString, Styled, Task, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowHandle, WindowKind, WindowOptions, div, point, px, size,
};
use settings::Settings as _;
use std::{path::PathBuf, time::Duration};
use theme::ActiveTheme as _;
use ui::{Icon, IconName, IconSize, prelude::*};
use util::ResultExt as _;
use workspace::MultiWorkspace;

const AUTO_DISMISS: Duration = Duration::from_secs(12);
const NOTIFICATION_SIZE: (f32, f32) = (360., 78.);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sound {
    Done,
    Input,
    Error,
}

impl Sound {
    fn asset(self) -> &'static str {
        match self {
            Sound::Done => "sounds/agent_done.wav",
            Sound::Input => "sounds/agent_input.wav",
            Sound::Error => "sounds/agent_error.wav",
        }
    }
}

fn play(sound: Sound, cx: &mut App) {
    let Some(bytes) = cx.asset_source().load(sound.asset()).log_err().flatten() else {
        return;
    };
    let bytes = bytes.into_owned();
    cx.background_spawn(async move {
        let path = std::env::temp_dir().join(format!(
            "wu-agent-sound-{}-{:?}.wav",
            std::process::id(),
            sound
        ));
        if let Err(error) = std::fs::write(&path, &bytes) {
            log::debug!("cannot stage notification sound: {error}");
            return;
        }
        if let Err(error) = run_player(&path).await {
            log::debug!("cannot play notification sound: {error}");
        }
    })
    .detach();
}

#[cfg(target_os = "macos")]
async fn run_player(path: &std::path::Path) -> anyhow::Result<()> {
    util::command::new_command("afplay").arg(path).status().await?;
    Ok(())
}

#[cfg(target_os = "windows")]
async fn run_player(path: &std::path::Path) -> anyhow::Result<()> {
    util::command::new_command("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(New-Object Media.SoundPlayer $env:WU_SOUND_PATH).PlaySync()",
        ])
        .env("WU_SOUND_PATH", path)
        .status()
        .await?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
async fn run_player(path: &std::path::Path) -> anyhow::Result<()> {
    let players: &[(&str, &[&str])] = &[
        ("paplay", &[]),
        ("pw-play", &[]),
        ("aplay", &["-q"]),
        ("ffplay", &["-nodisp", "-autoexit", "-loglevel", "quiet"]),
    ];
    for (program, args) in players {
        let status = util::command::new_command(program)
            .args(*args)
            .arg(path)
            .status()
            .await;
        if status.is_ok_and(|status| status.success()) {
            return Ok(());
        }
    }
    anyhow::bail!("no audio player found")
}

pub(crate) fn watch(store: &Entity<AgentStore>, cx: &mut App) {
    cx.subscribe(store, |store, event, cx| {
        let (id, sound, heading) = match event {
            StoreEvent::SessionFinished { id, errored: false } => {
                (id.clone(), Sound::Done, "Run finished")
            }
            StoreEvent::SessionFinished { id, errored: true } => {
                (id.clone(), Sound::Error, "Run failed")
            }
            StoreEvent::SessionNeedsInput { id } => {
                (id.clone(), Sound::Input, "Waiting on your input")
            }
        };
        let Some(metadata) = store
            .read(cx)
            .sessions_metadata()
            .find(|metadata| metadata.id == id)
            .cloned()
        else {
            return;
        };
        if metadata.side_chat {
            return;
        }
        let settings = AgentChatSettings::get_global(cx).clone();
        let wu_active = cx.active_window().is_some();
        if settings.notify_only_in_background && wu_active {
            return;
        }
        let play_sound = match sound {
            Sound::Done => settings.sound_when_done,
            Sound::Input => settings.sound_when_needs_input,
            Sound::Error => settings.sound_on_error,
        };
        if play_sound {
            play(sound, cx);
        }
        if settings.notifications {
            let title = if metadata.title.is_empty() {
                "New chat".to_string()
            } else {
                metadata.title.clone()
            };
            show(
                store,
                id,
                metadata.project_root().to_path_buf(),
                heading.into(),
                title.into(),
                cx,
            );
        }
    })
    .detach();
}

struct ChatNotification {
    store: Entity<AgentStore>,
    chat_id: String,
    project_root: PathBuf,
    heading: SharedString,
    title: SharedString,
    _dismiss: Task<()>,
}

fn show(
    store: Entity<AgentStore>,
    chat_id: String,
    project_root: PathBuf,
    heading: SharedString,
    title: SharedString,
    cx: &mut App,
) {
    let Some(screen) = cx.primary_display() else {
        return;
    };
    let screen_bounds = screen.bounds();
    let notification_size = size(px(NOTIFICATION_SIZE.0), px(NOTIFICATION_SIZE.1));
    let origin = point(
        screen_bounds.right() - notification_size.width - px(16.),
        screen_bounds.top() + px(36.),
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, notification_size))),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        display_id: Some(screen.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    cx.open_window(options, move |window, cx| {
        cx.new(|cx| {
            let handle = window.window_handle();
            ChatNotification {
                store,
                chat_id,
                project_root,
                heading,
                title,
                _dismiss: cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(AUTO_DISMISS).await;
                    handle
                        .update(cx, |_, window, _| window.remove_window())
                        .log_err();
                }),
            }
        })
    })
    .log_err();
}

impl ChatNotification {
    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = cx
            .windows()
            .into_iter()
            .filter_map(|window| window.downcast::<MultiWorkspace>())
            .max_by_key(|handle: &WindowHandle<MultiWorkspace>| {
                handle
                    .read(cx)
                    .map(|multi| {
                        multi
                            .workspace()
                            .read(cx)
                            .project()
                            .read(cx)
                            .visible_worktrees(cx)
                            .any(|worktree| worktree.read(cx).abs_path().as_ref() == self.project_root)
                    })
                    .unwrap_or(false)
            });
        let opening = self
            .store
            .update(cx, |store, cx| store.open_session(&self.chat_id, cx));
        if let Some(target) = target {
            cx.spawn(async move |_, cx| {
                let session = opening.await?;
                target.update(cx, |multi, window, cx| {
                    window.activate_window();
                    multi.workspace().update(cx, |workspace, cx| {
                        open_chat(workspace, session, window, cx)
                    });
                })
            })
            .detach_and_log_err(cx);
        }
        window.remove_window();
    }
}

impl Render for ChatNotification {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        h_flex()
            .id("agent-notification")
            .size_full()
            .p(px(12.))
            .gap(px(10.))
            .rounded(px(12.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.elevated_surface_background)
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| this.open(window, cx)))
            .child(
                Icon::new(IconName::ActivityAgent)
                    .size(IconSize::Medium)
                    .color(Color::Accent),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child(self.heading.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(colors.text_muted)
                            .child(self.title.clone()),
                    ),
            )
            .child(
                div()
                    .id("agent-notification-dismiss")
                    .p(px(4.))
                    .rounded(px(6.))
                    .hover(|style| style.bg(colors.element_hover))
                    .child(
                        Icon::new(IconName::AgentClose)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .on_click(cx.listener(|_, _, window, cx| {
                        cx.stop_propagation();
                        window.remove_window();
                    })),
            )
    }
}
