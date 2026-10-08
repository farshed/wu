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

use command_palette_hooks::CommandPaletteFilter;
use futures::FutureExt as _;
use gpui::{App, EntityId, PromptLevel, TaskExt as _, WindowHandle, actions};
use serde::{Deserialize, Serialize};
use settings::{Settings as _, SettingsStore};
use util::ResultExt as _;
use workspace::{MultiWorkspace, SaveIntent};

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
    cx.set_global(workspace::QuitWarning(session::quit_warning));
    agent_panel::init(cx);
    watch_enabled(cx);
}

fn watch_enabled(cx: &mut App) {
    let mut enabled = AgentChatSettings::get_global(cx).enabled;
    show_commands(enabled, cx);
    cx.observe_global::<SettingsStore>(move |cx| {
        let now = AgentChatSettings::get_global(cx).enabled;
        if now == enabled {
            return;
        }
        enabled = now;
        show_commands(now, cx);
        if now {
            session::load_saved_chats(cx);
        } else {
            turn_off(cx);
        }
    })
    .detach();
}

fn show_commands(shown: bool, cx: &mut App) {
    CommandPaletteFilter::update_global(cx, |filter, _| {
        if shown {
            filter.show_namespace("agent_chat");
        } else {
            filter.hide_namespace("agent_chat");
        }
    });
}

fn workspace_windows(cx: &App) -> Vec<WindowHandle<MultiWorkspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .collect()
}

fn turn_off(cx: &mut App) {
    let working = session::working_agent_count(cx);
    let prompt_window = cx
        .active_window()
        .or_else(|| workspace_windows(cx).first().map(|window| (*window).into()));
    let (true, Some(prompt_window)) = (working > 0, prompt_window) else {
        close_agent_chats(cx);
        return;
    };
    let message = if working == 1 {
        "An agent is still working.".to_string()
    } else {
        format!("{working} agents are still working.")
    };
    let answer = prompt_window.update(cx, |_, window, cx| {
        window.prompt(
            PromptLevel::Warning,
            &message,
            Some("Turning off Agent Chat stops it."),
            &["Cancel", "Turn Off"],
            cx,
        )
    });
    match answer {
        Ok(answer) => cx
            .spawn(async move |cx| {
                if answer.await.ok() == Some(1) {
                    cx.update(close_agent_chats);
                } else {
                    cx.update(turn_back_on);
                }
            })
            .detach(),
        Err(error) => {
            log::error!("couldn't ask before turning off agent chat: {error:#}");
            close_agent_chats(cx);
        }
    }
}

fn turn_back_on(cx: &mut App) {
    settings::update_settings_file(<dyn fs::Fs>::global(cx), cx, |content, _| {
        content.agent_chat.get_or_insert_default().enabled = None;
    });
}

fn close_agent_chats(cx: &mut App) {
    session::stop_all_agents(cx);
    for window in workspace_windows(cx) {
        window
            .update(cx, |multi, window, cx| {
                let workspaces: Vec<_> = multi.workspaces().cloned().collect();
                for workspace in workspaces {
                    workspace.update(cx, |workspace, cx| {
                        if workspace
                            .active_modal::<saved_chats::SavedChatPicker>(cx)
                            .is_some()
                        {
                            workspace.hide_modal(window, cx);
                        }
                        workspace.close_panel::<AgentPanel>(window, cx);
                        for pane in workspace.panes().to_vec() {
                            pane.update(cx, |pane, cx| {
                                let chats: collections::HashSet<EntityId> = pane
                                    .items_of_type::<ChatView>()
                                    .map(|chat| chat.entity_id())
                                    .collect();
                                if !chats.is_empty() {
                                    pane.close_items(window, cx, SaveIntent::Skip, &|id| {
                                        chats.contains(&id)
                                    })
                                    .detach_and_log_err(cx);
                                }
                            });
                        }
                    });
                }
            })
            .log_err();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::new_store;
    use gpui::{AppContext as _, TestAppContext, UpdateGlobal as _};
    use project::FakeFs;

    fn set_enabled(enabled: bool, cx: &mut gpui::VisualTestContext) {
        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content.agent_chat.get_or_insert_default().enabled = Some(enabled);
                })
            })
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn turning_agent_chat_off_closes_chats_and_hides_its_commands(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
            command_palette_hooks::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        cx.update(watch_enabled);
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let chat = cx.new(|cx| {
                ChatView::new(
                    session.clone(),
                    store.clone(),
                    project.clone(),
                    weak,
                    window,
                    cx,
                )
            });
            workspace.add_item_to_active_pane(Box::new(chat), None, true, window, cx);
        });
        let chat_count = |cx: &mut gpui::VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace.items_of_type::<ChatView>(cx).count()
            })
        };
        let hidden = |cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| {
                CommandPaletteFilter::try_global(cx)
                    .is_some_and(|filter| filter.is_hidden(&NewClaudeChat))
            })
        };
        assert_eq!(chat_count(cx), 1);
        assert!(!hidden(cx));

        set_enabled(false, cx);
        assert_eq!(chat_count(cx), 0);
        assert!(hidden(cx));

        set_enabled(true, cx);
        assert!(!hidden(cx));
    }
}
