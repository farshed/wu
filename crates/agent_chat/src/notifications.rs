use crate::{
    AgentChatSettings,
    agent_panel::open_chat,
    session::{AgentStore, StoreEvent},
};
use gpui::{App, AppContext as _, Entity, SystemNotification, TaskExt as _, WindowHandle};
use settings::Settings as _;
use util::ResultExt as _;
use workspace::MultiWorkspace;

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
    cx.on_system_notification_response({
        let store = store.clone();
        move |response, cx| open_chat_from_notification(&store, response.tag.as_ref(), cx)
    });
    cx.subscribe(store, |store, event, cx| {
        let (id, sound, status) = match event {
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
        let wu_active = is_app_active(cx);
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
                metadata.title
            };
            cx.show_system_notification(SystemNotification {
                tag: id.into(),
                title: title.into(),
                body: status.into(),
                actions: Vec::new(),
            });
        }
    })
    .detach();
}

fn open_chat_from_notification(store: &Entity<AgentStore>, chat_id: &str, cx: &mut App) {
    let Some(project_root) = store
        .read(cx)
        .sessions_metadata()
        .find(|metadata| metadata.id == chat_id)
        .map(|metadata| metadata.project_root().to_path_buf())
    else {
        return;
    };
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
                        .any(|worktree| worktree.read(cx).abs_path().as_ref() == project_root)
                })
                .unwrap_or(false)
        });
    let Some(target) = target else {
        return;
    };
    let opening = store.update(cx, |store, cx| store.open_session(chat_id, cx));
    cx.spawn(async move |cx| {
        let session = opening.await?;
        target.update(cx, |multi, window, cx| {
            window.activate_window();
            multi
                .workspace()
                .update(cx, |workspace, cx| open_chat(workspace, session, window, cx));
        })
    })
    .detach_and_log_err(cx);
}

pub(crate) fn is_app_active(cx: &mut App) -> bool {
    cx.windows().into_iter().any(|handle| {
        cx.update_window(handle, |_, window, _| window.is_window_active())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, IntoElement, Render, TestAppContext, VisualTestContext, Window, div};

    struct TestView;

    impl Render for TestView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn app_active_tracks_window_focus(cx: &mut TestAppContext) {
        assert!(!cx.update(is_app_active));

        let window = cx.add_window(|_, _| TestView);
        window
            .update(cx, |_, window, _| window.activate_window())
            .unwrap();
        let cx_visual = &mut VisualTestContext::from_window(*window, cx);
        cx_visual.run_until_parked();

        assert!(cx.update(is_app_active));

        cx_visual.deactivate_window();
        assert!(!cx.update(is_app_active));
    }
}


