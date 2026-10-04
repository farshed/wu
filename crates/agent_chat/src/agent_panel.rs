use crate::{
    AgentKind, NewClaudeChat, NewCodexChat, ToggleFocus,
    chat_view::ChatView,
    session::{AgentSession, AgentStore, SessionSummary},
};
use anyhow::Result;
use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Pixels, Render, SharedString, Styled, Subscription, WeakEntity,
    Window, px,
};
use project::Project;
use std::path::PathBuf;
use time::OffsetDateTime;
use time_format::TimestampFormat;
use ui::{
    CommonAnimationExt as _, ContextMenu, Icon, IconButton, IconName, IconSize, Label, ListItem,
    PopoverMenu, Tooltip, prelude::*,
};
use util::ResultExt as _;
use workspace::{
    SaveIntent, Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
};

const AGENT_PANEL_KEY: &str = "AgentPanel";

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace
            .register_action(|workspace, _: &ToggleFocus, window, cx| {
                workspace.toggle_panel_focus::<AgentPanel>(window, cx);
            })
            .register_action(|workspace, _: &NewClaudeChat, window, cx| {
                new_chat(workspace, AgentKind::Claude, window, cx);
            })
            .register_action(|workspace, _: &NewCodexChat, window, cx| {
                new_chat(workspace, AgentKind::Codex, window, cx);
            });
    })
    .detach();
}

fn project_roots(project: &Entity<Project>, cx: &App) -> Vec<PathBuf> {
    project
        .read(cx)
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        .collect()
}

fn store_for(workspace: &Workspace, cx: &mut App) -> Entity<AgentStore> {
    AgentStore::global(workspace.app_state().languages.clone(), cx)
}

fn new_chat(
    workspace: &mut Workspace,
    kind: AgentKind,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();
    if project.read(cx).is_via_remote_server() {
        workspace.show_toast(
            Toast::new(
                NotificationId::unique::<AgentPanel>(),
                "Agent chats only work in local projects for now.",
            )
            .autohide(),
            cx,
        );
        return;
    }
    let cwd = project_roots(&project, cx)
        .into_iter()
        .next()
        .unwrap_or_else(|| paths::home_dir().clone());
    let store = store_for(workspace, cx);
    let session = store.update(cx, |store, cx| store.create_session(kind, cwd, cx));
    open_chat(workspace, session, window, cx);
}

fn open_chat(
    workspace: &mut Workspace,
    session: Entity<AgentSession>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let existing = workspace
        .items_of_type::<ChatView>(cx)
        .find(|view| view.read(cx).session() == &session);
    if let Some(view) = existing {
        workspace.activate_item(&view, true, true, window, cx);
        return;
    }
    let store = store_for(workspace, cx);
    let project = workspace.project().clone();
    let view = cx.new(|cx| ChatView::new(session, store, project, window, cx));
    workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
}

pub struct AgentPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    store: Entity<AgentStore>,
    focus_handle: FocusHandle,
    position: DockPosition,
    _subscriptions: Vec<Subscription>,
}

impl AgentPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, _, cx| {
            let store = store_for(workspace, cx);
            let project = workspace.project().clone();
            let weak_workspace = cx.entity().downgrade();
            cx.new(|cx| Self {
                _subscriptions: vec![
                    cx.observe(&store, |_, _, cx| cx.notify()),
                    cx.observe(&project, |_, _, cx| cx.notify()),
                ],
                workspace: weak_workspace,
                project,
                store,
                focus_handle: cx.focus_handle(),
                position: DockPosition::Left,
            })
        })
    }

    fn open_session(&mut self, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let task = self
            .store
            .update(cx, |store, cx| store.open_session(&id, cx));
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            let session = task.await?;
            workspace.update_in(cx, |workspace, window, cx| {
                open_chat(workspace, session, window, cx)
            })
        })
        .detach_and_log_err(cx);
    }

    fn delete_session(&mut self, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                let open_views: Vec<_> = workspace
                    .items_of_type::<ChatView>(cx)
                    .filter(|view| view.read(cx).session().read(cx).metadata().id == id.as_ref())
                    .collect();
                for view in open_views {
                    for pane in workspace.panes().to_vec() {
                        pane.update(cx, |pane, cx| {
                            if pane.index_for_item(&view).is_some() {
                                pane.close_item_by_id(
                                    view.entity_id(),
                                    SaveIntent::Skip,
                                    window,
                                    cx,
                                )
                                .detach_and_log_err(cx);
                            }
                        });
                    }
                }
            })
            .log_err();
        self.store
            .update(cx, |store, cx| store.delete_session(&id, cx));
    }

    fn new_chat_menu(&self) -> impl IntoElement {
        let workspace = self.workspace.clone();
        PopoverMenu::new("agent-panel-new-chat")
            .trigger_with_tooltip(
                IconButton::new("agent-panel-new-chat-trigger", IconName::Plus)
                    .icon_size(IconSize::Small),
                Tooltip::text("New Chat"),
            )
            .anchor(gpui::Anchor::TopRight)
            .menu(move |window, cx| {
                let workspace = workspace.clone();
                Some(ContextMenu::build(window, cx, move |menu, _, _| {
                    [AgentKind::Claude, AgentKind::Codex]
                        .into_iter()
                        .fold(menu, |menu, kind| {
                            let workspace = workspace.clone();
                            menu.entry(
                                kind.label(),
                                Some(new_chat_action(kind)),
                                move |window, cx| {
                                    workspace
                                        .update(cx, |workspace, cx| {
                                            new_chat(workspace, kind, window, cx)
                                        })
                                        .log_err();
                                },
                            )
                        })
                }))
            })
    }

    fn render_toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .h_7()
            .pl_2()
            .pr_1()
            .gap_2()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                Label::new("Chats")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(self.new_chat_menu())
    }

    fn render_empty_state(&self) -> impl IntoElement {
        let new_chat_button = |kind: AgentKind| {
            let workspace = self.workspace.clone();
            Button::new(
                SharedString::from(format!("agent-panel-empty-{}", kind.label())),
                format!("New {} chat", kind.label()),
            )
            .full_width()
            .style(ButtonStyle::Outlined)
            .on_click(move |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| new_chat(workspace, kind, window, cx))
                    .log_err();
            })
        };
        v_flex()
            .flex_1()
            .p_4()
            .gap_2()
            .justify_center()
            .child(
                Label::new("No chats in this project yet.")
                    .color(Color::Muted)
                    .mb_2(),
            )
            .child(new_chat_button(AgentKind::Claude))
            .child(new_chat_button(AgentKind::Codex))
    }

    fn render_session(
        &self,
        session: SessionSummary,
        now: OffsetDateTime,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let status = if session.needs_input {
            Icon::new(IconName::Chat)
                .size(IconSize::Small)
                .color(Color::Accent)
                .into_any_element()
        } else if session.working {
            Icon::new(IconName::LoadCircle)
                .size(IconSize::Small)
                .color(Color::Muted)
                .with_keyed_rotate_animation(
                    SharedString::from(format!("agent-session-spinner-{}", session.id)),
                    2,
                )
                .into_any_element()
        } else {
            Icon::new(IconName::Chat)
                .size(IconSize::Small)
                .color(Color::Muted)
                .into_any_element()
        };
        let updated_at = OffsetDateTime::from_unix_timestamp(session.updated_at)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        let time = time_format::format_local_timestamp(updated_at, now, TimestampFormat::Relative);
        let id: SharedString = session.id.into();
        ListItem::new(SharedString::from(format!("agent-session-{id}")))
            .start_slot(status)
            .child(
                v_flex()
                    .min_w_0()
                    .child(Label::new(session.title).truncate())
                    .child(
                        Label::new(format!("{} · {time}", session.kind.label()))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .truncate(),
                    ),
            )
            .end_slot_on_hover(
                IconButton::new(SharedString::from(format!("delete-{id}")), IconName::Trash)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Delete Chat"))
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| this.delete_session(id.clone(), window, cx)
                    })),
            )
            .on_click(
                cx.listener(move |this, _, window, cx| this.open_session(id.clone(), window, cx)),
            )
    }
}

fn new_chat_action(kind: AgentKind) -> Box<dyn Action> {
    match kind {
        AgentKind::Claude => Box::new(NewClaudeChat),
        AgentKind::Codex => Box::new(NewCodexChat),
    }
}

impl EventEmitter<PanelEvent> for AgentPanel {}

impl Focusable for AgentPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AgentPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let roots = project_roots(&self.project, cx);
        let sessions = self.store.read(cx).sessions_in(&roots, cx);
        let now = OffsetDateTime::now_utc();
        v_flex()
            .id("agent-panel")
            .key_context("AgentPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(self.render_toolbar(cx))
            .map(|this| {
                if sessions.is_empty() {
                    this.child(self.render_empty_state())
                } else {
                    this.child(
                        v_flex()
                            .id("agent-sessions")
                            .flex_1()
                            .overflow_y_scroll()
                            .p_1()
                            .children(
                                sessions
                                    .into_iter()
                                    .map(|session| self.render_session(session, now, cx)),
                            ),
                    )
                }
            })
    }
}

impl Panel for AgentPanel {
    fn persistent_name() -> &'static str {
        "Agent Panel"
    }

    fn panel_key() -> &'static str {
        AGENT_PANEL_KEY
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        self.position = position;
        cx.notify();
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(300.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::ActivityAgent)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Agent Chats")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        8
    }
}
