use super::{ChatView, ui};
use crate::{
    chat_style::{
        Chip, icon, menu_heading, menu_row, menu_section, popover_card, search_input_frame,
        text_faint,
    },
    session::AgentSession,
};
use editor::Editor;
use git::repository::{Branch, CreateWorktreeTarget, Worktree};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Task, Window, div, px,
};
use project::Project;
use std::path::PathBuf;
use ui::{IconName, PopoverMenu, prelude::*};
use settings::Settings as _;
use util::ResultExt as _;

const LIST_HEIGHT: f32 = 220.;

pub(super) struct CheckoutPicker {
    session: Entity<AgentSession>,
    project: Entity<Project>,
    search: Entity<Editor>,
    branches: Vec<Branch>,
    worktrees: Vec<Worktree>,
    creating: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    _load: Task<()>,
    _create: Task<()>,
    _subscriptions: Vec<Subscription>,
}

fn branch_label(worktree: &Worktree) -> SharedString {
    worktree
        .ref_name
        .as_ref()
        .map(|name| SharedString::from(name.trim_start_matches("refs/heads/").to_string()))
        .unwrap_or_else(|| SharedString::from(worktree.sha.chars().take(8).collect::<String>()))
}

fn slug() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("chat-{}", &id[..8])
}

impl CheckoutPicker {
    fn new(
        session: Entity<AgentSession>,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Base branch for a new worktree…", window, cx);
            editor
        });
        let repository = project.read(cx).active_repository(cx);
        let load = cx.spawn(async move |this, cx| {
            let Some(repository) = repository else {
                return;
            };
            let (branches, worktrees) = repository.update(cx, |repository, _| {
                (repository.branches(), repository.worktrees())
            });
            let branches = branches.await.ok().and_then(|result| result.log_err());
            let worktrees = worktrees.await.ok().and_then(|result| result.log_err());
            this.update(cx, |this, cx| {
                if let Some(scan) = branches {
                    this.branches = scan.branches.into_iter().filter(|branch| !branch.is_remote()).collect();
                }
                if let Some(worktrees) = worktrees {
                    this.worktrees = worktrees.into_iter().filter(|worktree| !worktree.is_bare).collect();
                }
                cx.notify();
            })
            .log_err();
        });
        Self {
            _subscriptions: vec![cx.observe(&search, |_, _, cx| cx.notify())],
            session,
            project,
            search,
            branches: Vec::new(),
            worktrees: Vec::new(),
            creating: false,
            error: None,
            focus_handle: cx.focus_handle(),
            _load: load,
            _create: Task::ready(()),
        }
    }

    fn use_checkout(&mut self, path: PathBuf, branch: Option<String>, cx: &mut Context<Self>) {
        let root = self
            .project
            .read(cx)
            .active_repository(cx)
            .map(|repository| repository.read(cx).work_directory_abs_path.to_path_buf());
        self.session.update(cx, |session, cx| {
            session.update_metadata(
                |metadata| {
                    let project_root = metadata.project_root().to_path_buf();
                    metadata.project_root = root
                        .clone()
                        .filter(|root| *root != path)
                        .or_else(|| (project_root != path).then_some(project_root));
                    metadata.cwd = path;
                    metadata.branch = branch;
                },
                cx,
            )
        });
        cx.emit(DismissEvent);
    }

    fn create_worktree(&mut self, base: String, cx: &mut Context<Self>) {
        let Some(repository) = self.project.read(cx).active_repository(cx) else {
            return;
        };
        let slug = slug();
        let branch_name = format!("wu/{slug}");
        let setting = project::project_settings::ProjectSettings::get_global(cx)
            .git
            .worktree_directory
            .clone();
        let path = match repository
            .read(cx)
            .path_for_new_linked_worktree(&slug, &setting)
        {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let receiver = repository.update(cx, |repository, _| {
            repository.create_worktree(
                CreateWorktreeTarget::NewBranch {
                    branch_name: branch_name.clone(),
                    base_sha: Some(base),
                },
                path.clone(),
            )
        });
        self.creating = true;
        self.error = None;
        cx.notify();
        self._create = cx.spawn(async move |this, cx| {
            let result = match receiver.await {
                Ok(result) => result,
                Err(error) => Err(error.into()),
            };
            this.update(cx, |this, cx| {
                this.creating = false;
                match result {
                    Ok(()) => this.use_checkout(path, Some(branch_name), cx),
                    Err(error) => {
                        this.error = Some(error.to_string().into());
                        cx.notify();
                    }
                }
            })
            .log_err();
        });
    }
}

impl EventEmitter<DismissEvent> for CheckoutPicker {}

impl Focusable for CheckoutPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CheckoutPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let metadata = self.session.read(cx).metadata().clone();
        let query = self.search.read(cx).text(cx).trim().to_lowercase();
        let row = |id: SharedString,
                   icon_name: IconName,
                   label: SharedString,
                   detail: Option<SharedString>,
                   selected: bool| {
            menu_row(selected, cx)
                .id(id)
                .child(icon(icon_name, px(14.), colors.text_muted))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .when_some(detail, |this, detail| {
                    this.child(
                        div()
                            .flex_none()
                            .text_size(ui(10.))
                            .text_color(colors.text_muted)
                            .child(detail),
                    )
                })
        };
        let main_root = self
            .project
            .read(cx)
            .active_repository(cx)
            .map(|repository| repository.read(cx).work_directory_abs_path.to_path_buf());
        let on_main = main_root.as_ref().is_none_or(|root| *root == metadata.cwd);
        let worktrees: Vec<Worktree> = self
            .worktrees
            .iter()
            .filter(|worktree| !worktree.is_main)
            .cloned()
            .collect();
        let branches: Vec<Branch> = self
            .branches
            .iter()
            .filter(|branch| query.is_empty() || branch.name().to_lowercase().contains(&query))
            .cloned()
            .collect();
        popover_card(cx)
            .key_context("menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .w(px(300.))
            .child(menu_heading("Checkout", cx))
            .child(
                row(
                    "checkout-current".into(),
                    IconName::AgentFolder,
                    "Current checkout".into(),
                    None,
                    on_main,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(root) = main_root.clone() {
                        let branch = this
                            .project
                            .read(cx)
                            .active_repository(cx)
                            .and_then(|repository| {
                                repository
                                    .read(cx)
                                    .branch
                                    .as_ref()
                                    .map(|branch| branch.name().to_string())
                            });
                        this.use_checkout(root, branch, cx);
                    }
                })),
            )
            .children(worktrees.into_iter().map(|worktree| {
                let label = branch_label(&worktree);
                let selected = worktree.path == metadata.cwd;
                let path = worktree.path.clone();
                let branch = label.to_string();
                row(
                    SharedString::from(format!("checkout-worktree-{}", worktree.path.display())),
                    IconName::AgentFolderWithFiles,
                    format!("Worktree · {label}").into(),
                    worktree
                        .path
                        .file_name()
                        .map(|name| SharedString::from(name.to_string_lossy().into_owned())),
                    selected,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.use_checkout(path.clone(), Some(branch.clone()), cx)
                }))
            }))
            .child(
                menu_section(cx)
                    .child(menu_heading("New worktree from", cx))
                    .child(search_input_frame(cx).child(self.search.clone()))
                    .child(
                        v_flex()
                            .id("checkout-branches")
                            .max_h(px(LIST_HEIGHT))
                            .overflow_y_scroll()
                            .gap(px(2.))
                            .children(branches.into_iter().map(|branch| {
                                let name = branch.name().to_string();
                                row(
                                    SharedString::from(format!("checkout-branch-{name}")),
                                    IconName::AgentGitBranch,
                                    name.clone().into(),
                                    None,
                                    false,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.creating {
                                        this.create_worktree(name.clone(), cx)
                                    }
                                }))
                            })),
                    ),
            )
            .when(self.creating, |this| {
                this.child(
                    div()
                        .px(px(8.))
                        .py(px(4.))
                        .text_size(ui(11.))
                        .text_color(text_faint(cx))
                        .child("Creating worktree…"),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px(px(8.))
                        .py(px(4.))
                        .text_size(ui(11.))
                        .text_color(cx.theme().status().error.opacity(0.9))
                        .child(error),
                )
            })
    }
}

impl ChatView {
    pub(super) fn render_checkout_picker(&self, cx: &Context<Self>) -> AnyElement {
        let metadata = self.session.read(cx).metadata().clone();
        let colors = cx.theme().colors();
        let in_worktree = metadata.project_root.is_some();
        let branch: SharedString = metadata
            .branch
            .map(SharedString::from)
            .or_else(|| self.branch_name(cx))
            .unwrap_or_else(|| "No branch".into());
        let label: SharedString = if in_worktree {
            "Worktree".into()
        } else {
            "Local checkout".into()
        };
        let label_color = colors.text_muted.opacity(0.7);
        let hover_text = colors.text.opacity(0.8);
        let hover = colors.element_hover;
        let footer_chip = |id: &'static str, icon_name: IconName, label: SharedString| {
            h_flex()
                .id(id)
                .h(px(20.))
                .max_w(px(280.))
                .min_w_0()
                .px(px(8.))
                .gap(px(6.))
                .rounded(px(6.))
                .text_size(ui(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(label_color)
                .hover(move |style| style.bg(hover).text_color(hover_text))
                .child(icon(icon_name, px(12.), label_color))
                .child(div().min_w_0().truncate().child(label))
                .child(icon(
                    IconName::AgentArrowDown,
                    px(12.),
                    colors.text_muted.opacity(0.5),
                ))
        };
        let session = self.session.clone();
        let project = self.project.clone();
        PopoverMenu::new("agent-checkout-picker")
            .trigger(
                Chip::new(
                    "agent-checkout-trigger",
                    6.,
                    h_flex()
                        .gap(px(4.))
                        .child(footer_chip(
                            "agent-checkout-kind",
                            if in_worktree {
                                IconName::AgentFolderWithFiles
                            } else {
                                IconName::AgentFolder
                            },
                            label,
                        ))
                        .child(footer_chip(
                            "agent-checkout-branch",
                            IconName::AgentGitBranch,
                            branch,
                        )),
                )
                .hover_background(gpui::transparent_black()),
            )
            .menu(move |window, cx| {
                let session = session.clone();
                let project = project.clone();
                Some(cx.new(|cx| CheckoutPicker::new(session, project, window, cx)))
            })
            .anchor(gpui::Anchor::TopLeft)
            .into_any_element()
    }
}
