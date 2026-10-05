use super::{ChatView, icon};
use crate::chat_style::{format_elapsed, ink, popover_card, text_faint, ui};
use agent_harness::{BackgroundTaskKind, ToolCall};
use gpui::{
    Anchor, AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, anchored,
    deferred, div, point, px,
};
use std::time::Duration;
use theme::ActiveTheme as _;
use ui::prelude::*;

const TICK: Duration = Duration::from_secs(1);
const DETAILS_WIDTH: f32 = 360.;
const CLOSE_DEBOUNCE: Duration = Duration::from_millis(300);

struct TaskRow {
    kind: BackgroundTaskKind,
    title: SharedString,
    command: Option<SharedString>,
    output: Option<SharedString>,
    elapsed: SharedString,
    subagent: Option<(String, ToolCall)>,
}

fn kind_label(kind: BackgroundTaskKind) -> &'static str {
    match kind {
        BackgroundTaskKind::Shell => "Command",
        BackgroundTaskKind::Agent => "Agent",
        BackgroundTaskKind::Other => "Task",
    }
}

impl ChatView {
    pub(super) fn ensure_background_tick(&mut self, cx: &mut Context<Self>) {
        if self.session.read(cx).background_tasks().is_empty() {
            self.background_details = false;
            return;
        }
        if self.background_ticking {
            return;
        }
        self.background_ticking = true;
        self._background_tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let running = this
                    .update(cx, |this, cx| {
                        let running = !this.session.read(cx).background_tasks().is_empty();
                        if running {
                            cx.notify();
                        } else {
                            this.background_ticking = false;
                            this.background_details = false;
                        }
                        running
                    })
                    .unwrap_or(false);
                if !running {
                    return;
                }
            }
        });
    }

    fn background_rows(&self, cx: &Context<Self>) -> Vec<TaskRow> {
        let session = self.session.read(cx);
        session
            .background_tasks()
            .iter()
            .map(|task| {
                let tool = task
                    .tool_use_id
                    .as_deref()
                    .and_then(|id| session.tool_entry(id));
                let command = tool.and_then(|tool| match &tool.call {
                    ToolCall::Exec { command } => Some(SharedString::from(command.clone())),
                    _ => None,
                });
                let title: SharedString = if !task.description.trim().is_empty() {
                    task.description.clone().into()
                } else if let Some(command) = &command {
                    command.clone()
                } else {
                    format!("Background {}", kind_label(task.kind).to_lowercase()).into()
                };
                TaskRow {
                    kind: task.kind,
                    title,
                    command,
                    output: tool
                        .and_then(|tool| tool.output.clone())
                        .filter(|output| !output.trim().is_empty()),
                    elapsed: format_elapsed(task.started_at.elapsed().as_secs()).into(),
                    subagent: tool
                        .filter(|tool| tool.call.is_subagent_spawn())
                        .map(|tool| (tool.id.clone(), tool.call.clone())),
                }
            })
            .collect()
    }

    pub(super) fn render_background_indicator(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let tasks = self.session.read(cx).background_tasks();
        let oldest = tasks.iter().map(|task| task.started_at).min()?;
        let label: SharedString = match tasks {
            [task] => format!("{} running", kind_label(task.kind)).into(),
            _ => format!("{} tasks running", tasks.len()).into(),
        };
        let elapsed = format_elapsed(oldest.elapsed().as_secs());
        let colors = cx.theme().colors();
        let open = self.background_details;
        Some(
            h_flex()
                .id("agent-background-tasks")
                .debug_selector(|| "agent-background-tasks".into())
                .flex_none()
                .h(px(20.))
                .px(px(7.))
                .gap(px(5.))
                .rounded(px(6.))
                .text_size(ui(11.))
                .text_color(colors.text_muted)
                .cursor_pointer()
                .when(open, |this| this.bg(ink(0.06, cx)))
                .hover(|style| style.bg(ink(0.06, cx)).text_color(colors.text))
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(cx.theme().status().info),
                )
                .child(label)
                .child(div().text_color(text_faint(cx)).child(elapsed))
                .on_click(cx.listener(|this, _, _, cx| {
                    // The card closes on this same press via its outside-click handler.
                    let just_closed = this
                        .background_closed_at
                        .is_some_and(|closed| closed.elapsed() < CLOSE_DEBOUNCE);
                    this.background_details = !this.background_details && !just_closed;
                    cx.notify();
                }))
                .when(open, |this| {
                    this.child(deferred(
                        anchored()
                            .anchor(Anchor::BottomLeft)
                            .offset(point(px(0.), px(-6.)))
                            .snap_to_window_with_margin(px(8.))
                            .child(self.render_background_details(cx)),
                    ))
                })
                .into_any_element(),
        )
    }

    fn render_background_details(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let faint = text_faint(cx);
        let code_font = super::code_font(cx);
        popover_card(cx)
            .id("agent-background-details")
            .w(px(DETAILS_WIDTH))
            .max_h(px(360.))
            .overflow_y_scroll()
            .p(px(6.))
            .gap(px(2.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.background_details = false;
                this.background_closed_at = Some(std::time::Instant::now());
                cx.notify();
            }))
            .children(
                self.background_rows(cx)
                    .into_iter()
                    .enumerate()
                    .map(|(index, row)| {
                        let opens_subagent = row.subagent.is_some();
                        v_flex()
                            .id(("agent-background-task", index))
                            .px(px(8.))
                            .py(px(6.))
                            .gap(px(4.))
                            .rounded(px(7.))
                            .text_size(ui(12.))
                            .when(opens_subagent, |this| {
                                this.cursor_pointer().hover(|style| style.bg(ink(0.05, cx)))
                            })
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(
                                        icon(
                                            match row.kind {
                                                BackgroundTaskKind::Shell => {
                                                    IconName::AgentTerminal
                                                }
                                                BackgroundTaskKind::Agent => IconName::AgentBot,
                                                BackgroundTaskKind::Other => IconName::AgentWidget,
                                            },
                                            14.,
                                        )
                                        .text_color(colors.text_muted),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(colors.text)
                                            .child(row.title),
                                    )
                                    .child(div().flex_none().text_color(faint).child(row.elapsed)),
                            )
                            .when_some(row.command, |this, command| {
                                this.child(
                                    div()
                                        .pl(px(22.))
                                        .text_size(ui(11.))
                                        .text_color(colors.text_muted)
                                        .font(code_font.clone())
                                        .child(command),
                                )
                            })
                            .when_some(row.output, |this, output| {
                                this.child(
                                    div()
                                        .pl(px(22.))
                                        .text_size(ui(11.))
                                        .text_color(faint)
                                        .child(output),
                                )
                            })
                            .when(opens_subagent, |this| {
                                this.child(
                                    div()
                                        .pl(px(22.))
                                        .text_size(ui(11.))
                                        .text_color(faint)
                                        .child("Click to open"),
                                )
                            })
                            .when_some(row.subagent, |this, (id, call)| {
                                this.on_click(cx.listener(move |this, _, window, cx| {
                                    this.background_details = false;
                                    this.open_subagent(id.clone(), &call, window, cx);
                                }))
                            })
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentKind,
        session::tests::{feed_events, new_store},
    };
    use agent_harness::AgentEvent;
    use gpui::{TestAppContext, WeakEntity};
    use project::FakeFs;

    #[gpui::test]
    async fn details_show_the_command_and_link_subagents(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        feed_events(
            &session,
            vec![
                AgentEvent::ToolCall {
                    id: "toolu_bash".into(),
                    call: ToolCall::Exec {
                        command: "cargo test --workspace".into(),
                    },
                },
                AgentEvent::ToolCall {
                    id: "toolu_agent".into(),
                    call: ToolCall::Unknown {
                        name: "Agent: scan the repo".into(),
                        input: None,
                    },
                },
                AgentEvent::TaskStarted {
                    task_id: "bg1".into(),
                    tool_use_id: Some("toolu_bash".into()),
                    kind: BackgroundTaskKind::Shell,
                    description: String::new(),
                },
                AgentEvent::TaskStarted {
                    task_id: "a1".into(),
                    tool_use_id: Some("toolu_agent".into()),
                    kind: BackgroundTaskKind::Agent,
                    description: "Scan the repo".into(),
                },
                AgentEvent::BackgroundTasksChanged {
                    tasks: vec![
                        agent_harness::BackgroundTaskInfo {
                            task_id: "bg1".into(),
                            kind: BackgroundTaskKind::Shell,
                            description: String::new(),
                        },
                        agent_harness::BackgroundTaskInfo {
                            task_id: "a1".into(),
                            kind: BackgroundTaskKind::Agent,
                            description: String::new(),
                        },
                    ],
                },
            ],
            cx,
        );
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(
                session.clone(),
                store.clone(),
                project,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        chat.update(cx, |chat, cx| {
            assert!(chat.render_background_indicator(cx).is_some());
            let rows = chat.background_rows(cx);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].title.as_ref(), "cargo test --workspace");
            assert_eq!(
                rows[0].command.as_ref().map(|c| c.as_ref()),
                Some("cargo test --workspace")
            );
            assert!(rows[0].subagent.is_none());
            assert_eq!(rows[1].title.as_ref(), "Scan the repo");
            assert_eq!(
                rows[1].subagent.as_ref().map(|(id, _)| id.as_str()),
                Some("toolu_agent")
            );
        });
        feed_events(
            &session,
            vec![AgentEvent::BackgroundTasksChanged { tasks: Vec::new() }],
            cx,
        );
        chat.update(cx, |chat, cx| {
            assert!(chat.render_background_indicator(cx).is_none())
        });
    }

    #[gpui::test]
    async fn clicking_the_indicator_toggles_the_details(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        crate::session::tests::feed_user(&session, "run the tests in the background", cx);
        feed_events(
            &session,
            vec![AgentEvent::BackgroundTasksChanged {
                tasks: vec![agent_harness::BackgroundTaskInfo {
                    task_id: "bg1".into(),
                    kind: BackgroundTaskKind::Shell,
                    description: "Run tests".into(),
                }],
            }],
            cx,
        );
        let (chat, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(
                session.clone(),
                store.clone(),
                project,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        cx.run_until_parked();
        let chip = cx
            .debug_bounds("agent-background-tasks")
            .expect("indicator is drawn")
            .center();
        cx.simulate_click(chip, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(chat.read_with(cx, |chat, _| chat.background_details));
        cx.simulate_click(chip, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(
            !chat.read_with(cx, |chat, _| chat.background_details),
            "a second click on the indicator closes the details"
        );
    }
}
