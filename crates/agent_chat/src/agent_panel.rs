use crate::{
    AgentKind, NewClaudeChat, NewCodexChat, ToggleFocus,
    chat_view::ChatView,
    session::{AgentSession, AgentStore, ChatOutcome, ChatSort, SessionSummary},
};
use anyhow::Result;
use collections::HashSet;
use editor::{Editor, EditorEvent};
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, ClipboardItem, Context, DismissEvent, Entity,
    EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    Pixels, Point, PromptLevel, Render, SharedString, Styled, Subscription, WeakEntity, Window,
    anchored, deferred, div, px,
};
use project::Project;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use time_format::TimestampFormat;
use ui::{
    CommonAnimationExt as _, ContextMenu, Icon, IconButton, IconName, IconPosition, IconSize,
    Label, ListItem, PopoverMenu, Tooltip, prelude::*,
};
use util::ResultExt as _;
use workspace::{
    SaveIntent, SplitDirection, Toast, Workspace,
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

fn chat_roots(project: &Entity<Project>, cx: &App) -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = project
        .read(cx)
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        .collect();
    if roots.is_empty() {
        vec![paths::home_dir().clone()]
    } else {
        roots
    }
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
    let cwd = chat_roots(&project, cx)
        .into_iter()
        .next()
        .unwrap_or_else(|| paths::home_dir().clone());
    let store = store_for(workspace, cx);
    let session = store.update(cx, |store, cx| store.create_session(kind, cwd, cx));
    open_chat(workspace, session, window, cx);
}

pub(crate) fn open_chat(
    workspace: &mut Workspace,
    session: Entity<AgentSession>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open_chat_in(workspace, session, false, window, cx)
}

fn open_chat_in(
    workspace: &mut Workspace,
    session: Entity<AgentSession>,
    split: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let existing = workspace.items_of_type::<ChatView>(cx).find(|view| {
        let view = view.read(cx);
        view.session() == &session && !view.is_subagent()
    });
    if let Some(view) = existing {
        workspace.activate_item(&view, true, true, window, cx);
        return;
    }
    let store = store_for(workspace, cx);
    let project = workspace.project().clone();
    let weak_workspace = cx.entity().downgrade();
    let view = cx.new(|cx| ChatView::new(session, store, project, weak_workspace, window, cx));
    if split {
        workspace.split_item(SplitDirection::Right, Box::new(view), window, cx);
    } else {
        workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
    }
}

#[derive(Clone)]
struct DraggedChat {
    id: SharedString,
    title: SharedString,
}

impl Render for DraggedChat {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .shadow_md()
            .child(Label::new(self.title.clone()).size(LabelSize::Small))
    }
}

enum Renaming {
    Chat(SharedString),
    Section(String),
}

struct RenameField {
    target: Renaming,
    editor: Entity<Editor>,
    _subscription: Subscription,
}

struct RowMenu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

pub struct AgentPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    store: Entity<AgentStore>,
    focus_handle: FocusHandle,
    position: DockPosition,
    search: Entity<Editor>,
    renaming: Option<RenameField>,
    row_menu: Option<RowMenu>,
    show_archived: bool,
    _subscriptions: Vec<Subscription>,
}

impl AgentPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let store = store_for(workspace, cx);
            let project = workspace.project().clone();
            let weak_workspace = cx.entity().downgrade();
            cx.new(|cx| {
                let search = cx.new(|cx| {
                    let mut editor = Editor::single_line(window, cx);
                    editor.set_placeholder_text("Search chats…", window, cx);
                    editor
                });
                Self {
                    _subscriptions: vec![
                        cx.observe(&store, |_, _, cx| cx.notify()),
                        cx.observe(&project, |_, _, cx| cx.notify()),
                        cx.subscribe(&search, |_, _, event: &EditorEvent, cx| {
                            if matches!(event, EditorEvent::BufferEdited) {
                                cx.notify();
                            }
                        }),
                    ],
                    workspace: weak_workspace,
                    project,
                    store,
                    focus_handle: cx.focus_handle(),
                    position: DockPosition::Left,
                    search,
                    renaming: None,
                    row_menu: None,
                    show_archived: false,
                }
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

    fn fork(&mut self, id: SharedString, side_chat: bool, window: &mut Window, cx: &mut Context<Self>) {
        let task = self
            .store
            .update(cx, |store, cx| store.fork_session(&id, side_chat, cx));
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            let session = task.await?;
            workspace.update_in(cx, |workspace, window, cx| {
                open_chat_in(workspace, session, side_chat, window, cx)
            })
        })
        .detach_and_log_err(cx);
    }

    fn close_views(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                let open_views: Vec<_> = workspace
                    .items_of_type::<ChatView>(cx)
                    .filter(|view| view.read(cx).session().read(cx).metadata().id == id)
                    .collect();
                for view in open_views {
                    for pane in workspace.panes().to_vec() {
                        pane.update(cx, |pane, cx| {
                            if pane.index_for_item(&view).is_some() {
                                pane.close_item_by_id(view.entity_id(), SaveIntent::Skip, window, cx)
                                    .detach_and_log_err(cx);
                            }
                        });
                    }
                }
            })
            .log_err();
    }

    fn confirm_delete(&mut self, id: SharedString, title: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Delete \"{title}\"?"),
            Some("The chat and its history are removed from Wu. This can't be undone."),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                this.close_views(&id, window, cx);
                this.store
                    .update(cx, |store, cx| store.delete_session(&id, cx));
            })
            .log_err();
        })
        .detach();
    }

    fn start_rename(&mut self, target: Renaming, current: String, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(current, window, cx);
            editor.select_all(&Default::default(), window, cx);
            editor
        });
        let subscription = cx.subscribe_in(&editor, window, |this, _, event: &EditorEvent, window, cx| {
            if matches!(event, EditorEvent::Blurred) {
                this.finish_rename(true, window, cx);
            }
        });
        window.focus(&editor.focus_handle(cx), cx);
        self.renaming = Some(RenameField {
            target,
            editor,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn finish_rename(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.renaming.take() else {
            return;
        };
        if save {
            let text = field.editor.read(cx).text(cx);
            self.store.update(cx, |store, cx| match &field.target {
                Renaming::Chat(id) => store.rename_session(id, text, cx),
                Renaming::Section(id) => store.rename_section(id, text, cx),
            });
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn new_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self
            .store
            .update(cx, |store, cx| store.create_section("New section".into(), cx));
        self.start_rename(Renaming::Section(id), "New section".into(), window, cx);
    }

    fn show_row_menu(
        &mut self,
        session: &SessionSummary,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id: SharedString = session.id.clone().into();
        let title = session.title.clone();
        let pinned = session.pinned;
        let archived = session.archived;
        let current_section = session.section.clone();
        let native_id = session.native_session_id.clone();
        let sections = self.store.read(cx).list_prefs().sections.clone();
        let panel = cx.entity().downgrade();
        let menu = ContextMenu::build(window, cx, move |menu, _, _| {
            let action = |panel: &WeakEntity<AgentPanel>,
                          handler: Box<dyn Fn(&mut AgentPanel, &mut Window, &mut Context<AgentPanel>)>| {
                let panel = panel.clone();
                move |window: &mut Window, cx: &mut App| {
                    panel.update(cx, |panel, cx| handler(panel, window, cx)).log_err();
                }
            };
            let menu = menu
                .entry("Rename", None, {
                    let (id, title) = (id.clone(), title.clone());
                    action(&panel, Box::new(move |panel, window, cx| {
                        panel.start_rename(Renaming::Chat(id.clone()), title.to_string(), window, cx)
                    }))
                })
                .entry(if pinned { "Unpin" } else { "Pin" }, None, {
                    let id = id.clone();
                    action(&panel, Box::new(move |panel, _, cx| {
                        panel.store.update(cx, |store, cx| store.set_pinned(&id, !pinned, cx))
                    }))
                })
                .entry(if archived { "Unarchive" } else { "Archive" }, None, {
                    let id = id.clone();
                    action(&panel, Box::new(move |panel, _, cx| {
                        panel.store.update(cx, |store, cx| store.set_archived(&id, !archived, cx))
                    }))
                })
                .submenu("Move to Section", {
                    let (id, sections, current_section, panel) =
                        (id.clone(), sections.clone(), current_section.clone(), panel.clone());
                    move |menu, _, _| {
                        let mut menu = menu.toggleable_entry(
                            "No Section",
                            current_section.is_none(),
                            IconPosition::Start,
                            None,
                            {
                                let (id, panel) = (id.clone(), panel.clone());
                                move |_, cx| {
                                    panel
                                        .update(cx, |panel, cx| {
                                            panel.store.update(cx, |store, cx| {
                                                store.move_to_section(&id, None, cx)
                                            })
                                        })
                                        .log_err();
                                }
                            },
                        );
                        for section in &sections {
                            let (id, panel, section_id) = (id.clone(), panel.clone(), section.id.clone());
                            menu = menu.toggleable_entry(
                                section.name.clone(),
                                current_section.as_deref() == Some(section.id.as_str()),
                                IconPosition::Start,
                                None,
                                move |_, cx| {
                                    panel
                                        .update(cx, |panel, cx| {
                                            panel.store.update(cx, |store, cx| {
                                                store.move_to_section(&id, Some(section_id.clone()), cx)
                                            })
                                        })
                                        .log_err();
                                },
                            );
                        }
                        menu
                    }
                })
                .separator()
                .entry("Fork Chat", None, {
                    let id = id.clone();
                    action(&panel, Box::new(move |panel, window, cx| {
                        panel.fork(id.clone(), false, window, cx)
                    }))
                })
                .entry("New Side Chat", None, {
                    let id = id.clone();
                    action(&panel, Box::new(move |panel, window, cx| {
                        panel.fork(id.clone(), true, window, cx)
                    }))
                });
            let menu = match native_id.clone() {
                Some(native_id) => menu.entry("Copy Agent Session ID", None, move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(native_id.clone()))
                }),
                None => menu,
            };
            menu.separator().entry("Delete…", None, {
                let (id, title) = (id.clone(), title.clone());
                action(&panel, Box::new(move |panel, window, cx| {
                    panel.confirm_delete(id.clone(), title.clone(), window, cx)
                }))
            })
        });
        let dismiss = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.row_menu = None;
            cx.notify();
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.row_menu = Some(RowMenu {
            menu,
            position,
            _dismiss: dismiss,
        });
        cx.notify();
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
                            menu.entry(kind.label(), Some(new_chat_action(kind)), move |window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| new_chat(workspace, kind, window, cx))
                                    .log_err();
                            })
                        })
                }))
            })
    }

    fn options_menu(&self, cx: &Context<Self>) -> impl IntoElement {
        let prefs = self.store.read(cx).list_prefs().clone();
        let panel = cx.entity().downgrade();
        PopoverMenu::new("agent-panel-options")
            .trigger_with_tooltip(
                IconButton::new("agent-panel-options-trigger", IconName::Ellipsis)
                    .icon_size(IconSize::Small),
                Tooltip::text("Chat List Options"),
            )
            .anchor(gpui::Anchor::TopRight)
            .menu(move |window, cx| {
                let prefs = prefs.clone();
                let panel = panel.clone();
                Some(ContextMenu::build(window, cx, move |menu, _, _| {
                    let toggle = |label: &'static str,
                                  on: bool,
                                  change: fn(&mut crate::session::ChatListPrefs)| {
                        let panel = panel.clone();
                        (label, on, move |_: &mut Window, cx: &mut App| {
                            panel
                                .update(cx, |panel, cx| {
                                    panel.store.update(cx, |store, cx| store.update_list_prefs(change, cx))
                                })
                                .log_err();
                        })
                    };
                    let items = [
                        toggle("Show Chats from All Projects", prefs.all_projects, |prefs| {
                            prefs.all_projects = !prefs.all_projects
                        }),
                        toggle("Group by Project", prefs.group_by_project, |prefs| {
                            prefs.group_by_project = !prefs.group_by_project
                        }),
                        toggle("Compact Rows", prefs.compact_rows, |prefs| {
                            prefs.compact_rows = !prefs.compact_rows
                        }),
                    ];
                    let mut menu = menu.header("Show");
                    for (label, on, handler) in items {
                        menu = menu.toggleable_entry(label, on, IconPosition::Start, None, handler);
                    }
                    let sorts = [
                        toggle("Sort by Last Updated", prefs.sort == ChatSort::Updated, |prefs| {
                            prefs.sort = ChatSort::Updated
                        }),
                        toggle("Sort by Created", prefs.sort == ChatSort::Created, |prefs| {
                            prefs.sort = ChatSort::Created
                        }),
                    ];
                    menu = menu.separator();
                    for (label, on, handler) in sorts {
                        menu = menu.toggleable_entry(label, on, IconPosition::Start, None, handler);
                    }
                    let panel = panel.clone();
                    menu.separator().entry("New Section", None, move |window, cx| {
                        panel
                            .update(cx, |panel, cx| panel.new_section(window, cx))
                            .log_err();
                    })
                }))
            })
    }

    fn render_toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                h_flex()
                    .h_7()
                    .pl_2()
                    .pr_1()
                    .gap_1()
                    .child(
                        Label::new("Chats")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(div().flex_1())
                    .child(self.options_menu(cx))
                    .child(self.new_chat_menu()),
            )
            .child(
                h_flex()
                    .mx_2()
                    .mb_1p5()
                    .px_2()
                    .h_6()
                    .gap_1p5()
                    .rounded_md()
                    .bg(cx.theme().colors().editor_background)
                    .child(Icon::new(IconName::MagnifyingGlass).size(IconSize::XSmall).color(Color::Muted))
                    .child(div().flex_1().child(self.search.clone())),
            )
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

    fn status_indicator(session: &SessionSummary, cx: &App) -> AnyElement {
        if session.needs_input {
            Icon::new(IconName::AgentChatRoundLine)
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
            let dot = match (session.outcome, session.unseen) {
                (Some(ChatOutcome::Errored), _) => Some(Color::Error),
                (Some(ChatOutcome::Completed), true) => Some(Color::Accent),
                _ => None,
            };
            div()
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .when_some(dot, |this, dot| {
                    this.child(div().size(px(7.)).rounded_full().bg(dot.color(cx)))
                })
                .into_any_element()
        }
    }

    fn render_session(
        &self,
        session: SessionSummary,
        indent: bool,
        now: OffsetDateTime,
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id: SharedString = session.id.clone().into();
        let renaming = self.renaming.as_ref().filter(|field| {
            matches!(&field.target, Renaming::Chat(renaming_id) if *renaming_id == id)
        });
        let updated_at = OffsetDateTime::from_unix_timestamp(session.updated_at)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        let time = time_format::format_local_timestamp(updated_at, now, TimestampFormat::Relative);
        let meta = match &session.branch {
            Some(branch) => format!("{} · {time} · {branch}", session.kind.label()),
            None => format!("{} · {time}", session.kind.label()),
        };
        let title = session.title.clone();
        let menu_session = session.clone();
        let pinned = session.pinned;
        let archived = session.archived;
        let content: AnyElement = match renaming {
            Some(field) => div()
                .w_full()
                .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| this.finish_rename(true, window, cx)))
                .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| this.finish_rename(false, window, cx)))
                .child(field.editor.clone())
                .into_any_element(),
            None => v_flex()
                .min_w_0()
                .child(Label::new(title.clone()).truncate())
                .when(!compact, |this| {
                    this.child(
                        Label::new(meta)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .truncate(),
                    )
                })
                .into_any_element(),
        };
        let hover_actions = h_flex()
            .gap_0p5()
            .child(
                IconButton::new(
                    SharedString::from(format!("pin-{id}")),
                    IconName::AgentPin,
                )
                .icon_size(IconSize::Small)
                .toggle_state(pinned)
                .tooltip(Tooltip::text(if pinned { "Unpin" } else { "Pin" }))
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.store.update(cx, |store, cx| store.set_pinned(&id, !pinned, cx))
                    }
                })),
            )
            .child(
                IconButton::new(
                    SharedString::from(format!("archive-{id}")),
                    if archived { IconName::AgentUnarchive } else { IconName::AgentArchive },
                )
                .icon_size(IconSize::Small)
                .tooltip(Tooltip::text(if archived { "Unarchive" } else { "Archive" }))
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.store.update(cx, |store, cx| store.set_archived(&id, !archived, cx))
                    }
                })),
            );
        div()
            .id(SharedString::from(format!("agent-session-row-{id}")))
            .when(indent, |this| this.pl_4())
            .on_drag(
                DraggedChat {
                    id: id.clone(),
                    title,
                },
                |dragged, _, _, cx| cx.new(|_| dragged.clone()),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.show_row_menu(&menu_session, event.position, window, cx)
                }),
            )
            .child(
                ListItem::new(SharedString::from(format!("agent-session-{id}")))
                    .start_slot(Self::status_indicator(&session, cx))
                    .child(content)
                    .end_slot_on_hover(hover_actions)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_session(id.clone(), window, cx)
                    })),
            )
            .into_any_element()
    }

    fn section_header(
        &self,
        key: SharedString,
        label: SharedString,
        section_id: Option<Option<String>>,
        collapsible: bool,
        collapsed: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let renaming = self.renaming.as_ref().filter(|field| {
            matches!((&field.target, &section_id), (Renaming::Section(id), Some(Some(section))) if id == section)
        });
        let drop_target = section_id.clone();
        let highlight = cx.theme().colors().drop_target_background;
        let menu_section = section_id.clone().flatten();
        h_flex()
            .id(key.clone())
            .h_6()
            .px_2()
            .mt_1()
            .gap_1()
            .rounded_sm()
            .cursor_pointer()
            .when(collapsible, |this| {
                this.child(
                    Icon::new(if collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .size(IconSize::XSmall)
                    .color(Color::Muted),
                )
            })
            .child(match renaming {
                Some(field) => div()
                    .flex_1()
                    .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| this.finish_rename(true, window, cx)))
                    .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| this.finish_rename(false, window, cx)))
                    .child(field.editor.clone())
                    .into_any_element(),
                None => Label::new(label.clone())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .into_any_element(),
            })
            .when_some(drop_target, |this, target| {
                this.drag_over::<DraggedChat>(move |style, _, _, _| style.bg(highlight))
                    .on_drop(cx.listener(move |this, dragged: &DraggedChat, _, cx| {
                        let target = target.clone();
                        this.store.update(cx, |store, cx| {
                            store.move_to_section(&dragged.id, target, cx)
                        })
                    }))
            })
            .when(collapsible, |this| {
                let key = key.to_string();
                this.on_click(cx.listener(move |this, _, _, cx| {
                    let key = key.clone();
                    if key == "agent-archived" {
                        this.show_archived = !this.show_archived;
                        cx.notify();
                        return;
                    }
                    this.store.update(cx, |store, cx| {
                        store.update_list_prefs(
                            |prefs| {
                                if let Some(position) =
                                    prefs.collapsed_sections.iter().position(|id| *id == key)
                                {
                                    prefs.collapsed_sections.remove(position);
                                } else {
                                    prefs.collapsed_sections.push(key);
                                }
                            },
                            cx,
                        )
                    })
                }))
            })
            .when_some(menu_section, |this, section| {
                let label = label.to_string();
                this.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.show_section_menu(section.clone(), label.clone(), event.position, window, cx)
                    }),
                )
            })
            .into_any_element()
    }

    fn show_section_menu(
        &mut self,
        section: String,
        name: String,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity().downgrade();
        let menu = ContextMenu::build(window, cx, move |menu, _, _| {
            let rename = {
                let (panel, section, name) = (panel.clone(), section.clone(), name.clone());
                move |window: &mut Window, cx: &mut App| {
                    panel
                        .update(cx, |panel, cx| {
                            panel.start_rename(Renaming::Section(section.clone()), name.clone(), window, cx)
                        })
                        .log_err();
                }
            };
            let archive = {
                let (panel, section) = (panel.clone(), section.clone());
                move |_: &mut Window, cx: &mut App| {
                    panel
                        .update(cx, |panel, cx| {
                            panel.store.update(cx, |store, cx| store.archive_section(&section, cx))
                        })
                        .log_err();
                }
            };
            let delete = {
                let (panel, section) = (panel.clone(), section);
                move |_: &mut Window, cx: &mut App| {
                    panel
                        .update(cx, |panel, cx| {
                            panel.store.update(cx, |store, cx| store.delete_section(&section, cx))
                        })
                        .log_err();
                }
            };
            menu.entry("Rename Section", None, rename)
                .entry("Archive All", None, archive)
                .separator()
                .entry("Delete Section", None, delete)
        });
        let dismiss = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.row_menu = None;
            cx.notify();
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.row_menu = Some(RowMenu {
            menu,
            position,
            _dismiss: dismiss,
        });
        cx.notify();
    }

    fn matches_search(session: &SessionSummary, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let project = session
            .project_root
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        session.title.to_lowercase().contains(query)
            || project.contains(query)
            || session
                .branch
                .as_ref()
                .is_some_and(|branch| branch.to_lowercase().contains(query))
    }

    fn render_list(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let roots = chat_roots(&self.project, cx);
        let store = self.store.read(cx);
        let prefs = store.list_prefs().clone();
        let query = self.search.read(cx).text(cx).trim().to_lowercase();
        let sessions: Vec<SessionSummary> = store
            .sessions_in(&roots, cx)
            .into_iter()
            .filter(|session| Self::matches_search(session, &query))
            .collect();
        let now = OffsetDateTime::now_utc();
        let compact = prefs.compact_rows;
        let known_sections: HashSet<String> =
            prefs.sections.iter().map(|section| section.id.clone()).collect();
        let children_of = |parent: &SessionSummary| -> Vec<SessionSummary> {
            sessions
                .iter()
                .filter(|session| {
                    session.side_chat
                        && session.archived == parent.archived
                        && session.parent_id.as_deref() == Some(parent.id.as_str())
                })
                .cloned()
                .collect()
        };
        let is_top_level = |session: &SessionSummary| {
            !(session.side_chat
                && session.parent_id.as_ref().is_some_and(|parent| {
                    sessions
                        .iter()
                        .any(|other| other.id == *parent && other.archived == session.archived)
                }))
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        let push_with_children = |rows: &mut Vec<AnyElement>, session: &SessionSummary| {
            rows.push(self.render_session(session.clone(), false, now, compact, cx));
            for child in children_of(session) {
                rows.push(self.render_session(child, true, now, compact, cx));
            }
        };
        let live: Vec<&SessionSummary> = sessions
            .iter()
            .filter(|session| !session.archived && is_top_level(session))
            .collect();
        let pinned: Vec<&SessionSummary> = live.iter().copied().filter(|session| session.pinned).collect();
        if !pinned.is_empty() {
            rows.push(self.section_header("agent-pinned".into(), "PINNED".into(), None, false, false, cx));
            for session in pinned {
                push_with_children(&mut rows, session);
            }
        }
        for section in &prefs.sections {
            let members: Vec<&SessionSummary> = live
                .iter()
                .copied()
                .filter(|session| !session.pinned && session.section.as_deref() == Some(section.id.as_str()))
                .collect();
            let collapsed = prefs.collapsed_sections.contains(&section.id);
            rows.push(self.section_header(
                section.id.clone().into(),
                section.name.to_uppercase().into(),
                Some(Some(section.id.clone())),
                true,
                collapsed,
                cx,
            ));
            if !collapsed {
                for session in members {
                    push_with_children(&mut rows, session);
                }
            }
        }
        let rest: Vec<&SessionSummary> = live
            .iter()
            .copied()
            .filter(|session| {
                !session.pinned
                    && session
                        .section
                        .as_ref()
                        .is_none_or(|section| !known_sections.contains(section))
            })
            .collect();
        if !prefs.sections.is_empty() || !rows.is_empty() {
            rows.push(self.section_header("agent-chats".into(), "CHATS".into(), Some(None), false, false, cx));
        }
        if prefs.group_by_project {
            let mut projects: Vec<PathBuf> = Vec::new();
            for session in &rest {
                if !projects.contains(&session.project_root) {
                    projects.push(session.project_root.clone());
                }
            }
            for project in projects {
                let name = project_name(&project);
                rows.push(
                    div()
                        .px_2()
                        .pt_1()
                        .child(Label::new(name).size(LabelSize::XSmall).color(Color::Muted))
                        .into_any_element(),
                );
                for session in rest.iter().filter(|session| session.project_root == project) {
                    push_with_children(&mut rows, session);
                }
            }
        } else {
            for session in rest {
                push_with_children(&mut rows, session);
            }
        }
        let archived: Vec<&SessionSummary> = sessions
            .iter()
            .filter(|session| session.archived && is_top_level(session))
            .collect();
        if !archived.is_empty() {
            rows.push(self.section_header(
                "agent-archived".into(),
                format!("ARCHIVED ({})", archived.len()).into(),
                None,
                true,
                !self.show_archived,
                cx,
            ));
            if self.show_archived {
                for session in archived {
                    push_with_children(&mut rows, session);
                }
            }
        }
        rows
    }
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
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
        let rows = self.render_list(cx);
        let has_any = !self
            .store
            .read(cx)
            .sessions_in(&chat_roots(&self.project, cx), cx)
            .is_empty();
        v_flex()
            .id("agent-panel")
            .key_context("AgentPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(self.render_toolbar(cx))
            .map(|this| {
                if !has_any {
                    this.child(self.render_empty_state())
                } else {
                    this.child(
                        v_flex()
                            .id("agent-sessions")
                            .flex_1()
                            .overflow_y_scroll()
                            .p_1()
                            .children(rows),
                    )
                }
            })
            .when_some(self.row_menu.as_ref(), |this, row_menu| {
                this.child(
                    deferred(
                        anchored()
                            .position(row_menu.position)
                            .anchor(gpui::Anchor::TopLeft)
                            .child(row_menu.menu.clone()),
                    )
                    .with_priority(1),
                )
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
