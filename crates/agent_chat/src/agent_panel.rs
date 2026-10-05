use crate::{
    AgentKind, NewClaudeChat, NewCodexChat, ToggleFocus,
    chat_style::{accent, ink, selected_row, text_faint, ui, wash},
    chat_view::ChatView,
    session::{AgentSession, AgentStore, ChatListPrefs, ChatOutcome, SessionSummary},
};
use anyhow::Result;
use collections::HashSet;
use editor::{Editor, EditorEvent};
use gpui::{
    Action, Animation, AnimationExt as _, AnyElement, App, AsyncWindowContext, Bounds, ClickEvent,
    ClipboardItem, Context, CursorStyle, DismissEvent, Div, EdgeFade, Element, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, PromptLevel,
    Render, RenderOnce, ScrollHandle, SharedString, Stateful, Styled, Subscription, Svg,
    TextStyleRefinement,
    Transformation, WeakEntity, Window, anchored, deferred, div, percentage, point, px, rgb, svg,
};
use project::Project;
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    time::Duration,
};
use time::OffsetDateTime;
use ui::{
    Clickable, ContextMenu, IconName, IconPosition, Label, PopoverMenu, Toggleable, Tooltip,
    prelude::*,
};
use util::ResultExt as _;
use workspace::{
    SaveIntent, SplitDirection, Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
};

const AGENT_PANEL_KEY: &str = "AgentPanel";

const SPACE_SM: f32 = 8.0;
const LIST_GAP: f32 = 2.0;
const LIST_PAD_TOP: f32 = 4.0;
const SECTION_GAP: f32 = 12.0;
const DISCLOSURE_HEADER_HEIGHT: f32 = 28.0;
const DISCLOSURE_BODY_INSET: f32 = 4.0;
const HEADER_CONTROL_SIZE: f32 = 29.0;
const HARNESS_ICON_SIZE: f32 = 13.0;
const COMPACT_TIME_WIDTH: f32 = 30.0;
const LABEL_FADE_BAND: f32 = 20.0;
const LIST_FADE_BAND: f32 = 24.0;
const CLAUDE_BRAND: u32 = 0xD97757;
const SPINNER_PERIOD: Duration = Duration::from_millis(750);
const SPINNER_DIM: f32 = 0.1;
const SPINNER_CELL: f32 = 2.0;
const SPINNER_RING: [[f32; 2]; 3] = [[0.0, 1.0], [5.0, 2.0], [4.0, 3.0]];

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
    project_root: PathBuf,
    archived: bool,
}

impl DraggedChat {
    fn can_drop_on_project(&self, project: &Path) -> bool {
        !self.archived && self.project_root == project
    }
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
    hovered_row: Option<SharedString>,
    list_scroll: ScrollHandle,
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
            let workspace_entity = cx.entity();
            let weak_workspace = workspace_entity.downgrade();
            cx.new(|cx| {
                let search = cx.new(|cx| {
                    let mut editor = Editor::single_line(window, cx);
                    editor.set_placeholder_text("Search chats…", window, cx);
                    editor.set_text_style_refinement(TextStyleRefinement {
                        font_size: Some(ui(13.).into()),
                        ..Default::default()
                    });
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
                        cx.subscribe(&workspace_entity, |_, _, event: &workspace::Event, cx| {
                            if matches!(event, workspace::Event::ActiveItemChanged) {
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
                    hovered_row: None,
                    list_scroll: ScrollHandle::new(),
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

    fn fork(
        &mut self,
        id: SharedString,
        side_chat: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    }

    fn confirm_delete(
        &mut self,
        id: SharedString,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn start_rename(
        &mut self,
        target: Renaming,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let font_size = match target {
            Renaming::Chat(_) => ui(13.),
            Renaming::Section(_) => ui(12.),
        };
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text_style_refinement(TextStyleRefinement {
                font_size: Some(font_size.into()),
                ..Default::default()
            });
            editor.set_text(current, window, cx);
            editor.select_all(&Default::default(), window, cx);
            editor
        });
        let subscription = cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &EditorEvent, window, cx| {
                if matches!(event, EditorEvent::Blurred) {
                    this.finish_rename(true, window, cx);
                }
            },
        );
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
                          handler: Box<
                dyn Fn(&mut AgentPanel, &mut Window, &mut Context<AgentPanel>),
            >| {
                let panel = panel.clone();
                move |window: &mut Window, cx: &mut App| {
                    panel
                        .update(cx, |panel, cx| handler(panel, window, cx))
                        .log_err();
                }
            };
            let menu = menu
                .entry("Rename", None, {
                    let (id, title) = (id.clone(), title.clone());
                    action(
                        &panel,
                        Box::new(move |panel, window, cx| {
                            panel.start_rename(
                                Renaming::Chat(id.clone()),
                                title.to_string(),
                                window,
                                cx,
                            )
                        }),
                    )
                })
                .entry(if pinned { "Unpin" } else { "Pin" }, None, {
                    let id = id.clone();
                    action(
                        &panel,
                        Box::new(move |panel, _, cx| {
                            panel
                                .store
                                .update(cx, |store, cx| store.set_pinned(&id, !pinned, cx))
                        }),
                    )
                })
                .entry(if archived { "Unarchive" } else { "Archive" }, None, {
                    let id = id.clone();
                    action(
                        &panel,
                        Box::new(move |panel, _, cx| {
                            panel
                                .store
                                .update(cx, |store, cx| store.set_archived(&id, !archived, cx))
                        }),
                    )
                })
                .submenu("Move to Section", {
                    let (id, sections, current_section, panel) = (
                        id.clone(),
                        sections.clone(),
                        current_section.clone(),
                        panel.clone(),
                    );
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
                            let (id, panel, section_id) =
                                (id.clone(), panel.clone(), section.id.clone());
                            menu = menu.toggleable_entry(
                                section.name.clone(),
                                current_section.as_deref() == Some(section.id.as_str()),
                                IconPosition::Start,
                                None,
                                move |_, cx| {
                                    panel
                                        .update(cx, |panel, cx| {
                                            panel.store.update(cx, |store, cx| {
                                                store.move_to_section(
                                                    &id,
                                                    Some(section_id.clone()),
                                                    cx,
                                                )
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
                    action(
                        &panel,
                        Box::new(move |panel, window, cx| {
                            panel.fork(id.clone(), false, window, cx)
                        }),
                    )
                })
                .entry("New Side Chat", None, {
                    let id = id.clone();
                    action(
                        &panel,
                        Box::new(move |panel, window, cx| panel.fork(id.clone(), true, window, cx)),
                    )
                });
            let menu = match native_id.clone() {
                Some(native_id) => menu.entry("Copy Agent Session ID", None, move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(native_id.clone()))
                }),
                None => menu,
            };
            menu.separator().entry("Delete…", None, {
                let (id, title) = (id.clone(), title.clone());
                action(
                    &panel,
                    Box::new(move |panel, window, cx| {
                        panel.confirm_delete(id.clone(), title.clone(), window, cx)
                    }),
                )
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
            .trigger(HeaderButton::new(
                "agent-panel-new-chat-trigger",
                IconName::Plus,
                "New Chat",
            ))
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
        let colors = cx.theme().colors();
        v_flex()
            .flex_none()
            .child(
                h_flex()
                    .gap(px(4.))
                    .px(px(SPACE_SM))
                    .pt(px(8.))
                    .pb(px(4.))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .h(px(HEADER_CONTROL_SIZE))
                            .gap(px(SPACE_SM))
                            .px(px(SPACE_SM))
                            .text_size(ui(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text.opacity(0.8))
                            .child(faded_label("agent-panel-title", false, "Chats")),
                    )
                    .child(self.new_chat_menu()),
            )
            .child(
                div().px(px(SPACE_SM)).pb(px(4.)).child(
                    h_flex()
                        .gap(px(SPACE_SM))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(7.))
                        .bg(ink(0.04, cx))
                        .text_size(ui(13.))
                        .child(glyph(IconName::AgentMagnifer, 16., colors.text_muted))
                        .child(div().flex_1().min_w_0().child(self.search.clone())),
                ),
            )
    }

    fn render_empty_state(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let text = colors.text;
        let hover = colors.element_hover;
        let muted = colors.text_muted.opacity(0.55);
        let new_chat_row = |kind: AgentKind| {
            let workspace = self.workspace.clone();
            let group = SharedString::from(format!("agent-panel-empty-{}", kind.label()));
            h_flex()
                .id(group.clone())
                .group(group.clone())
                .h(px(36.))
                .gap(px(10.))
                .px(px(SPACE_SM))
                .rounded(px(6.))
                .text_size(ui(13.))
                .text_color(muted)
                .cursor_pointer()
                .hover(move |style| style.bg(hover).text_color(text))
                .on_click(move |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| new_chat(workspace, kind, window, cx))
                        .log_err();
                })
                .child(
                    glyph(IconName::Plus, 14., muted)
                        .group_hover(group, move |style| style.text_color(text)),
                )
                .child(format!("New {} chat", kind.label()))
        };
        v_flex()
            .px(px(SPACE_SM))
            .pt(px(LIST_PAD_TOP))
            .child(
                div()
                    .px(px(SPACE_SM))
                    .pb(px(SPACE_SM))
                    .text_size(ui(12.))
                    .text_color(text_faint(cx))
                    .child("No chats in this project yet."),
            )
            .child(
                v_flex()
                    .gap(px(LIST_GAP))
                    .child(new_chat_row(AgentKind::Claude))
                    .child(new_chat_row(AgentKind::Codex)),
            )
    }

    fn active_chat_id(&self, cx: &App) -> Option<String> {
        let workspace = self.workspace.upgrade()?;
        let view = workspace.read(cx).active_item(cx)?.downcast::<ChatView>()?;
        let id = view.read(cx).session().read(cx).metadata().id.clone();
        Some(id)
    }

    fn rename_field(&self, editor: &Entity<Editor>, cx: &Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .h(px(21.))
            .my(px(-2.))
            .px(px(4.))
            .flex()
            .items_center()
            .rounded(px(4.))
            .border_1()
            .border_color(accent(cx))
            .bg(ink(0.03, cx))
            .text_color(cx.theme().colors().text)
            .cursor_text()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                this.finish_rename(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                this.finish_rename(false, window, cx)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(editor.clone()),
            )
            .into_any_element()
    }

    fn row_action(
        &self,
        session_id: &SharedString,
        action: RowAction,
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let text_muted = colors.text_muted;
        let (key, icon, text, show_label, icon_color) = match action {
            RowAction::Pin { pinned } => (
                "pin",
                IconName::AgentPin,
                if pinned { "Unpin" } else { "Pin" },
                false,
                if pinned { colors.text } else { text_muted },
            ),
            RowAction::Archive { archived } => (
                "archive",
                if archived {
                    IconName::AgentUnarchive
                } else {
                    IconName::AgentArchive
                },
                if archived { "Unarchive" } else { "Archive" },
                !compact,
                text_muted,
            ),
        };
        let rest = wash(0.10, cx);
        let pressed = wash(0.18, cx);
        let id = session_id.clone();
        h_flex()
            .id(SharedString::from(format!(
                "agent-session-{key}-{session_id}"
            )))
            .flex_none()
            .h(px(18.))
            .gap(px(4.))
            .cursor_pointer()
            .map(|this| {
                if compact {
                    this.w(px(18.)).justify_center()
                } else {
                    this.px(px(4.))
                        .rounded(px(5.))
                        .bg(rest)
                        .hover(move |style| style.bg(pressed))
                }
            })
            .child(glyph(
                icon,
                if compact { HARNESS_ICON_SIZE } else { 11. },
                icon_color,
            ))
            .when(show_label, |this| {
                this.child(div().text_size(ui(10.)).text_color(text_muted).child(text))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.hovered_row = None;
                this.store.update(cx, |store, cx| match action {
                    RowAction::Pin { pinned } => store.set_pinned(&id, !pinned, cx),
                    RowAction::Archive { archived } => store.set_archived(&id, !archived, cx),
                });
            }))
            .tooltip(Tooltip::text(text))
            .into_any_element()
    }

    fn render_session(
        &self,
        session: SessionSummary,
        nested: bool,
        now: i64,
        compact: bool,
        selected: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id: SharedString = session.id.clone().into();
        let colors = cx.theme().colors();
        let text = colors.text;
        let subline = colors.text_muted.opacity(0.5);
        let hovered = self.hovered_row.as_ref() == Some(&id);
        let archived = session.archived;
        let archived_muted = archived && !selected && !hovered;
        let status = RowStatus::of(&session);
        let status_color = status.color(cx);
        let time_ago: SharedString = format_time_ago(session.updated_at, now).into();
        let branch = session.branch.clone().map(SharedString::from);
        let row_height = row_height(compact, branch.is_some());
        let renaming = self.renaming.as_ref().filter(
            |field| matches!(&field.target, Renaming::Chat(renaming_id) if *renaming_id == id),
        );
        let title = session.title.clone();
        let menu_session = session.clone();
        let status_glyph = || -> AnyElement {
            match status {
                RowStatus::Working => {
                    mini_spinner(format!("agent-session-working-{id}").into(), cx)
                        .into_any_element()
                }
                RowStatus::Done => {
                    glyph(IconName::AgentCheck, 11., status_color).into_any_element()
                }
                _ => div()
                    .size(px(6.))
                    .flex_none()
                    .rounded_full()
                    .bg(status_color)
                    .into_any_element(),
            }
        };
        let compact_status = compact.then(|| {
            div()
                .size(px(HARNESS_ICON_SIZE))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(status_glyph())
        });
        let corner_body: AnyElement = if hovered {
            h_flex()
                .gap(px(4.))
                .when(!compact, |this| this.mr(px(-4.)))
                .child(self.row_action(
                    &id,
                    RowAction::Pin {
                        pinned: session.pinned,
                    },
                    compact,
                    cx,
                ))
                .child(self.row_action(&id, RowAction::Archive { archived }, compact, cx))
                .into_any_element()
        } else {
            match status.label() {
                Some(label) => h_flex()
                    .gap(px(4.))
                    .child(status_glyph())
                    .child(
                        div()
                            .text_size(ui(10.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(status_color)
                            .child(label),
                    )
                    .into_any_element(),
                None => div()
                    .text_size(ui(10.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(time_ago.clone())
                    .into_any_element(),
            }
        };
        let mut corner = Some(
            div()
                .flex_none()
                .h(px(14.))
                .flex()
                .items_center()
                .text_color(subline)
                .child(corner_body),
        );
        let harness_icon = match session.kind {
            AgentKind::Claude => IconName::AgentClaude,
            AgentKind::Codex => IconName::AgentCodex,
        };
        let harness_tint: Option<Hsla> = match session.kind {
            AgentKind::Claude => Some(rgb(CLAUDE_BRAND).into()),
            AgentKind::Codex => None,
        };
        let title_content = match renaming {
            Some(field) => self.rename_field(&field.editor, cx),
            None => faded_label(
                SharedString::from(format!("agent-session-title-{id}")),
                true,
                div()
                    .text_size(ui(13.))
                    .line_height(px(17.))
                    .child(title.clone()),
            )
            .into_any_element(),
        };
        let hover_bg = if selected {
            selected_row(cx)
        } else {
            colors.element_hover
        };
        let rest_text = if selected {
            text
        } else if archived {
            text.opacity(0.55)
        } else {
            text.opacity(0.8)
        };
        let row = div()
            .id(SharedString::from(format!("agent-session-row-{id}")))
            .h(px(row_height))
            .flex()
            .flex_col()
            .gap(px(2.))
            .rounded(px(8.))
            .px(px(SPACE_SM))
            .py(px(6.))
            .text_color(rest_text)
            .when(selected, |this| this.bg(selected_row(cx)))
            .hover(move |style| style.bg(hover_bg).text_color(text))
            .cursor_pointer()
            .on_hover(cx.listener({
                let id = id.clone();
                move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        if this.hovered_row.as_ref() != Some(&id) {
                            this.hovered_row = Some(id.clone());
                            cx.notify();
                        }
                    } else if this.hovered_row.as_ref() == Some(&id) {
                        this.hovered_row = None;
                        cx.notify();
                    }
                }
            }))
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, window, cx| this.open_session(id.clone(), window, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.show_row_menu(&menu_session, event.position, window, cx)
                }),
            )
            .on_drag(
                DraggedChat {
                    id: id.clone(),
                    title,
                    project_root: session.project_root,
                    archived,
                },
                |dragged, _, _, cx| cx.new(|_| dragged.clone()),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap(px(if compact { 4. } else { SPACE_SM }))
                    .children(compact_status)
                    .child(glyph(
                        harness_icon,
                        HARNESS_ICON_SIZE,
                        harness_tint.unwrap_or(subline).opacity(if archived_muted {
                            0.4
                        } else {
                            0.8
                        }),
                    ))
                    .child(title_content)
                    .when(!compact || hovered, |this| this.children(corner.take()))
                    .when(compact, |this| {
                        this.child(
                            div()
                                .w(px(COMPACT_TIME_WIDTH))
                                .flex_none()
                                .whitespace_nowrap()
                                .text_right()
                                .text_size(ui(11.))
                                .text_color(subline)
                                .child(time_ago.clone()),
                        )
                    }),
            )
            .when_some(branch.filter(|_| !compact), |this, branch| {
                this.child(
                    h_flex()
                        .w_full()
                        .gap(px(4.))
                        .child(glyph(IconName::AgentGitBranch, 11., subline))
                        .child(faded_label(
                            SharedString::from(format!("agent-session-branch-{id}")),
                            false,
                            div()
                                .text_size(ui(11.))
                                .line_height(px(14.))
                                .text_color(subline)
                                .child(branch),
                        ))
                        .child(div().flex_1().min_w_0()),
                )
            });
        if nested {
            div()
                .pl(px(HARNESS_ICON_SIZE + SPACE_SM))
                .child(row)
                .into_any_element()
        } else {
            row.into_any_element()
        }
    }

    fn section_header(
        &self,
        key: SharedString,
        label: SharedString,
        custom_section: Option<String>,
        collapsed: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let faint = colors.text_muted.opacity(0.5);
        let renaming = self.renaming.as_ref().filter(|field| {
            matches!((&field.target, &custom_section), (Renaming::Section(id), Some(section)) if id == section)
        });
        let hover = colors.element_hover;
        let group = SharedString::from(format!("{key}-header"));
        let label_element = match renaming {
            Some(field) => self.rename_field(&field.editor, cx),
            None if custom_section.is_some() => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(ui(12.))
                .text_color(faint)
                .child(label.clone())
                .into_any_element(),
            None => faded_label(
                SharedString::from(format!("{key}-label")),
                false,
                div()
                    .text_size(ui(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(faint)
                    .child(label.clone()),
            )
            .into_any_element(),
        };
        let menu_button = custom_section.clone().map(|section| {
            let label = label.to_string();
            div()
                .id(SharedString::from(format!("{key}-menu")))
                .size(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .opacity(0.)
                .group_hover(group.clone(), |style| style.opacity(1.))
                .hover(move |style| style.bg(hover))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.show_section_menu(
                        section.clone(),
                        label.clone(),
                        event.position(),
                        window,
                        cx,
                    )
                }))
                .tooltip(Tooltip::text("Section Options"))
                .child(glyph(IconName::Ellipsis, 14., colors.text_muted))
        });
        h_flex()
            .id(key.clone())
            .group(group)
            .h(px(DISCLOSURE_HEADER_HEIGHT))
            .px(px(SPACE_SM))
            .gap(px(SPACE_SM))
            .cursor_pointer()
            .child(label_element)
            .when(custom_section.is_none(), |this| this.child(div().flex_1()))
            .children(menu_button)
            .child(disclosure_chevron(!collapsed, faint))
            .on_click(cx.listener({
                let key = key.to_string();
                move |this, _, _, cx| {
                    let key = key.clone();
                    if key == ARCHIVED_KEY {
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
                }
            }))
            .when_some(custom_section, |this, section| {
                let label = label.to_string();
                this.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.show_section_menu(
                            section.clone(),
                            label.clone(),
                            event.position,
                            window,
                            cx,
                        )
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
                            panel.start_rename(
                                Renaming::Section(section.clone()),
                                name.clone(),
                                window,
                                cx,
                            )
                        })
                        .log_err();
                }
            };
            let archive = {
                let (panel, section) = (panel.clone(), section.clone());
                move |_: &mut Window, cx: &mut App| {
                    panel
                        .update(cx, |panel, cx| {
                            panel
                                .store
                                .update(cx, |store, cx| store.archive_section(&section, cx))
                        })
                        .log_err();
                }
            };
            let delete = {
                let (panel, section) = (panel.clone(), section);
                move |_: &mut Window, cx: &mut App| {
                    panel
                        .update(cx, |panel, cx| {
                            panel
                                .store
                                .update(cx, |store, cx| store.delete_section(&section, cx))
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

    fn render_list(&self, cx: &Context<Self>) -> ChatList {
        let roots = chat_roots(&self.project, cx);
        let store = self.store.read(cx);
        let prefs = store.list_prefs().clone();
        let query = self.search.read(cx).text(cx).trim().to_lowercase();
        let searching = !query.is_empty();
        let sessions: Vec<SessionSummary> = store
            .sessions_in(&roots, cx)
            .into_iter()
            .filter(|session| Self::matches_search(session, &query))
            .collect();
        let layout = layout_list(&sessions, &prefs, searching, self.show_archived);
        let rendered_rows: HashSet<String> = layout
            .active
            .iter()
            .chain(&layout.archived)
            .flat_map(|group| group.rows.iter().map(|row| row.session.id.clone()))
            .collect();
        let row_context = RowContext {
            now: OffsetDateTime::now_utc().unix_timestamp(),
            compact: prefs.compact_rows,
            active_id: self.active_chat_id(cx),
        };
        let active = layout
            .active
            .into_iter()
            .enumerate()
            .map(|(index, group)| self.render_group(group, index > 0, &row_context, cx))
            .collect();
        let archived = layout
            .archived
            .map(|group| self.render_group(group, false, &row_context, cx));
        ChatList {
            active,
            archived,
            searching,
            rendered_rows,
        }
    }

    fn render_group(
        &self,
        group: ListGroup,
        follows_group: bool,
        row_context: &RowContext,
        cx: &Context<Self>,
    ) -> AnyElement {
        let collapsed = group.collapsed;
        let (key, label, top_gap): (SharedString, SharedString, bool) = match &group.kind {
            GroupKind::Pinned => (
                PINNED_KEY.into(),
                disclosure_label("Pinned", collapsed, group.total),
                false,
            ),
            GroupKind::Section { id, name } => (id.clone().into(), name.clone().into(), true),
            GroupKind::Project(project) => (
                project_key(project).into(),
                disclosure_label(&project_name(project), collapsed, group.total),
                true,
            ),
            GroupKind::Chats => (
                CHATS_KEY.into(),
                disclosure_label("Chats", collapsed, group.total),
                follows_group,
            ),
            GroupKind::Archived => (
                ARCHIVED_KEY.into(),
                disclosure_label("Archived", collapsed, group.total),
                false,
            ),
        };
        let custom_section = match &group.kind {
            GroupKind::Section { id, .. } => Some(id.clone()),
            _ => None,
        };
        let header = self.section_header(key.clone(), label, custom_section, collapsed, cx);
        let mut body: Vec<AnyElement> = group
            .rows
            .iter()
            .map(|row| {
                let selected = row_context.active_id.as_deref() == Some(row.session.id.as_str());
                self.render_session(
                    row.session.clone(),
                    row.nested,
                    row_context.now,
                    row_context.compact,
                    selected,
                    cx,
                )
            })
            .collect();
        let colors = cx.theme().colors();
        if body.is_empty() && !collapsed && matches!(group.kind, GroupKind::Section { .. }) {
            body.push(
                h_flex()
                    .h(px(40.))
                    .px(px(SPACE_SM))
                    .text_size(ui(12.))
                    .text_color(colors.text_muted.opacity(0.5))
                    .child("Drop chats here")
                    .into_any_element(),
            );
        }
        let highlight = colors.drop_target_background;
        let section = v_flex()
            .id(SharedString::from(format!("{key}-section")))
            .w_full()
            .rounded(px(8.))
            .child(header)
            .when(!body.is_empty(), |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .pt(px(DISCLOSURE_BODY_INSET))
                        .gap(px(LIST_GAP))
                        .children(body),
                )
            });
        let drop_into = |section: Stateful<Div>, target: Option<String>| {
            section
                .drag_over::<DraggedChat>(move |style, _, _, _| style.bg(highlight))
                .on_drop(cx.listener(move |this, dragged: &DraggedChat, _, cx| {
                    let target = target.clone();
                    this.store
                        .update(cx, |store, cx| store.move_to_section(&dragged.id, target, cx))
                }))
        };
        let section = match group.kind {
            GroupKind::Pinned | GroupKind::Archived => section,
            GroupKind::Section { id, .. } => drop_into(section, Some(id)),
            GroupKind::Chats => drop_into(section, None),
            GroupKind::Project(project) => drop_into(
                section.can_drop(move |dragged, _, _| {
                    dragged
                        .downcast_ref::<DraggedChat>()
                        .is_some_and(|dragged| dragged.can_drop_on_project(&project))
                }),
                None,
            ),
        };
        div()
            .w_full()
            .when(top_gap, |this| this.pt(px(SECTION_GAP)))
            .child(section)
            .into_any_element()
    }
}

struct ChatList {
    active: Vec<AnyElement>,
    archived: Option<AnyElement>,
    searching: bool,
    rendered_rows: HashSet<String>,
}

struct RowContext {
    now: i64,
    compact: bool,
    active_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum GroupKind {
    Pinned,
    Section { id: String, name: String },
    Project(PathBuf),
    Chats,
    Archived,
}

struct ListGroup<'a> {
    kind: GroupKind,
    total: usize,
    collapsed: bool,
    rows: Vec<ListRow<'a>>,
}

struct ListRow<'a> {
    session: &'a SessionSummary,
    nested: bool,
}

struct ListLayout<'a> {
    active: Vec<ListGroup<'a>>,
    archived: Option<ListGroup<'a>>,
}

fn needs_attention(session: &SessionSummary) -> bool {
    session.working || session.needs_input || session.unseen
}

fn layout_list<'a>(
    sessions: &'a [SessionSummary],
    prefs: &ChatListPrefs,
    searching: bool,
    show_archived: bool,
) -> ListLayout<'a> {
    let known_sections: HashSet<&str> = prefs
        .sections
        .iter()
        .map(|section| section.id.as_str())
        .collect();
    let is_collapsed = |key: &str| prefs.collapsed_sections.iter().any(|id| id == key);
    let children_of = |parent: &SessionSummary| -> Vec<&'a SessionSummary> {
        sessions
            .iter()
            .filter(|session| {
                session.side_chat
                    && session.archived == parent.archived
                    && session.parent_id.as_deref() == Some(parent.id.as_str())
            })
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
    let group = |kind: GroupKind, members: &[&'a SessionSummary], collapsed: bool| {
        let collapsed = collapsed && !searching;
        let mut rows = Vec::new();
        for &session in members {
            let mut children = children_of(session);
            if collapsed {
                children.retain(|child| needs_attention(child));
                if !needs_attention(session) && children.is_empty() {
                    continue;
                }
            }
            rows.push(ListRow {
                session,
                nested: false,
            });
            rows.extend(children.into_iter().map(|session| ListRow {
                session,
                nested: true,
            }));
        }
        ListGroup {
            kind,
            total: members.len(),
            collapsed,
            rows,
        }
    };
    let live: Vec<&SessionSummary> = sessions
        .iter()
        .filter(|session| !session.archived && is_top_level(session))
        .collect();
    let mut active = Vec::new();
    let pinned: Vec<&SessionSummary> = live
        .iter()
        .copied()
        .filter(|session| session.pinned)
        .collect();
    if !pinned.is_empty() {
        active.push(group(GroupKind::Pinned, &pinned, is_collapsed(PINNED_KEY)));
    }
    for section in &prefs.sections {
        let members: Vec<&SessionSummary> = live
            .iter()
            .copied()
            .filter(|session| {
                !session.pinned && session.section.as_deref() == Some(section.id.as_str())
            })
            .collect();
        if searching && members.is_empty() {
            continue;
        }
        active.push(group(
            GroupKind::Section {
                id: section.id.clone(),
                name: section.name.clone(),
            },
            &members,
            is_collapsed(&section.id),
        ));
    }
    let rest: Vec<&SessionSummary> = live
        .iter()
        .copied()
        .filter(|session| {
            !session.pinned
                && session
                    .section
                    .as_deref()
                    .is_none_or(|section| !known_sections.contains(section))
        })
        .collect();
    if prefs.group_by_project && !rest.is_empty() {
        let mut projects: Vec<&Path> = Vec::new();
        for session in &rest {
            if !projects.contains(&session.project_root.as_path()) {
                projects.push(&session.project_root);
            }
        }
        for project in projects {
            let members: Vec<&SessionSummary> = rest
                .iter()
                .copied()
                .filter(|session| session.project_root == project)
                .collect();
            active.push(group(
                GroupKind::Project(project.to_path_buf()),
                &members,
                is_collapsed(&project_key(project)),
            ));
        }
    } else if !rest.is_empty() || (!searching && !prefs.sections.is_empty()) {
        active.push(group(GroupKind::Chats, &rest, is_collapsed(CHATS_KEY)));
    }
    let archived: Vec<&SessionSummary> = sessions
        .iter()
        .filter(|session| session.archived && is_top_level(session))
        .collect();
    let archived = (!archived.is_empty()).then(|| {
        let mut archived = group(GroupKind::Archived, &archived, !show_archived);
        if archived.collapsed {
            archived.rows.clear();
        }
        archived
    });
    ListLayout { active, archived }
}

const PINNED_KEY: &str = "agent-pinned";
const CHATS_KEY: &str = "agent-chats";
const ARCHIVED_KEY: &str = "agent-archived";

fn project_key(project: &Path) -> String {
    format!("agent-project:{}", project.to_string_lossy())
}

fn disclosure_label(label: &str, collapsed: bool, count: usize) -> SharedString {
    if collapsed {
        format!("{label} ({count})").into()
    } else {
        SharedString::from(label.to_string())
    }
}

fn disclosure_chevron(open: bool, color: Hsla) -> impl IntoElement {
    div().flex_none().size(px(12.)).child(
        glyph(IconName::AgentArrowRight, 12., color).with_transformation(Transformation::rotate(
            percentage(if open { 0.25 } else { 0. }),
        )),
    )
}

fn glyph(icon: IconName, size: f32, color: Hsla) -> Svg {
    svg()
        .path(icon.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
}

fn row_height(compact: bool, shows_branch: bool) -> f32 {
    if shows_branch && !compact { 45. } else { 29. }
}

fn format_time_ago(then: i64, now: i64) -> String {
    let seconds = (now - then).max(0);
    if seconds < 60 {
        return "now".to_string();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    if days < 30 {
        return format!("{}w", days / 7);
    }
    if days < 365 {
        return format!("{}mo", days / 30);
    }
    format!("{}y", days / 365)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowStatus {
    Working,
    Input,
    Failed,
    Done,
    Idle,
}

impl RowStatus {
    fn of(session: &SessionSummary) -> Self {
        if session.needs_input {
            Self::Input
        } else if session.working {
            Self::Working
        } else {
            match (session.outcome, session.unseen) {
                (Some(ChatOutcome::Errored), _) => Self::Failed,
                (Some(ChatOutcome::Completed), true) => Self::Done,
                _ => Self::Idle,
            }
        }
    }

    fn label(self) -> Option<&'static str> {
        match self {
            Self::Working => Some("Working"),
            Self::Input => Some("Input"),
            Self::Failed => Some("Failed"),
            Self::Done => Some("Done"),
            Self::Idle => None,
        }
    }

    fn color(self, cx: &App) -> Hsla {
        let status = cx.theme().status();
        match self {
            Self::Working => accent(cx).opacity(0.55),
            Self::Input => accent(cx).opacity(0.6),
            Self::Failed => status.error.opacity(0.65),
            Self::Done => status.success.opacity(0.9),
            Self::Idle => ink(0.14, cx),
        }
    }
}

#[derive(Clone, Copy)]
enum RowAction {
    Pin { pinned: bool },
    Archive { archived: bool },
}

fn glyph_rows(cx: &App) -> [Hsla; 3] {
    let primary = accent(cx);
    let mut light = primary;
    let mut deep = primary;
    if cx.theme().appearance.is_light() {
        light.l = (light.l + 0.11).min(0.76);
        light.s *= 0.78;
        deep.l = (deep.l - 0.09).max(0.22);
    } else {
        light.l = (light.l + 0.14).min(0.90);
        light.s *= 0.72;
        deep.l = (deep.l - 0.08).max(0.);
    }
    [light, primary, deep]
}

fn spinner_opacity(phase: f32) -> f32 {
    let phase = phase.rem_euclid(1.0);
    if phase < 0.45 {
        1.0 + (SPINNER_DIM - 1.0) * (phase / 0.45)
    } else if phase < 0.92 {
        SPINNER_DIM
    } else {
        SPINNER_DIM + (1.0 - SPINNER_DIM) * ((phase - 0.92) / 0.08)
    }
}

fn mini_spinner(id: SharedString, cx: &App) -> impl IntoElement {
    let ring_length = (SPINNER_RING.len() * 2) as f32;
    v_flex().flex_none().gap(px(SPINNER_CELL / 2.)).children(
        SPINNER_RING
            .iter()
            .zip(glyph_rows(cx))
            .enumerate()
            .map(move |(row, (ring, tint))| {
                let id = id.clone();
                h_flex()
                    .gap(px(SPINNER_CELL / 2.))
                    .children(ring.iter().enumerate().map(move |(column, position)| {
                        let phase = position / ring_length;
                        div()
                            .size(px(SPINNER_CELL))
                            .rounded(px(SPINNER_CELL / 2.))
                            .bg(tint)
                            .with_animation(
                                ElementId::Name(format!("{id}-{row}-{column}").into()),
                                Animation::new(SPINNER_PERIOD).repeat(),
                                move |cell, delta| cell.opacity(spinner_opacity(delta + phase)),
                            )
                    }))
            }),
    )
}

thread_local! {
    static LIST_FADE: Cell<Option<EdgeFade>> = const { Cell::new(None) };
}

enum FadeTarget {
    Label,
    List,
}

struct EdgeFaded {
    child: AnyElement,
    scroll: ScrollHandle,
    target: FadeTarget,
}

fn faded_label(id: impl Into<ElementId>, fill: bool, label: impl IntoElement) -> EdgeFaded {
    let scroll = ScrollHandle::new();
    EdgeFaded {
        child: div()
            .id(id)
            .when(fill, |this| this.flex_1())
            .min_w_0()
            .overflow_hidden()
            .track_scroll(&scroll)
            .flex()
            .child(div().flex_none().whitespace_nowrap().child(label))
            .into_any_element(),
        scroll,
        target: FadeTarget::Label,
    }
}

fn faded_list(scroll: &ScrollHandle, list: impl IntoElement) -> EdgeFaded {
    EdgeFaded {
        child: list.into_any_element(),
        scroll: scroll.clone(),
        target: FadeTarget::List,
    }
}

impl EdgeFaded {
    fn fade(&self, bounds: Bounds<Pixels>) -> Option<EdgeFade> {
        let unfaded = EdgeFade {
            bounds,
            band: px(LABEL_FADE_BAND),
            band_top: None,
            band_bottom: None,
            band_left: None,
            band_right: None,
            top: false,
            bottom: false,
            left: false,
            right: false,
        };
        match self.target {
            FadeTarget::Label => {
                if self.scroll.max_offset().x <= px(0.5) {
                    return None;
                }
                Some(match LIST_FADE.with(Cell::get) {
                    Some(list) => EdgeFade {
                        bounds: Bounds::from_corners(
                            point(bounds.left(), list.bounds.top()),
                            point(bounds.right(), list.bounds.bottom()),
                        ),
                        band: list.band,
                        band_right: Some(px(LABEL_FADE_BAND)),
                        top: list.top,
                        bottom: list.bottom,
                        right: true,
                        ..unfaded
                    },
                    None => EdgeFade {
                        right: true,
                        ..unfaded
                    },
                })
            }
            FadeTarget::List => {
                let offset = self.scroll.offset().y;
                let top = offset < px(-0.5);
                let bottom = offset > -self.scroll.max_offset().y + px(0.5);
                (top || bottom).then_some(EdgeFade {
                    band: px(LIST_FADE_BAND),
                    top,
                    bottom,
                    ..unfaded
                })
            }
        }
    }
}

impl Element for EdgeFaded {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let fade = self.fade(bounds);
        match self.target {
            FadeTarget::Label => window.with_edge_fade(fade, |window| self.child.paint(window, cx)),
            FadeTarget::List => {
                let previous = LIST_FADE.with(|cell| cell.replace(fade));
                window.with_edge_fade(fade, |window| self.child.paint(window, cx));
                LIST_FADE.with(|cell| cell.set(previous));
            }
        }
    }
}

impl IntoElement for EdgeFaded {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[derive(IntoElement)]
struct HeaderButton {
    id: ElementId,
    icon: IconName,
    tooltip: SharedString,
    open: bool,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
}

impl HeaderButton {
    fn new(id: impl Into<ElementId>, icon: IconName, tooltip: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon,
            tooltip: tooltip.into(),
            open: false,
            on_click: None,
        }
    }
}

impl Clickable for HeaderButton {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    fn cursor_style(self, _: CursorStyle) -> Self {
        self
    }
}

impl Toggleable for HeaderButton {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }
}

impl RenderOnce for HeaderButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let hover = colors.element_hover;
        div()
            .id(self.id)
            .size(px(HEADER_CONTROL_SIZE))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .when(self.open, |this| this.bg(hover))
            .hover(move |style| style.bg(hover))
            .tooltip(Tooltip::text(self.tooltip))
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
            .child(glyph(self.icon, 16., colors.text_muted.opacity(0.6)))
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
        let list = self.render_list(cx);
        if self
            .hovered_row
            .as_ref()
            .is_some_and(|id| !list.rendered_rows.contains(id.as_ref()))
        {
            self.hovered_row = None;
        }
        let has_any = !self
            .store
            .read(cx)
            .sessions_in(&chat_roots(&self.project, cx), cx)
            .is_empty();
        let body: AnyElement = if has_any {
            let only_archived_matches = list.searching && list.archived.is_some();
            let active: Option<AnyElement> = if list.active.is_empty() {
                (!only_archived_matches).then(|| {
                    div()
                        .px(px(SPACE_SM))
                        .pb(px(SPACE_SM))
                        .text_size(ui(12.))
                        .text_color(text_faint(cx))
                        .child(if list.searching {
                            "No matching chats"
                        } else {
                            "No chats yet"
                        })
                        .into_any_element()
                })
            } else {
                Some(
                    v_flex()
                        .gap(px(LIST_GAP))
                        .pb(px(SPACE_SM))
                        .children(list.active)
                        .into_any_element(),
                )
            };
            faded_list(
                &self.list_scroll,
                v_flex()
                    .id("agent-sessions")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.list_scroll)
                    .px(px(SPACE_SM))
                    .pt(px(LIST_PAD_TOP))
                    .children(active)
                    .children(list.archived),
            )
            .into_any_element()
        } else {
            self.render_empty_state(cx).into_any_element()
        };
        v_flex()
            .id("agent-panel")
            .key_context("AgentPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(body))
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
        Some(IconName::AgentBot)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::ChatSection;

    #[test]
    fn time_ago_uses_compact_units() {
        assert_eq!(format_time_ago(1_000, 1_030), "now");
        assert_eq!(format_time_ago(0, 17 * 60), "17m");
        assert_eq!(format_time_ago(0, 5 * 3_600), "5h");
        assert_eq!(format_time_ago(0, 3 * 86_400), "3d");
        assert_eq!(format_time_ago(0, 14 * 86_400), "2w");
        assert_eq!(format_time_ago(0, 90 * 86_400), "3mo");
        assert_eq!(format_time_ago(0, 800 * 86_400), "2y");
        assert_eq!(format_time_ago(100, 0), "now");
    }

    #[test]
    fn time_ago_never_shows_zero_units() {
        const MINUTE: i64 = 60;
        const HOUR: i64 = 60 * MINUTE;
        const DAY: i64 = 24 * HOUR;
        let cases = [
            (59, "now"),
            (60, "1m"),
            (59 * MINUTE, "59m"),
            (60 * MINUTE, "1h"),
            (23 * HOUR, "23h"),
            (24 * HOUR, "1d"),
            (6 * DAY, "6d"),
            (7 * DAY, "1w"),
            (29 * DAY, "4w"),
            (30 * DAY, "1mo"),
            (359 * DAY, "11mo"),
            (360 * DAY, "12mo"),
            (364 * DAY, "12mo"),
            (365 * DAY, "1y"),
        ];
        for (elapsed, expected) in cases {
            assert_eq!(format_time_ago(0, elapsed), expected, "elapsed {elapsed}s");
        }
    }

    fn session(id: &str, project: &str) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            kind: AgentKind::Claude,
            title: SharedString::from(id.to_string()),
            created_at: 0,
            updated_at: 0,
            working: false,
            needs_input: false,
            pinned: false,
            archived: false,
            section: None,
            parent_id: None,
            side_chat: false,
            outcome: None,
            unseen: false,
            project_root: PathBuf::from(project),
            branch: None,
            native_session_id: None,
        }
    }

    fn row_ids(group: &ListGroup) -> Vec<String> {
        group
            .rows
            .iter()
            .map(|row| row.session.id.clone())
            .collect()
    }

    #[test]
    fn collapsed_group_keeps_rows_that_need_attention() {
        let working = SessionSummary {
            working: true,
            ..session("working", "/a")
        };
        let waiting = SessionSummary {
            needs_input: true,
            ..session("waiting", "/a")
        };
        let unseen = SessionSummary {
            unseen: true,
            ..session("unseen", "/a")
        };
        let sessions = vec![working, session("idle", "/a"), waiting, unseen];
        let prefs = ChatListPrefs {
            collapsed_sections: vec![CHATS_KEY.to_string()],
            ..ChatListPrefs::default()
        };

        let layout = layout_list(&sessions, &prefs, false, false);
        let [chats] = layout.active.as_slice() else {
            panic!("expected only the Chats group");
        };
        assert_eq!(chats.kind, GroupKind::Chats);
        assert!(chats.collapsed);
        assert_eq!(chats.total, 4);
        assert_eq!(row_ids(chats), ["working", "waiting", "unseen"]);

        let layout = layout_list(&sessions, &prefs, true, false);
        let [chats] = layout.active.as_slice() else {
            panic!("expected only the Chats group");
        };
        assert!(!chats.collapsed);
        assert_eq!(row_ids(chats), ["working", "idle", "waiting", "unseen"]);
    }

    #[test]
    fn search_expands_archived_group() {
        let archived = SessionSummary {
            archived: true,
            ..session("old", "/a")
        };
        let sessions = vec![archived];
        let prefs = ChatListPrefs::default();

        let hidden = layout_list(&sessions, &prefs, false, false);
        assert!(hidden.archived.as_ref().is_some_and(|group| group.rows.is_empty()));

        let searched = layout_list(&sessions, &prefs, true, false);
        let archived = searched.archived.as_ref().map(row_ids);
        assert_eq!(archived, Some(vec!["old".to_string()]));
    }

    #[test]
    fn search_hides_sections_without_matches() {
        let prefs = ChatListPrefs {
            sections: vec![ChatSection {
                id: "work".to_string(),
                name: "Work".to_string(),
            }],
            ..ChatListPrefs::default()
        };
        let sessions = vec![session("loose", "/a")];

        let browsing = layout_list(&sessions, &prefs, false, false);
        let kinds: Vec<&GroupKind> = browsing.active.iter().map(|group| &group.kind).collect();
        assert_eq!(
            kinds,
            [
                &GroupKind::Section {
                    id: "work".to_string(),
                    name: "Work".to_string(),
                },
                &GroupKind::Chats,
            ]
        );

        let matching = layout_list(&sessions, &prefs, true, false);
        let kinds: Vec<&GroupKind> = matching.active.iter().map(|group| &group.kind).collect();
        assert_eq!(kinds, [&GroupKind::Chats]);

        let no_match = layout_list(&[], &prefs, true, false);
        assert!(no_match.active.is_empty());
        assert!(no_match.archived.is_none());
    }

    #[test]
    fn project_drop_accepts_only_live_chats_from_that_project() {
        let dragged = |project: &str, archived: bool| DraggedChat {
            id: "chat".into(),
            title: "Chat".into(),
            project_root: PathBuf::from(project),
            archived,
        };
        assert!(dragged("/a", false).can_drop_on_project(Path::new("/a")));
        assert!(!dragged("/b", false).can_drop_on_project(Path::new("/a")));
        assert!(!dragged("/a", true).can_drop_on_project(Path::new("/a")));
    }

    #[test]
    fn row_height_tracks_visible_lines() {
        assert_eq!(row_height(true, true), 29.);
        assert_eq!(row_height(false, false), 29.);
        assert_eq!(row_height(false, true), 45.);
    }
}
