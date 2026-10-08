mod attachments;
mod background_tasks;
mod checkout;
mod completion;
mod composer;
mod dictation;
mod diff_view;
mod image_viewer;
mod links;
mod mermaid;
mod outline;
mod queue_panel;
mod search;
mod todo_panel;

use crate::{
    AgentChatSettings, Send, Stop,
    chat_style::{
        Chip, accent, flavour_seed, flavour_word, format_elapsed, gradient_spinner, hairline, ink,
        mix, page, text_faint, ui, wash,
    },
    model_picker::{ModelPicker, Selection, agent_icon},
    session::{AgentSession, AgentStore, Entry, PendingQuestion, Prompt, ToolEntry, ToolStatus},
    slash_commands::{CommandToken, app_command_for_text},
    usage_rings::{ContextCard, UsageCard, context_chip, usage_chip},
};
use agent_harness::{ToolCall, UserInputAnswer, view};
use settings::{AgentChatSendKey, Settings as _};
use gpui::Font;
use theme_settings::ThemeSettings;
use collections::{HashMap, HashSet};
use editor::{Editor, EditorEvent};
use gpui::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, Bounds, ClipboardItem, ContentMask,
    Context, Entity, EventEmitter, FillOptions, FillRule, FocusHandle, Focusable, FontWeight,
    Hsla, IntoElement, ParentElement, KeyContext, PathBuilder, PathStyle, Pixels, Point, Render,
    ScrollHandle, SharedString, StyleRefinement, Styled, ObjectFit, Subscription, Svg,
    ease_out_quint, Task, TextAlign, TextRun, TextStyleRefinement, Transformation,
    UnderlineStyle, WeakEntity, Window, canvas, div, img, point, px, radians, size, svg,
};
use markdown::{
    CodeBlockRenderer, HeadingLevelStyles, Markdown, MarkdownElement, MarkdownFont, MarkdownStyle,
    parser::CodeBlockKind,
};
use project::Project;
use gpui::{FollowMode, ListAlignment, ListState, list};
#[cfg(test)]
use gpui::ListOffset;
use std::{
    cell::Cell,
    rc::Rc,
    {sync::Arc, time::Duration},
};
use theme::ActiveTheme as _;
use util::ResultExt as _;
use ui::{
    CommonAnimationExt as _, Icon, IconName, IconSize, Label, PopoverMenu, PopoverMenuHandle,
    Tooltip, prelude::*,
};
use std::time::Instant;
use gpui::ExternalPaths;
use workspace::{
    DraggedSelection, Workspace,
    item::{Item, ItemEvent, TabContentParams},
};

const CONTENT_WIDTH: f32 = 736.;
const COMPOSER_WIDTH: f32 = CONTENT_WIDTH + 32.;
const SIDE_GUTTER: f32 = 48.;
const LIST_OVERDRAW: f32 = 1200.;
const SCROLL_BUTTON_THRESHOLD: f32 = 320.;
const AT_BOTTOM: f32 = 2.;
const COMPOSER_RADIUS: f32 = 26.;
const DOCKED_COMPOSER_RADIUS: f32 = 22.;
const SEND_BUTTON_SIZE: f32 = 28.;
const USER_COLLAPSED_LINES: usize = 5;
const USER_COLLAPSE_CHARS: usize = 400;
const USER_LINE_HEIGHT: f32 = 22.;
const USER_TOGGLE_GAP: f32 = 8.;
const ATTACHMENT_THUMB_WIDTH: f32 = 112.;
const ATTACHMENT_THUMB_HEIGHT: f32 = 80.;
const GENERATED_IMAGE_MAX_WIDTH: f32 = 512.;
const GENERATED_IMAGE_MAX_HEIGHT: f32 = 420.;
const SPACE_SM: f32 = 8.;
const SPACE_MD: f32 = 12.;
const SPACE_LG: f32 = 16.;
const FIRST_ROW_BREATHING_ROOM: f32 = 10.;
const TRANSCRIPT_BOTTOM_PAD: f32 = 32.;
const MD_BLOCK_GAP: f32 = 12.;
const TREE_ROW_HEIGHT: f32 = 32.;
const TREE_GUTTER: f32 = 48.;
const TREE_TRUNK_X: f32 = 12.5;
const TREE_BEND_RADIUS: f32 = 6.;
const TREE_BRANCH_END_X: f32 = 28.;
const TREE_ICON_LEFT: f32 = 32.;
const TREE_ICON_SIZE: f32 = 16.;
const TREE_TEXT_GAP: f32 = 8.;
const CHIP_HEIGHT: f32 = 38.;
const CHIP_CARD_HEIGHT: f32 = 30.;
const CHIP_HEADER_HEIGHT: f32 = CHIP_CARD_HEIGHT - 2.;
const CHIPS_TOP_PAD: f32 = 2.;
const TOOL_GROUP_HEADER_HEIGHT: f32 = 26.;
const TOOL_TEXT_SIZE: f32 = 12.;
const THOUGHT_BODY_SIZE: f32 = 15.;
const THOUGHT_CODE_SIZE: f32 = 14.;
const TOOL_LINE_HEIGHT: f32 = 18.;
const TOOL_SHIMMER_HALF_WIDTH: f32 = 0.36;
const TOOL_SHIMMER_STRIP_WIDTH: f32 = 2.;
const DETAIL_SEPARATOR: f32 = 1.;
const BLOB_AFFORDANCE_HEIGHT: f32 = 24.;
const CALL_WRAP_COLUMNS: usize = 80;
const OUTPUT_MAX_LINES: usize = 24;
const OUTPUT_LINE_HEIGHT: f32 = 18.;
const CODE_HEADER_HEIGHT: f32 = 28.;
const CODE_ACTION_SIZE: f32 = 22.;
const DIAGRAM_MAX_HEIGHT: f32 = 480.;
const SUBAGENT_SPINNER_CELL: f32 = 2.;
const SUBAGENT_SPINNER_DIM: f32 = 0.1;
const SUBAGENT_SPINNER_PERIOD: Duration = Duration::from_millis(750);
const COPIED_FEEDBACK: Duration = Duration::from_millis(1200);
const AUTO_ADVANCE: Duration = Duration::from_millis(220);
const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const GROUP_SHIMMER: Duration = Duration::from_millis(3400);
const FADE_IN: Duration = Duration::from_millis(260);
const GLIDE: Duration = Duration::from_millis(280);
const GLIDE_FRAME: Duration = Duration::from_millis(16);

#[derive(Clone)]
struct SubagentRef {
    id: String,
    title: SharedString,
}

pub struct ChatView {
    session: Entity<AgentSession>,
    subagent: Option<SubagentRef>,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    store: Entity<AgentStore>,
    project: Entity<Project>,
    composer: Entity<Editor>,
    list: ListState,
    turns: Vec<Turn>,
    show_scroll_button: bool,
    expanded_rows: HashSet<String>,
    group_overrides: HashMap<String, bool>,
    expanded_users: HashSet<usize>,
    user_markdown: HashMap<usize, Entity<Markdown>>,
    selected_options: HashMap<String, Vec<String>>,
    question_pages: HashMap<String, usize>,
    answer_editors: HashMap<String, (Entity<Editor>, Subscription)>,
    command_menu: Option<completion::CompletionMenu>,
    mention_results: Vec<completion::MentionItem>,
    _mention_search: Task<()>,
    composer_skills: Vec<agent_harness::SkillRef>,
    attachments: Vec<std::path::PathBuf>,
    lightbox: Option<image_viewer::Lightbox>,
    diff_cache: std::cell::RefCell<HashMap<String, Arc<diff_view::FileDiffView>>>,
    diff_tasks: std::cell::RefCell<Vec<Task<()>>>,
    full_output: HashSet<String>,
    dictation: dictation::DictationState,
    dictation_model_ready: std::cell::Cell<Option<bool>>,
    _dictation_levels: Task<()>,
    _dictation_events: Task<()>,
    _dictation_download: Task<()>,
    _glide: Task<()>,
    container_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    painted_turns: Rc<std::cell::RefCell<HashMap<usize, Bounds<Pixels>>>>,
    this: WeakEntity<ChatView>,
    background_ticking: bool,
    _background_tick: Task<()>,
    search: search::SearchState,
    outline_hover: Option<usize>,
    expanded_work: HashSet<usize>,
    todo_panel: todo_panel::TodoPanelState,
    known_paths: HashMap<std::path::PathBuf, links::PathState>,
    scanned_sources: HashMap<gpui::EntityId, usize>,
    link_menu: Option<links::LinkMenu>,
    diagrams: std::cell::RefCell<HashMap<(String, bool), mermaid::Diagram>>,
    diagram_tasks: HashMap<(String, bool), Task<()>>,
    diagram_scan: (Option<bool>, HashMap<gpui::EntityId, usize>),
    diagram_sources_shown: HashSet<String>,
    editing_queued: Option<queue_panel::QueuedEdit>,
    composer_layout: composer::ComposerLayout,
    lightbox_focus: FocusHandle,
    attachment_error: Option<SharedString>,
    staging_count: usize,
    dismissed_command: Option<CommandToken>,
    command_scroll: ScrollHandle,
    model_picker: PopoverMenuHandle<ModelPicker>,
    copied: Option<SharedString>,
    flavour_seed: u64,
    _copied_reset: Task<()>,
    _auto_advance: Task<()>,
    _poll_usage: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ChatView {
    pub fn new(
        session: Entity<AgentSession>,
        store: Entity<AgentStore>,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(session, None, store, project, workspace, window, cx)
    }

    fn build(
        session: Entity<AgentSession>,
        subagent: Option<SubagentRef>,
        store: Entity<AgentStore>,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let kind = session.read(cx).kind();
        let flavour = flavour_seed(&session.read(cx).metadata().id);
        let composer = cx.new(|cx| {
            let mut editor = Editor::auto_height(1, 10, window, cx);
            editor.set_placeholder_text("Do anything…", window, cx);
            editor.set_text_style_refinement(TextStyleRefinement {
                font_size: Some(ui(14.).into()),
                line_height: Some(ui(22.75).into()),
                ..Default::default()
            });
            editor
        });
        store.update(cx, |store, cx| {
            store.ensure_models(kind, cx);
            store.refresh_plan_usage(kind, false, cx);
        });
        let markdown = store.read(cx).languages().language_for_name("Markdown");
        cx.spawn({
            let composer = composer.clone();
            async move |_, cx| {
                let Some(markdown) = markdown.await.log_err() else {
                    return;
                };
                composer.update(cx, |editor, cx| {
                    if let Some(buffer) = editor.buffer().read(cx).as_singleton() {
                        buffer.update(cx, |buffer, cx| buffer.set_language(Some(markdown), cx));
                    }
                });
            }
        })
        .detach();
        let poll_usage = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(USAGE_POLL_INTERVAL).await;
                let refreshed = this.update(cx, |this, cx| {
                    this.store
                        .update(cx, |store, cx| store.refresh_plan_usage(kind, false, cx))
                });
                if refreshed.is_err() {
                    return;
                }
            }
        });
        let list = ListState::new(0, ListAlignment::Top, px(LIST_OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        let subscriptions = vec![
            cx.observe_in(&session, window, |this, session, window, cx| {
                if session.read(cx).is_deleted() {
                    cx.emit(ItemEvent::CloseItem);
                    return;
                }
                this.sync_turns(cx);
                if !this.search.is_empty() {
                    cx.emit(workspace::searchable::SearchEvent::MatchesInvalidated);
                }
                if this.subagent.is_none() {
                    this.sync_answer_editors(window, cx);
                }
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }),
            cx.subscribe(&composer, |this, _, event: &EditorEvent, cx| match event {
                EditorEvent::BufferEdited => {
                    this.update_command_menu(cx);
                    cx.notify();
                }
                EditorEvent::SelectionsChanged { .. } => this.update_command_menu(cx),
                _ => {}
            }),
            cx.on_release(|this, cx| this.clear_search_highlights(cx)),
            cx.observe_window_activation(window, |_, _, cx| cx.notify()),
            cx.observe(&store, |this, _, cx| {
                if this.subagent.is_none() {
                    this.pin_default_settings(cx);
                }
                cx.notify();
            }),
        ];
        let mut view = Self {
            session,
            subagent,
            workspace,
            focus_handle: cx.focus_handle(),
            store,
            project,
            composer,
            list,
            turns: Vec::new(),
            show_scroll_button: false,
            expanded_rows: HashSet::default(),
            group_overrides: HashMap::default(),
            expanded_users: HashSet::default(),
            user_markdown: HashMap::default(),
            selected_options: HashMap::default(),
            question_pages: HashMap::default(),
            answer_editors: HashMap::default(),
            command_menu: None,
            mention_results: Vec::new(),
            _mention_search: Task::ready(()),
            composer_skills: Vec::new(),
            attachments: Vec::new(),
            lightbox: None,
            diff_cache: Default::default(),
            diff_tasks: Default::default(),
            full_output: HashSet::default(),
            dictation: dictation::DictationState::Idle,
            dictation_model_ready: Default::default(),
            _dictation_levels: Task::ready(()),
            _dictation_events: Task::ready(()),
            _dictation_download: Task::ready(()),
            _glide: Task::ready(()),
            container_bounds: Default::default(),
            painted_turns: Default::default(),
            this: cx.entity().downgrade(),
            background_ticking: false,
            _background_tick: Task::ready(()),
            search: Default::default(),
            outline_hover: None,
            expanded_work: HashSet::default(),
            todo_panel: Default::default(),
            known_paths: HashMap::default(),
            scanned_sources: HashMap::default(),
            link_menu: None,
            diagrams: Default::default(),
            diagram_tasks: HashMap::default(),
            diagram_scan: Default::default(),
            diagram_sources_shown: HashSet::default(),
            editing_queued: None,
            composer_layout: Default::default(),
            lightbox_focus: cx.focus_handle(),
            attachment_error: None,
            staging_count: 0,
            dismissed_command: None,
            command_scroll: ScrollHandle::new(),
            model_picker: PopoverMenuHandle::default(),
            copied: None,
            flavour_seed: flavour,
            _copied_reset: Task::ready(()),
            _auto_advance: Task::ready(()),
            _poll_usage: poll_usage,
            _subscriptions: subscriptions,
        };
        view.pin_default_settings(cx);
        view.sync_answer_editors(window, cx);
        view.sync_turns(cx);
        view
    }

    fn sync_answer_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let free_text: Vec<String> = self
            .session
            .read(cx)
            .pending_questions()
            .iter()
            .filter(|pending| !pending.is_permission())
            .flat_map(|pending| &pending.questions)
            .filter(|question| question.options.is_empty())
            .map(|question| question.id.clone())
            .collect();
        self.answer_editors
            .retain(|question_id, _| free_text.contains(question_id));
        for question_id in free_text {
            if self.answer_editors.contains_key(&question_id) {
                continue;
            }
            let editor = cx.new(|cx| {
                let mut editor = Editor::auto_height(1, 6, window, cx);
                editor.set_placeholder_text("Type your answer…", window, cx);
                editor.set_text_style_refinement(TextStyleRefinement {
                    font_size: Some(ui(14.).into()),
                    line_height: Some(ui(22.75).into()),
                    ..Default::default()
                });
                editor
            });
            let subscription = cx.subscribe(&editor, |_, _, event: &EditorEvent, cx| {
                if matches!(event, EditorEvent::BufferEdited) {
                    cx.notify();
                }
            });
            self.answer_editors
                .insert(question_id, (editor, subscription));
        }
    }

    fn answer_labels(&self, question_id: &str, cx: &App) -> Vec<String> {
        if let Some((editor, _)) = self.answer_editors.get(question_id) {
            let text = editor.read(cx).text(cx).trim().to_string();
            return if text.is_empty() { Vec::new() } else { vec![text] };
        }
        self.selected_options
            .get(question_id)
            .cloned()
            .unwrap_or_default()
    }

    fn pin_default_settings(&mut self, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let Some(models) = self.store.read(cx).models_discovered(session.kind()) else {
            return;
        };
        if session.settings().model.is_some() {
            if let Some(reasoning) = crate::session::reasoning_for_model(session.settings(), models)
            {
                self.session.update(cx, |session, cx| {
                    session.pin_settings(|settings| settings.reasoning = reasoning, cx)
                });
            }
            return;
        }
        let Some(model) = view::default_model(models).cloned() else {
            return;
        };
        self.session.update(cx, |session, cx| {
            session.pin_settings(
                |settings| {
                    settings.reasoning =
                        view::clamp_reasoning(settings.reasoning, &model.reasoning_levels);
                    settings.model = Some(model.id.clone());
                },
                cx,
            )
        });
    }

    pub fn session(&self) -> &Entity<AgentSession> {
        &self.session
    }

    pub fn is_subagent(&self) -> bool {
        self.subagent.is_some()
    }

    fn entries<'a>(&self, cx: &'a App) -> &'a [Entry] {
        let session = self.session.read(cx);
        match &self.subagent {
            Some(subagent) => session
                .subagent(&subagent.id)
                .map_or(&[], |subagent| subagent.entries()),
            None => session.entries(),
        }
    }

    fn is_working(&self, cx: &App) -> bool {
        let session = self.session.read(cx);
        match &self.subagent {
            Some(subagent) => session.subagent_running(&subagent.id),
            None => session.is_working(),
        }
    }

    fn working_since(&self, cx: &App) -> Option<Instant> {
        let session = self.session.read(cx);
        match &self.subagent {
            Some(subagent) => session
                .subagent_running(&subagent.id)
                .then(|| session.subagent(&subagent.id).map(|subagent| subagent.started_at()))
                .flatten(),
            None => session.working_since(),
        }
    }

    fn open_subagent(
        &mut self,
        id: String,
        call: &ToolCall,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subagent = SubagentRef {
            id,
            title: subagent_title(call),
        };
        let session = self.session.clone();
        let store = self.store.clone();
        let project = self.project.clone();
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    let existing = workspace.items_of_type::<ChatView>(cx).find(|view| {
                        let view = view.read(cx);
                        view.session == session
                            && view.subagent.as_ref().map(|open| &open.id) == Some(&subagent.id)
                    });
                    if let Some(view) = existing {
                        workspace.activate_item(&view, true, true, window, cx);
                        return;
                    }
                    let weak_workspace = cx.entity().downgrade();
                    let view = cx.new(|cx| {
                        ChatView::build(
                            session,
                            Some(subagent),
                            store,
                            project,
                            weak_workspace,
                            window,
                            cx,
                        )
                    });
                    workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
                })
                .log_err();
        });
    }

    fn title(&self, cx: &App) -> SharedString {
        if let Some(subagent) = &self.subagent {
            return subagent.title.clone();
        }
        let title = &self.session.read(cx).metadata().title;
        if title.is_empty() {
            "New chat".into()
        } else {
            title.clone().into()
        }
    }

    fn send(&mut self, _: &Send, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text(cx);
        if self.session.read(cx).is_deleted() || self.staging_count > 0 {
            return;
        }
        if text.trim().is_empty() && self.attachments.is_empty() {
            if self.editing_queued.is_some() {
                self.cancel_queued_edit(window, cx);
            } else {
                self.steer_newest_queued(cx);
            }
            return;
        }
        self.composer
            .update(cx, |editor, cx| editor.clear(window, cx));
        if self.attachments.is_empty()
            && self.editing_queued.is_none()
            && let Some(command) = app_command_for_text(&text, &self.command_items(cx))
        {
            self.run_app_command(command, window, cx);
            return;
        }
        if self.attachments.is_empty()
            && self.editing_queued.is_none()
            && self.redirect_hidden_command(&text, window, cx)
        {
            return;
        }
        let skills = std::mem::take(&mut self.composer_skills)
            .into_iter()
            .filter(|skill| text.contains(&format!("${}", skill.name)))
            .collect();
        let prompt = Prompt {
            text,
            attachments: std::mem::take(&mut self.attachments),
            skills,
        };
        self.attachment_error = None;
        if self.editing_queued.is_some() {
            self.save_queued_edit(prompt, window, cx);
            return;
        }
        self.session
            .update(cx, |session, cx| session.send_message(prompt, cx));
        self.glide_to_bottom(cx);
    }

    fn glide_to_bottom(&mut self, cx: &mut Context<Self>) {
        let start = self.scroll_top();
        self._glide = cx.spawn(async move |this, cx| {
            let frames = (GLIDE.as_millis() / GLIDE_FRAME.as_millis()).max(1) as u32;
            for frame in 1..=frames {
                cx.background_executor().timer(GLIDE_FRAME).await;
                let progress = ease_out_quint()(frame as f32 / frames as f32);
                let still_open = this.update(cx, |this, cx| {
                    let target = this.list.max_offset_for_scrollbar().y;
                    let y = start + (target - start) * progress;
                    this.list.set_offset_from_scrollbar(point(px(0.), -y));
                    cx.notify();
                });
                if still_open.is_err() {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.list.set_follow_mode(FollowMode::Tail);
                cx.notify();
            })
            .ok();
        });
    }

    fn steer_newest_queued(&mut self, cx: &mut Context<Self>) {
        let newest = self.session.read(cx).queue().last().map(|queued| queued.id);
        if let Some(id) = newest {
            self.session
                .update(cx, |session, cx| session.steer_queued(id, cx));
        }
    }

    fn stop(&mut self, _: &Stop, _: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| session.stop(cx));
    }

    // A tail-following list reports its anchor past the end, so clamp to the real maximum.
    fn scroll_top(&self) -> Pixels {
        (-self.list.scroll_px_offset_for_scrollbar().y).min(self.list.max_offset_for_scrollbar().y)
    }

    fn update_scroll_state(&mut self) {
        let distance = if self.list.is_following_tail() {
            0.
        } else {
            f32::from(self.list.max_offset_for_scrollbar().y - self.scroll_top()).max(0.)
        };
        if distance > SCROLL_BUTTON_THRESHOLD {
            self.show_scroll_button = true;
        } else if distance <= AT_BOTTOM {
            self.show_scroll_button = false;
        }
    }

    fn copy(&mut self, key: SharedString, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(key.clone());
        self._copied_reset = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FEEDBACK).await;
            this.update(cx, |this, cx| {
                if this.copied.as_ref() == Some(&key) {
                    this.copied = None;
                    cx.notify();
                }
            })
            .ok();
        });
        cx.notify();
    }

    fn message_style(window: &Window, cx: &App) -> MarkdownStyle {
        let colors = cx.theme().colors();
        let accent = accent(cx);
        let mut style = MarkdownStyle::themed(MarkdownFont::Editor, window, cx);
        style.base_text_style.font_size = ui(14.).into();
        style.base_text_style.line_height = ui(22.).into();
        style.base_text_style.color = colors.text;
        style.paragraph_spacing = px(MD_BLOCK_GAP);
        style.list_spacing = px(MD_BLOCK_GAP);
        style.table_cell_padding = point(px(12.), px(12.));
        style.container_style.margin.bottom = Some(px(-MD_BLOCK_GAP).into());
        style.heading.margin.top = Some(px(0.).into());
        style.heading.margin.bottom = Some(px(MD_BLOCK_GAP).into());
        let is_light = cx.theme().appearance.is_light();
        style.inline_code = TextStyleRefinement {
            color: Some(accent),
            background_color: Some(accent.opacity(if is_light { 0.10 } else { 0.12 })),
            ..code_text_style(cx)
        };
        style.link = TextStyleRefinement {
            color: Some(colors.text),
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(colors.text_muted),
                wavy: false,
            }),
            ..Default::default()
        };
        let mut code_block = StyleRefinement::default();
        code_block.padding.left = Some(px(12.).into());
        code_block.padding.right = Some(px(12.).into());
        code_block.padding.top = Some(px(10.).into());
        code_block.padding.bottom = Some(px(10.).into());
        code_block.text = TextStyleRefinement {
            font_size: Some(px(12.5).into()),
            line_height: Some(px(18.).into()),
            ..code_text_style(cx)
        };
        style.code_block = code_block;
        let heading = |size: f32, line_height: f32| {
            Some(TextStyleRefinement {
                font_size: Some(ui(size).into()),
                line_height: Some(ui(line_height).into()),
                font_weight: Some(FontWeight::SEMIBOLD),
                color: Some(colors.text),
                ..Default::default()
            })
        };
        style.heading_level_styles = Some(HeadingLevelStyles {
            h1: heading(19., 27.),
            h2: heading(16., 24.),
            h3: heading(15., 22.),
            h4: heading(14., 22.),
            h5: heading(14., 22.),
            h6: heading(14., 22.),
        });
        style.rule_color = colors.border;
        style.block_quote_border_color = accent.opacity(0.6);
        style.block_quote = TextStyleRefinement {
            color: Some(colors.text_muted),
            ..Default::default()
        };
        style.selection_background_color = accent.opacity(if is_light { 0.24 } else { 0.35 });
        style
    }

    fn thought_style(window: &Window, cx: &App) -> MarkdownStyle {
        let faint = text_faint(cx);
        let mut style = Self::message_style(window, cx);
        style.base_text_style.font_size = ui(THOUGHT_BODY_SIZE).into();
        style.base_text_style.line_height = px(OUTPUT_LINE_HEIGHT).into();
        // Markdown body text inherits its size from the container, not from base_text_style.
        style.container_style.text.font_size = Some(ui(THOUGHT_BODY_SIZE).into());
        style.base_text_style.color = faint;
        style.paragraph_spacing = px(OUTPUT_LINE_HEIGHT);
        style.list_spacing = px(OUTPUT_LINE_HEIGHT);
        style.container_style.margin.bottom = Some(px(-OUTPUT_LINE_HEIGHT).into());
        style.heading.margin.bottom = Some(px(OUTPUT_LINE_HEIGHT).into());
        style.inline_code = TextStyleRefinement {
            color: Some(faint),
            ..code_text_style(cx)
        };
        style.link = TextStyleRefinement {
            color: Some(faint),
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(faint),
                wavy: false,
            }),
            ..Default::default()
        };
        let mut code_block = StyleRefinement::default();
        code_block.margin.bottom = Some(px(OUTPUT_LINE_HEIGHT).into());
        code_block.text = TextStyleRefinement {
            font_size: Some(ui(THOUGHT_CODE_SIZE).into()),
            line_height: Some(px(OUTPUT_LINE_HEIGHT).into()),
            color: Some(faint),
            ..code_text_style(cx)
        };
        style.code_block = code_block;
        let heading = || {
            Some(TextStyleRefinement {
                font_size: Some(ui(THOUGHT_BODY_SIZE).into()),
                line_height: Some(px(OUTPUT_LINE_HEIGHT).into()),
                font_weight: Some(FontWeight::SEMIBOLD),
                color: Some(faint),
                ..Default::default()
            })
        };
        style.heading_level_styles = Some(HeadingLevelStyles {
            h1: heading(),
            h2: heading(),
            h3: heading(),
            h4: heading(),
            h5: heading(),
            h6: heading(),
        });
        style.block_quote = TextStyleRefinement {
            color: Some(faint),
            ..Default::default()
        };
        style
    }

    fn code_block_renderer(&self, message_key: usize, cx: &Context<Self>) -> CodeBlockRenderer {
        let view: WeakEntity<Self> = cx.entity().downgrade();
        let view_for_hide = view.clone();
        let copied = self.copied.clone();
        CodeBlockRenderer::Custom {
            render: Arc::new(move |kind, parsed, range, metadata, _, cx| {
                let colors = cx.theme().colors();
                let language: SharedString = match kind {
                    CodeBlockKind::FencedLang(language) => language.clone(),
                    CodeBlockKind::FencedSrc(path) => path
                        .path
                        .rsplit(['/', '\\'])
                        .next()
                        .map(|name| SharedString::from(name.to_string()))
                        .unwrap_or_default(),
                    _ => SharedString::default(),
                };
                let code = parsed
                    .source()
                    .get(metadata.content_range)
                    .unwrap_or_default()
                    .to_string();
                let key: SharedString = format!("code-{message_key}-{}", range.start).into();
                let is_copied = copied.as_ref() == Some(&key);
                let view = view.clone();
                let is_mermaid = language.eq_ignore_ascii_case("mermaid");
                let diagram = if is_mermaid {
                    view.upgrade().map(|chat| {
                        let shown_source =
                            chat.read(cx).diagram_sources_shown.contains(code.trim_end());
                        let diagram = chat.read(cx).diagram(&code, cx);
                        (shown_source, diagram)
                    })
                } else {
                    None
                };
                let toggle_key = key.clone();
                let toggle_code = code.trim_end().to_string();
                let diagram_view = view.clone();
                let muted = colors.text_muted;
                let ready_diagram = diagram.as_ref().and_then(|(shown_source, diagram)| {
                    matches!(diagram, Some(mermaid::Diagram::Ready { .. }))
                        .then_some(*shown_source)
                });
                let failure = diagram.as_ref().and_then(|(_, diagram)| match diagram {
                    Some(mermaid::Diagram::Failed(error)) => {
                        Some(SharedString::from(format!("Couldn't draw this diagram: {error}")))
                    }
                    _ => None,
                });
                let warning = cx.theme().status().warning;
                div()
                    .w_full()
                    .min_w_0()
                    .mb(px(MD_BLOCK_GAP))
                    .flex()
                    .flex_col()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(ink(0.035, cx))
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .h(px(CODE_HEADER_HEIGHT))
                            .flex_none()
                            .pl(px(12.))
                            .pr(px(5.))
                            .justify_between()
                            .border_b_1()
                            .border_color(colors.border)
                            .bg(ink(0.02, cx))
                            .child(
                                div()
                                    .min_w_0()
                                    .text_size(px(11.))
                                    .text_color(muted)
                                    .child(language),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap(px(2.))
                                    .when_some(failure, |this, failure| {
                                        this.child(
                                            div()
                                                .id(ElementId::Name(
                                                    format!("{toggle_key}-failure").into(),
                                                ))
                                                .size(px(CODE_ACTION_SIZE))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .tooltip(Tooltip::text(failure))
                                                .child(
                                                    icon(IconName::AgentDangerTriangle, 13.)
                                                        .text_color(warning),
                                                ),
                                        )
                                    })
                                    .when_some(ready_diagram, |this, shown_source| {
                                        let view = diagram_view.clone();
                                        this.child(
                                            div()
                                                .id(ElementId::Name(
                                                    format!("{toggle_key}-toggle").into(),
                                                ))
                                                .size(px(CODE_ACTION_SIZE))
                                                .rounded(px(6.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(|style| style.bg(ink(0.08, cx)))
                                                .tooltip(Tooltip::text(if shown_source {
                                                    "Show diagram"
                                                } else {
                                                    "Show source"
                                                }))
                                                .child(
                                                    icon(
                                                        if shown_source {
                                                            IconName::Eye
                                                        } else {
                                                            IconName::FileCode
                                                        },
                                                        13.,
                                                    )
                                                    .text_color(muted),
                                                )
                                                .on_click(move |_, _, cx| {
                                                    cx.stop_propagation();
                                                    let code = toggle_code.clone();
                                                    view.update(cx, |this, cx| {
                                                        if !this.diagram_sources_shown.remove(&code)
                                                        {
                                                            this.diagram_sources_shown.insert(code);
                                                        }
                                                        cx.notify();
                                                    })
                                                    .ok();
                                                }),
                                        )
                                    })
                                    .child(
                                        h_flex()
                                            .id(ElementId::Name(key.clone()))
                                            .h(px(CODE_ACTION_SIZE))
                                            .px(px(6.))
                                            .gap(px(4.))
                                            .rounded(px(5.))
                                            .cursor_pointer()
                                            .text_size(px(10.5))
                                            .text_color(muted)
                                            .hover(|style| style.bg(ink(0.08, cx)))
                                            .tooltip(Tooltip::text("Copy code"))
                                            .child(
                                                icon(
                                                    if is_copied {
                                                        IconName::AgentCheck
                                                    } else {
                                                        IconName::AgentCopy
                                                    },
                                                    12.,
                                                )
                                                .text_color(muted),
                                            )
                                            .when(is_copied, |this| this.child("Copied"))
                                            .on_click(move |_, _, cx| {
                                                cx.stop_propagation();
                                                let key = key.clone();
                                                let code = code.clone();
                                                view.update(cx, |this, cx| this.copy(key, code, cx))
                                                    .ok();
                                            }),
                                    ),
                            ),
                    )
                    .when_some(
                        diagram.and_then(|(shown_source, diagram)| match diagram {
                            Some(diagram @ mermaid::Diagram::Ready { .. }) if !shown_source => {
                                Some(diagram)
                            }
                            _ => None,
                        }),
                        |this, diagram| this.child(render_diagram(diagram, diagram_view.clone())),
                    )
            }),
            transform: None,
            hide_body: Some(Arc::new({
                let view = view_for_hide;
                move |kind, code, cx| {
                    let is_mermaid = matches!(kind, CodeBlockKind::FencedLang(language) if language.eq_ignore_ascii_case("mermaid"));
                    is_mermaid
                        && view.upgrade().is_some_and(|chat| {
                            let chat = chat.read(cx);
                            !chat.diagram_sources_shown.contains(code.trim_end())
                                && matches!(
                                    chat.diagram(code, cx),
                                    Some(mermaid::Diagram::Ready { .. })
                                )
                        })
                }
            })),
        }
    }

    fn sync_turns(&mut self, cx: &App) {
        let turns = group_turns(self.entries(cx));
        let old_count = self.turns.len();
        let unchanged = self
            .turns
            .iter()
            .zip(&turns)
            .take_while(|(old, new)| old.user == new.user)
            .count();
        // The growing last turn re-measures itself while visible; splicing it would reset the scroll.
        if unchanged < old_count || turns.len() > old_count {
            let start = unchanged.min(old_count);
            self.list.splice(start..old_count, turns.len() - start);
        }
        self.turns = turns;
    }

    fn render_turn(&mut self, turn_index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(Turn { user, items }) = self.turns.get(turn_index).cloned() else {
            return div().into_any_element();
        };
        let entries = self.entries(cx);
        let is_working = self.is_working(cx);
        let is_first_turn = turn_index == 0;
        let is_last_turn = turn_index + 1 == self.turns.len();
        let leading_gap = if is_first_turn && user.is_none() {
            0.
        } else {
            SPACE_LG
        };
        let undelivered = user.filter(|index| {
            self.subagent.is_none()
                && matches!(entries.get(*index), Some(Entry::User { undelivered: true, .. }))
        });
        let message_style = Self::message_style(window, cx);
        let user_text = user.and_then(|index| self.user_markdown(index, cx));
        let user_style = Self::user_style(window, cx);
        let painted_turns = self.painted_turns.clone();
        let turn = v_flex()
            .relative()
            .w_full()
            .child(
                canvas(
                    move |bounds, _, _| {
                        painted_turns.borrow_mut().insert(turn_index, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .pt(px(if is_first_turn {
                self.first_row_inset()
            } else {
                SPACE_LG
            }))
            .when(is_last_turn, |this| this.pb(px(TRANSCRIPT_BOTTOM_PAD)))
            .when_some(user, |this, index| {
                this.child(self.render_user_message(index, user_text, user_style, cx))
            })
            .when(
                !items.is_empty() || (is_last_turn && is_working),
                |this| {
                    this.child(self.render_assistant_turn(
                        turn_index,
                        &items,
                        is_last_turn,
                        leading_gap,
                        &message_style,
                        window,
                        cx,
                    ))
                },
            )
            .when_some(undelivered, |this, index| {
                this.child(self.render_retry(index, cx))
            });
        column(turn).into_any_element()
    }

    fn hover_strip(
        &self,
        group: SharedString,
        at: i64,
        copy: Option<(SharedString, CopySource)>,
        align_end: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let timestamp = chrono::DateTime::from_timestamp(at, 0)
            .filter(|_| at > 0)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%b %-d, %-I:%M %p")
                    .to_string()
            });
        let muted = cx.theme().colors().text_muted;
        h_flex()
            .h(px(SPACE_SM + SPACE_MD * 2.))
            .pt(px(SPACE_SM))
            .w_full()
            .when(align_end, |this| this.justify_end())
            .child(
                h_flex()
                    .gap(px(SPACE_SM))
                    .visible_on_hover(group)
                    .when_some(timestamp, |this, timestamp| {
                        this.child(
                            div()
                                .text_size(ui(12.))
                                .text_color(muted.opacity(0.55))
                                .child(timestamp),
                        )
                    })
                    .when_some(copy, |this, (copy_key, copy_source)| {
                        let is_copied = self.copied.as_ref() == Some(&copy_key);
                        this.child(
                            div()
                                .id(ElementId::Name(copy_key.clone()))
                                .size(px(SPACE_MD * 2.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .cursor_pointer()
                                .hover(|style| style.bg(ink(0.08, cx)))
                                .tooltip(Tooltip::text("Copy message"))
                                .child(
                                    icon(
                                        if is_copied {
                                            IconName::AgentCheck
                                        } else {
                                            IconName::AgentCopy
                                        },
                                        14.,
                                    )
                                    .text_color(muted),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let text = copy_source.text(this, cx);
                                    this.copy(copy_key.clone(), text, cx)
                                })),
                        )
                    }),
            )
    }

    fn user_markdown(&mut self, index: usize, cx: &mut Context<Self>) -> Option<Entity<Markdown>> {
        let Some(Entry::User { text, .. }) = self.entries(cx).get(index) else {
            return None;
        };
        let text = text.clone();
        if let Some(markdown) = self.user_markdown.get(&index)
            && markdown.read(cx).source() == text.as_ref()
        {
            return Some(markdown.clone());
        }
        let markdown = cx.new(|cx| Markdown::new_text(text, cx));
        self.highlight_new_message(index, &markdown, cx);
        self.user_markdown.insert(index, markdown.clone());
        Some(markdown)
    }

    fn user_style(window: &Window, cx: &App) -> MarkdownStyle {
        let mut style = Self::message_style(window, cx);
        style.base_text_style.line_height = ui(USER_LINE_HEIGHT).into();
        style.container_style.text.font_size = Some(ui(14.).into());
        style.container_style.text.line_height = Some(ui(USER_LINE_HEIGHT).into());
        style.container_style.margin.bottom = None;
        style.paragraph_spacing = px(0.);
        style
    }

    fn render_user_message(
        &self,
        index: usize,
        markdown: Option<Entity<Markdown>>,
        style: MarkdownStyle,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(Entry::User {
            text,
            at,
            attachments,
            ..
        }) = self.entries(cx).get(index)
        else {
            return div().into_any_element();
        };
        let text = text.clone();
        let attachments = attachments.clone();
        let line_count = text.lines().count();
        let collapsible =
            line_count > USER_COLLAPSED_LINES || text.chars().count() > USER_COLLAPSE_CHARS;
        let expanded = self.expanded_users.contains(&index);
        let collapsed = collapsible && !expanded;
        let group: SharedString = format!("agent-user-{index}").into();
        let toggle_group: SharedString = format!("agent-user-toggle-{index}").into();
        let colors = cx.theme().colors();
        let bubble_background = if cx.theme().appearance.is_light() {
            wash(0.04, cx)
        } else {
            wash(0.08, cx)
        };
        let user_text = markdown.map(|markdown| MarkdownElement::new(markdown, style));
        let copy = (!text.trim().is_empty()).then(|| {
            (
                SharedString::from(format!("copy-user-{index}")),
                CopySource::Text(text.clone()),
            )
        });
        v_flex()
            .group(group.clone())
            .w_full()
            .items_end()
            .when(!attachments.is_empty(), |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_none()
                        .flex_wrap()
                        .justify_end()
                        .items_start()
                        .gap(px(8.))
                        .px(px(4.))
                        .pt(px(4.))
                        .pb(px(6.))
                        .children(attachments.into_iter().enumerate().map(|(position, path)| {
                            div()
                                .id(SharedString::from(format!(
                                    "agent-sent-image-{index}-{position}"
                                )))
                                .flex_none()
                                .w(px(ATTACHMENT_THUMB_WIDTH))
                                .h(px(ATTACHMENT_THUMB_HEIGHT))
                                .rounded(px(8.))
                                .overflow_hidden()
                                .border_1()
                                .border_color(hairline(0.11, cx))
                                .bg(ink(0.035, cx))
                                .cursor_pointer()
                                .child(
                                    img(path.clone())
                                        .w(px(ATTACHMENT_THUMB_WIDTH - 2.))
                                        .h(px(ATTACHMENT_THUMB_HEIGHT - 2.))
                                        .rounded(px(7.))
                                        .object_fit(ObjectFit::Cover),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_image(path.clone(), window, cx)
                                }))
                        })),
                )
            })
            .child(
                v_flex()
                    .when(text.trim().is_empty(), |this| this.hidden())
                    .min_w_0()
                    .max_w(px(CONTENT_WIDTH * 0.8))
                    .px(px(16.))
                    .py(px(10.))
                    .rounded(px(16.))
                    .bg(bubble_background)
                    .text_size(ui(14.))
                    .line_height(ui(USER_LINE_HEIGHT))
                    .text_color(colors.text)
                    .map(|this| {
                        if collapsed {
                            this.child(
                                div()
                                    .max_h(ui(USER_LINE_HEIGHT * USER_COLLAPSED_LINES as f32))
                                    .overflow_hidden()
                                    .children(user_text),
                            )
                            .child(div().h(ui(USER_LINE_HEIGHT)).child("..."))
                        } else {
                            this.children(user_text)
                        }
                    })
                    .when(collapsible, |this| {
                        this.child(
                            div().mt(px(USER_TOGGLE_GAP)).flex().items_start().child(
                                h_flex()
                                    .id(("agent-user-expand", index))
                                    .group(toggle_group.clone())
                                    .gap(px(5.))
                                    .text_size(ui(14.))
                                    .line_height(ui(USER_LINE_HEIGHT))
                                    .text_color(colors.text_muted)
                                    .cursor_pointer()
                                    .hover(|style| style.text_color(colors.text))
                                    .child(if expanded { "Show less" } else { "Show more" })
                                    .child(
                                        icon(
                                            if expanded {
                                                IconName::AgentArrowUp
                                            } else {
                                                IconName::AgentArrowDown
                                            },
                                            12.,
                                        )
                                        .text_color(colors.text_muted)
                                        .group_hover(toggle_group, |style| {
                                            style.text_color(colors.text)
                                        }),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.expanded_users.remove(&index) {
                                            this.expanded_users.insert(index);
                                        }
                                        cx.notify();
                                    })),
                            ),
                        )
                    }),
            )
            .child(self.hover_strip(group, *at, copy, true, cx))
            .into_any_element()
    }

    fn render_retry(&self, index: usize, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .id(("agent-retry", index))
            .gap(px(SPACE_SM))
            .pt(px(SPACE_LG))
            .text_size(ui(12.))
            .text_color(cx.theme().status().error)
            .cursor_pointer()
            .child("Not delivered. Click to retry.")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.session
                    .update(cx, |session, cx| session.retry_undelivered(index, cx))
            }))
            .into_any_element()
    }

    fn group_header(
        &self,
        id: ElementId,
        open: bool,
        title: AnyElement,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = cx.theme().colors();
        let muted = colors.text_muted;
        let text = colors.text;
        let rotation = if open {
            0.
        } else {
            -std::f32::consts::FRAC_PI_2
        };
        h_flex()
            .id(id)
            .relative()
            .gap(px(6.))
            .pr(px(4.))
            .h(px(TOOL_GROUP_HEADER_HEIGHT))
            .cursor_pointer()
            .text_size(px(TOOL_TEXT_SIZE))
            .line_height(px(TOOL_LINE_HEIGHT))
            .text_color(muted)
            .hover(|style| style.text_color(text))
            .child(
                div().w(px(22.)).h(px(TOOL_LINE_HEIGHT)).flex_none().relative().child(
                    icon(IconName::AgentArrowDown, 14.)
                        .absolute()
                        .left(px(TREE_TRUNK_X - 7.))
                        .top(px(2.))
                        .with_transformation(Transformation::rotate(radians(rotation)))
                        .text_color(muted),
                ),
            )
            .child(
                div()
                    .min_w_0()
                    .h(px(TOOL_LINE_HEIGHT))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(title),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_assistant_turn(
        &self,
        turn_index: usize,
        items: &[usize],
        is_last_turn: bool,
        leading_gap: f32,
        message_style: &MarkdownStyle,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entries = self.entries(cx);
        let is_working = self.is_working(cx);
        let last_entry = entries.len().saturating_sub(1);
        let group: SharedString = format!("agent-turn-{turn_index}").into();
        let mut rows: Vec<(TranscriptRow, AnyElement)> = Vec::new();
        let all_items = items;
        let reply_position = items
            .iter()
            .rposition(|index| matches!(entries[*index], Entry::Assistant { .. }));
        let compact = AgentChatSettings::get_global(cx).compact_transcript
            && !(is_working && is_last_turn)
            && reply_position.is_some_and(|position| position > 0);
        let items = match reply_position {
            Some(position) if compact && !self.expanded_work.contains(&turn_index) => {
                &items[position..]
            }
            _ => items,
        };
        if compact {
            let started = all_items
                .first()
                .and_then(|first| first.checked_sub(1))
                .and_then(|index| match &entries[index] {
                    Entry::User { at, .. } => Some(*at),
                    _ => None,
                });
            let finished = all_items.iter().rev().find_map(|index| match &entries[*index] {
                Entry::Assistant { at, .. } | Entry::User { at, .. } => Some(*at),
                _ => None,
            });
            let label = match (started, finished) {
                (Some(started), Some(finished)) if finished > started => {
                    format!("Worked for {}", format_elapsed((finished - started) as u64))
                }
                _ => "Worked".to_string(),
            };
            let open = self.expanded_work.contains(&turn_index);
            rows.push((
                TranscriptRow::ToolGroup,
                self.group_header(
                    ("agent-worked", turn_index).into(),
                    open,
                    SharedString::from(label).into_any_element(),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.expanded_work.remove(&turn_index) {
                        this.expanded_work.insert(turn_index);
                    }
                    cx.notify();
                }))
                .into_any_element(),
            ));
        }
        let is_agent = |index: usize| {
            matches!(&entries[index], Entry::Tool(tool) if tool.call.is_subagent_spawn())
        };
        let mut cursor = 0;
        while cursor < items.len() {
            let index = items[cursor];
            match &entries[index] {
                Entry::Tool(_) | Entry::Thinking(_) => {
                    let genus = is_agent(index);
                    let end = items[cursor..]
                        .iter()
                        .position(|index| {
                            !matches!(entries[*index], Entry::Tool(_) | Entry::Thinking(_))
                                || is_agent(*index) != genus
                        })
                        .map_or(items.len(), |offset| cursor + offset);
                    let group_items = &items[cursor..end];
                    let is_live = is_working && group_items.contains(&last_entry);
                    rows.push((
                        TranscriptRow::ToolGroup,
                        self.render_tool_group(group_items, is_live, window, cx),
                    ));
                    cursor = end;
                }
                Entry::Assistant { markdown, .. } => {
                    let view = cx.entity().downgrade();
                    let link_view = view.clone();
                    let cwd = self.session.read(cx).metadata().cwd.clone();
                    let menu_markdown = markdown.clone();
                    rows.push((
                        TranscriptRow::Other,
                        div()
                            .w_full()
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                    if let Some(url) =
                                        menu_markdown.read(cx).context_menu_link().cloned()
                                    {
                                        this.show_link_menu(url, event.position, window, cx);
                                        cx.stop_propagation();
                                    }
                                }),
                            )
                            .with_animation(
                                ("agent-reply-fade", index),
                                Animation::new(FADE_IN).with_easing(ease_out_quint()),
                                |this, delta| this.opacity(delta),
                            )
                            .child(
                                MarkdownElement::new(markdown.clone(), message_style.clone())
                                    .code_block_renderer(self.code_block_renderer(index, cx))
                                    .on_url_click(move |url, window, cx| {
                                        link_view
                                            .update(cx, |this, cx| this.open_link(url, window, cx))
                                            .ok();
                                    })
                                    .on_code_span_link(move |text, cx| {
                                        view.upgrade()?.read(cx).code_span_url(text, &cwd)
                                    }),
                            )
                            .into_any_element(),
                    ));
                    cursor += 1;
                }
                Entry::Notice { text, is_error } => {
                    rows.push((
                        TranscriptRow::Other,
                        if *is_error {
                            self.render_error(index, text.clone(), cx)
                        } else {
                            div()
                                .text_size(ui(12.))
                                .text_color(cx.theme().colors().text_muted)
                                .child(text.clone())
                                .into_any_element()
                        },
                    ));
                    cursor += 1;
                }
                Entry::Image { path } => {
                    let path = path.clone();
                    rows.push((
                        TranscriptRow::Other,
                        div()
                            .id(("agent-generated-image", index))
                            .max_w(px(GENERATED_IMAGE_MAX_WIDTH))
                            .max_h(px(GENERATED_IMAGE_MAX_HEIGHT))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(12.))
                            .overflow_hidden()
                            .bg(ink(0.045, cx))
                            .cursor_pointer()
                            .child(
                                img(path.clone())
                                    .max_w(px(GENERATED_IMAGE_MAX_WIDTH))
                                    .max_h(px(GENERATED_IMAGE_MAX_HEIGHT))
                                    .rounded(px(12.))
                                    .object_fit(ObjectFit::Contain),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_image(path.clone(), window, cx)
                            }))
                            .into_any_element(),
                    ));
                    cursor += 1;
                }
                Entry::User { .. } => cursor += 1,
            }
        }

        let settled = !(is_working && is_last_turn);
        let message_items: Vec<usize> = all_items
            .iter()
            .copied()
            .filter(|index| matches!(entries[*index], Entry::Assistant { .. }))
            .collect();
        let last_at = all_items
            .iter()
            .rev()
            .find_map(|index| match &entries[*index] {
                Entry::Assistant { at, .. } => Some(*at),
                _ => None,
            })
            .unwrap_or_default();
        let copy = (!message_items.is_empty()).then(|| {
            (
                SharedString::from(format!("copy-turn-{turn_index}")),
                CopySource::Messages(message_items),
            )
        });
        let show_strip = settled && !rows.is_empty() && (copy.is_some() || last_at > 0);

        let mut previous = None;
        let rows = rows.into_iter().map(|(kind, element)| {
            let gap = match previous {
                None => leading_gap,
                Some(previous) => top_gap(previous, kind),
            };
            previous = Some(kind);
            div().w_full().pt(px(gap)).child(element)
        });

        v_flex()
            .group(group.clone())
            .w_full()
            .children(rows)
            .when(show_strip, |this| {
                this.child(self.hover_strip(group, last_at, copy, false, cx))
            })
            .when(is_working && is_last_turn, |this| {
                this.child(self.render_working_trailer(cx))
            })
            .into_any_element()
    }

    fn render_working_trailer(&self, cx: &Context<Self>) -> AnyElement {
        let elapsed = self
            .working_since(cx)
            .map(|since| since.elapsed().as_secs())
            .unwrap_or(0);
        let label = if self.subagent.is_none() && self.session.read(cx).is_compacting() {
            "Compacting conversation…".to_string()
        } else {
            format!("{}…", flavour_word(self.flavour_seed, elapsed))
        };
        let colors = cx.theme().colors();
        h_flex()
            .gap(px(SPACE_SM))
            .pt(px(SPACE_LG))
            .text_size(ui(11.))
            .child(gradient_spinner(
                format!("agent-working-{}", cx.entity_id()).into(),
                2.5,
            ))
            .child(
                div()
                    .text_size(ui(12.))
                    .text_color(colors.text_muted)
                    .child(label),
            )
            .child(
                div()
                    .relative()
                    .top(px(1.))
                    .text_color(text_faint(cx))
                    .child(format_elapsed(elapsed)),
            )
            .into_any_element()
    }

    fn render_error(&self, index: usize, text: SharedString, cx: &Context<Self>) -> AnyElement {
        let danger = cx.theme().status().error;
        let danger_muted = mix(danger, cx.theme().colors().text, 0.28).opacity(0.8);
        let copy_key: SharedString = format!("copy-error-{index}").into();
        let is_copied = self.copied.as_ref() == Some(&copy_key);
        div()
            .py(px(4.))
            .w_full()
            .child(
                v_flex()
                    .w_full()
                    .overflow_hidden()
                    .gap(px(6.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(danger.opacity(0.16))
                    .bg(danger.opacity(0.05))
                    .px(px(10.))
                    .py(px(8.))
                    .text_size(px(12.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(20.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.))
                                    .bg(danger.opacity(0.12))
                                    .child(
                                        icon(IconName::AgentDangerTriangle, 12.)
                                            .text_color(danger_muted),
                                    ),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(danger_muted)
                                    .child("Error"),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .id(ElementId::Name(copy_key.clone()))
                                    .flex_none()
                                    .size(px(20.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(danger.opacity(0.12)))
                                    .tooltip(Tooltip::text("Copy message"))
                                    .child(
                                        icon(
                                            if is_copied {
                                                IconName::AgentCheck
                                            } else {
                                                IconName::AgentCopy
                                            },
                                            12.,
                                        )
                                        .text_color(danger_muted),
                                    )
                                    .on_click(cx.listener({
                                        let text = text.to_string();
                                        move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.copy(copy_key.clone(), text.clone(), cx)
                                        }
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .w_full()
                            .text_color(cx.theme().colors().text.opacity(0.8))
                            .child(text),
                    ),
            )
            .into_any_element()
    }

    fn group_summary(&self, items: &[usize], cx: &App) -> String {
        let entries = self.entries(cx);
        let mut tools = Vec::new();
        let mut thoughts = 0;
        for index in items {
            match &entries[*index] {
                Entry::Tool(tool) => {
                    tools.push((tool.call.clone(), tool.status == ToolStatus::Failed))
                }
                Entry::Thinking(_) => thoughts += 1,
                _ => {}
            }
        }
        let mut segments: Vec<String> = Vec::new();
        match thoughts {
            0 => {}
            1 => segments.push("thought process".into()),
            count => segments.push(format!("thought {count} times")),
        }
        if !tools.is_empty() {
            segments.push(view::tool_group_summary(&tools));
        }
        let mut summary = segments.join(" · ");
        if let Some(first) = summary.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        summary
    }

    fn render_tool_group(
        &self,
        items: &[usize],
        is_live: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entries = self.entries(cx);
        let collapses = items.iter().any(|index| match &entries[*index] {
            Entry::Tool(tool) => !tool.call.is_subagent_spawn(),
            _ => true,
        });
        if !collapses {
            return v_flex()
                .w_full()
                .pt(px(CHIPS_TOP_PAD))
                .children(
                    items
                        .iter()
                        .map(|index| self.render_subagent_chip(*index, cx)),
                )
                .into_any_element();
        }
        let group_key = match items.first().map(|index| &entries[*index]) {
            Some(Entry::Tool(tool)) => format!("tools-{}", tool.id),
            _ => format!("tools-at-{}", items.first().copied().unwrap_or_default()),
        };
        let open = self
            .group_overrides
            .get(&group_key)
            .copied()
            .unwrap_or(is_live);
        let summary: SharedString = self.group_summary(items, cx).into();
        let colors = cx.theme().colors();
        let title = if is_live {
            shimmer_title(
                summary,
                ElementId::Name(format!("{group_key}-shimmer").into()),
                colors.text_muted,
                colors.text,
            )
        } else {
            summary.into_any_element()
        };
        let header = self
            .group_header(
                ElementId::Name(format!("{group_key}-header").into()),
                open,
                title,
                cx,
            )
            .on_click(cx.listener({
                move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.group_overrides.insert(group_key.clone(), !open);
                    cx.notify();
                }
            }));

        let row_count = items.len();
        v_flex()
            .w_full()
            .child(header)
            .when(open, |this| {
                this.child(
                    v_flex()
                        .pt(px(CHIPS_TOP_PAD))
                        .children(items.iter().enumerate().map(|(position, index)| {
                            self.render_tree_row(
                                *index,
                                position + 1 < row_count,
                                window,
                                cx,
                            )
                        })),
                )
            })
            .into_any_element()
    }

    fn render_tree_row(
        &self,
        index: usize,
        continues: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entries = self.entries(cx);
        let colors = cx.theme().colors();
        let danger = cx.theme().status().error;
        let (key, icon_name, label, detail, failed): (String, IconName, SharedString, String, bool) =
            match &entries[index] {
                Entry::Tool(tool) => {
                    let (label, detail) = view::tool_chip_content(&tool.call);
                    (
                        format!("tool-{}", tool.id),
                        tool_icon(&tool.call),
                        label.into(),
                        detail,
                        tool.status == ToolStatus::Failed,
                    )
                }
                Entry::Thinking(_) => (
                    format!("thought-{index}"),
                    IconName::AgentChatRoundLine,
                    "Thought process".into(),
                    String::new(),
                    false,
                ),
                _ => return div().into_any_element(),
            };
        let stats = match &entries[index] {
            Entry::Tool(tool) => tool
                .diff
                .as_ref()
                .map(|diff| self.diff_view(&tool.id, diff, cx))
                .map(|view| (view.additions, view.deletions)),
            _ => None,
        };
        let expanded = self.expanded_rows.contains(&key);
        let body = expanded.then(|| match &entries[index] {
            Entry::Tool(tool) => self.render_tool_detail(tool, cx),
            Entry::Thinking(markdown) => v_flex()
                .w_full()
                .min_w_0()
                .child(detail_separator())
                .child(
                    div().py(px(6.)).child(
                        MarkdownElement::new(markdown.clone(), Self::thought_style(window, cx))
                            .code_block_renderer(plain_code_block_renderer()),
                    ),
                )
                .into_any_element(),
            _ => div().into_any_element(),
        });
        let tint = if failed { danger } else { colors.text_muted };
        let text = colors.text;
        let hover_group: SharedString = format!("{key}-header").into();
        let hover_text = !failed;
        let header_row = h_flex()
            .group(hover_group.clone())
            .h(px(CHIP_CARD_HEIGHT))
            .w_full()
            .min_w_0()
            .gap(px(8.))
            .text_size(px(TOOL_TEXT_SIZE))
            .line_height(px(TOOL_LINE_HEIGHT))
            .child(
                div()
                    .id(ElementId::Name(format!("{key}-label").into()))
                    .flex_none()
                    .h(px(TOOL_LINE_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(tint)
                    .when(hover_text, |this| {
                        this.group_hover(hover_group.clone(), |style| style.text_color(text))
                    })
                    .child(label),
            )
            .child(
                div()
                    .id(ElementId::Name(format!("{key}-detail").into()))
                    .min_w_0()
                    .h(px(TOOL_LINE_HEIGHT))
                    .flex()
                    .items_center()
                    .when(detail.is_empty(), |this| this.hidden())
                    .truncate()
                    .text_color(tint)
                    .when(hover_text, |this| {
                        this.group_hover(hover_group.clone(), |style| style.text_color(text))
                    })
                    .child(div().min_w_0().truncate().child(detail)),
            )
            .when_some(stats, |this, (additions, deletions)| {
                let status = cx.theme().status();
                this.child(
                    h_flex()
                        .flex_none()
                        .gap(px(4.))
                        .font(code_font(cx))
                        .text_size(px(11.))
                        .when(additions > 0, |this| {
                            this.child(
                                div()
                                    .text_color(status.created)
                                    .child(format!("+{additions}")),
                            )
                        })
                        .when(deletions > 0, |this| {
                            this.child(
                                div()
                                    .text_color(status.deleted)
                                    .child(format!("−{deletions}")),
                            )
                        }),
                )
            })
            .child(
                div()
                    .size(px(18.))
                    .flex_none()
                    .opacity(0.)
                    .group_hover(hover_group.clone(), |style| style.opacity(1.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        icon(
                            if expanded {
                                IconName::AgentArrowDown
                            } else {
                                IconName::AgentArrowRight
                            },
                            12.,
                        )
                        .text_color(text_faint(cx))
                        .group_hover(hover_group, move |style| {
                            style.text_color(if failed { danger } else { text })
                        }),
                    ),
            );
        let card = v_flex()
            .ml(px(TREE_TEXT_GAP))
            .my(px((TREE_ROW_HEIGHT - CHIP_CARD_HEIGHT) / 2.))
            .min_w_0()
            .flex_1()
            .overflow_hidden()
            .child(
                div()
                    .id(ElementId::Name(key.clone().into()))
                    .h(px(CHIP_CARD_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(header_row)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if !this.expanded_rows.remove(&key) {
                            this.expanded_rows.insert(key.clone());
                        }
                        cx.notify();
                    })),
            )
            .children(body);
        div()
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .with_animation(
                ("agent-tree-row-fade", index),
                Animation::new(FADE_IN).with_easing(ease_out_quint()),
                |this, delta| this.opacity(delta),
            )
            .child(activity_rail(
                icon_name,
                tint,
                continues,
                hairline(0.12, cx),
            ))
            .child(card)
            .into_any_element()
    }

    fn render_subagent_chip(&self, index: usize, cx: &Context<Self>) -> AnyElement {
        let Some(Entry::Tool(tool)) = self.entries(cx).get(index) else {
            return div().into_any_element();
        };
        let colors = cx.theme().colors();
        let danger = cx.theme().status().error;
        let (label, detail) = view::tool_chip_content(&tool.call);
        let running = self.session.read(cx).subagent_running(&tool.id);
        let detail = running
            .then(|| self.subagent_activity(&tool.id, cx))
            .flatten()
            .unwrap_or(detail);
        let failed = tool.status == ToolStatus::Failed;
        let tint = if failed { danger } else { colors.text_muted };
        let model = tool.call.subagent_model().map(|model| model.to_string());
        let id = tool.id.clone();
        let call = tool.call.clone();
        div()
            .h(px(CHIP_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .child(
                div()
                    .id(SharedString::from(format!("subagent-{}", tool.id)))
                    .h(px(CHIP_CARD_HEIGHT))
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded(px(9.))
                    .border_1()
                    .border_color(hairline(0.07, cx))
                    .bg(ink(0.03, cx))
                    .cursor_pointer()
                    .hover(|style| style.bg(ink(0.05, cx)))
                    .tooltip(Tooltip::text("Open subagent"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_subagent(id.clone(), &call, window, cx)
                    }))
                    .child(
                        h_flex()
                            .h(px(CHIP_HEADER_HEIGHT))
                            .w_full()
                            .min_w_0()
                            .gap(px(8.))
                            .px(px(8.))
                            .text_size(px(TOOL_TEXT_SIZE))
                            .line_height(px(TOOL_LINE_HEIGHT))
                            .child(
                                div()
                                    .size(px(18.))
                                    .flex_none()
                                    .rounded(px(5.))
                                    .bg(ink(0.08, cx))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        icon(tool_icon(&tool.call), 12.)
                                            .text_color(colors.text_muted),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(TOOL_LINE_HEIGHT))
                                    .flex()
                                    .items_center()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tint)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(TOOL_LINE_HEIGHT))
                                    .flex()
                                    .items_center()
                                    .truncate()
                                    .text_color(if failed {
                                        danger
                                    } else {
                                        colors.text.opacity(0.85)
                                    })
                                    .child(div().min_w_0().truncate().child(detail)),
                            )
                            .when_some(model, |this, model| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .h(px(18.))
                                        .flex()
                                        .items_center()
                                        .text_size(px(11.))
                                        .text_color(text_faint(cx))
                                        .child(model),
                                )
                            })
                            .when(running, |this| {
                                this.child(div().flex_none().child(subagent_spinner(
                                    format!("subagent-spinner-{}", tool.id).into(),
                                    accent(cx),
                                    cx.theme().appearance.is_light(),
                                )))
                            })
                            .child(
                                div()
                                    .size(px(18.))
                                    .flex_none()
                                    .rounded(px(5.))
                                    .bg(ink(0.06, cx))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        icon(IconName::AgentArrowUpRight, 11.)
                                            .text_color(colors.text_muted.opacity(0.8)),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn subagent_activity(&self, id: &str, cx: &App) -> Option<String> {
        let subagent = self.session.read(cx).subagent(id)?;
        subagent.entries().iter().rev().find_map(|entry| match entry {
            Entry::Tool(tool) => {
                let (label, detail) = view::tool_chip_content(&tool.call);
                Some(if detail.is_empty() {
                    label.to_string()
                } else {
                    format!("{label} {detail}")
                })
            }
            Entry::Assistant { markdown, .. } => {
                let text = view::single_line(markdown.read(cx).source());
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        })
    }

    fn render_tool_detail(&self, tool: &ToolEntry, cx: &Context<Self>) -> AnyElement {
        let faint = text_faint(cx);
        let show_all = self.full_output.contains(&tool.id);
        let max_lines = if show_all { usize::MAX } else { OUTPUT_MAX_LINES };
        let call_text = view::tool_call_text(&tool.call);
        let invocation = clip_lines(&call_text, CALL_WRAP_COLUMNS, max_lines);
        let output = clip_lines(tool.output.as_deref().unwrap_or(""), usize::MAX, max_lines);
        let output_size = tool.output.as_ref().map_or(0, |output| output.len());
        let diff = tool
            .diff
            .as_ref()
            .map(|diff| self.diff_view(&tool.id, diff, cx));
        let invocation_hidden = invocation.hidden;
        let output_hidden = output.hidden;
        v_flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .when(!invocation.lines.is_empty(), |this| {
                this.child(detail_separator())
                    .child(detail_lines(
                        invocation.lines,
                        invocation_hidden,
                        faint,
                        code_font(cx),
                    ))
                    .when(invocation_hidden > 0, |this| {
                        this.child(self.render_show_full(
                            format!("agent-full-input-{}", tool.id),
                            format!("Show full input ({})", format_bytes(call_text.len())),
                            &tool.id,
                            cx,
                        ))
                    })
            })
            .map(|this| match diff {
                Some(diff) => this
                    .child(detail_separator())
                    .child(self.render_diff(&diff, cx)),
                None if !output.lines.is_empty() => this
                    .child(detail_separator())
                    .child(detail_lines(
                        output.lines,
                        output_hidden,
                        faint,
                        code_font(cx),
                    ))
                    .when(output_hidden > 0, |this| {
                        this.child(self.render_show_full(
                            format!("agent-full-output-{}", tool.id),
                            format!("Show full output ({})", format_bytes(output_size)),
                            &tool.id,
                            cx,
                        ))
                    }),
                None => this,
            })
            .into_any_element()
    }

    fn render_show_full(
        &self,
        id: String,
        label: String,
        tool_id: &str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let tool_id = tool_id.to_string();
        let muted = cx.theme().colors().text_muted;
        div()
            .id(SharedString::from(id))
            .h(px(BLOB_AFFORDANCE_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .text_size(px(TOOL_TEXT_SIZE))
            .text_color(text_faint(cx))
            .cursor_pointer()
            .hover(|style| style.text_color(muted))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.full_output.insert(tool_id.clone());
                cx.notify();
            }))
    }

    fn render_new_chat(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let project_name: SharedString = self
            .session
            .read(cx)
            .metadata()
            .cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned().into())
            .unwrap_or_default();
        v_flex()
            .size_full()
            .justify_center()
            .items_center()
            .pt(px(16.))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(COMPOSER_WIDTH))
                    .px(px(16.))
                    .gap(px(8.))
                    .child(
                        h_flex().h(px(20.)).px(px(10.)).justify_end().child(
                            Self::footer_label_shell(IconName::AgentFolder, cx)
                                .child(div().min_w_0().truncate().child(project_name)),
                        ),
                    )
                    .child(self.render_pill(true, COMPOSER_RADIUS, window, cx))
                    .child(
                        h_flex()
                            .h(px(24.))
                            .px(px(10.))
                            .child(self.render_checkout_picker(cx)),
                    ),
            )
            .into_any_element()
    }
}

pub(crate) fn code_font(cx: &App) -> Font {
    ThemeSettings::get_global(cx).buffer_font.clone()
}

fn code_text_style(cx: &App) -> TextStyleRefinement {
    let font = code_font(cx);
    TextStyleRefinement {
        font_family: Some(font.family),
        font_features: Some(font.features),
        font_fallbacks: font.fallbacks,
        font_weight: Some(font.weight),
        font_style: Some(font.style),
        ..Default::default()
    }
}

fn render_diagram(diagram: mermaid::Diagram, view: WeakEntity<ChatView>) -> AnyElement {
    let mermaid::Diagram::Ready {
        image,
        width,
        height,
    } = diagram
    else {
        return gpui::Empty.into_any_element();
    };
    div()
        .id(SharedString::from(format!("agent-diagram-{}", image.id())))
        .w_full()
        .max_w(px(width))
        .mx_auto()
        .max_h(px(DIAGRAM_MAX_HEIGHT))
        .aspect_ratio(width / height.max(1.))
        .cursor_pointer()
        .child(
            img(image.clone())
                .size_full()
                .object_fit(ObjectFit::Contain),
        )
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let image = image.clone();
            view.update(cx, |this, cx| this.open_image_data(image, window, cx))
                .ok();
        })
        .into_any_element()
}

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    }
}

fn subagent_title(call: &ToolCall) -> SharedString {
    let (name, input) = match call {
        ToolCall::Unknown { name, input } => (name.as_str(), input.as_ref()),
        ToolCall::Mcp { tool, input, .. } => (tool.as_str(), input.as_ref()),
        _ => return "Subagent".into(),
    };
    let candidates = [
        Some(name),
        input.and_then(|input| input.get("description")?.as_str()),
        input.and_then(|input| input.get("prompt")?.as_str()),
    ];
    candidates
        .into_iter()
        .flatten()
        .map(|text| view::single_line(text.strip_prefix("Agent").unwrap_or(text)))
        .map(|text| text.trim_start_matches(':').trim().to_string())
        .find(|text| !text.is_empty())
        .map_or_else(|| "Subagent".into(), Into::into)
}

fn icon(name: IconName, size: f32) -> Svg {
    svg().path(name.path()).size(px(size)).flex_none()
}

enum CopySource {
    Text(SharedString),
    Messages(Vec<usize>),
}

impl CopySource {
    fn text(&self, chat: &ChatView, cx: &App) -> String {
        match self {
            CopySource::Text(text) => text.to_string(),
            CopySource::Messages(indices) => {
                let entries = chat.entries(cx);
                indices
                    .iter()
                    .filter_map(|index| match entries.get(*index) {
                        Some(Entry::Assistant { markdown, .. }) => {
                            Some(markdown.read(cx).source().to_string())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum TranscriptRow {
    ToolGroup,
    Other,
}

fn top_gap(previous: TranscriptRow, current: TranscriptRow) -> f32 {
    if previous == TranscriptRow::ToolGroup || current == TranscriptRow::ToolGroup {
        SPACE_MD
    } else {
        SPACE_SM
    }
}

fn shimmer_amount(x: f32, phase: f32) -> f32 {
    let primary_center = -2.5 + phase.clamp(0., 1.) * 6.;
    (-2..=2)
        .map(|copy| primary_center + copy as f32 * 3.)
        .map(|center| (1. - (x - center).abs() / TOOL_SHIMMER_HALF_WIDTH).clamp(0., 1.))
        .fold(0., f32::max)
}

fn shimmer_overlay(text: SharedString, phase: f32, base: Hsla, peak: Hsla) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            let font = window.text_style().font();
            let run = |color: Hsla| TextRun {
                len: text.len(),
                font: font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let text_system = window.text_system();
            let probe =
                text_system.shape_line(text.clone(), px(TOOL_TEXT_SIZE), &[run(peak)], None);
            let text_width = f32::from(probe.width()).min(f32::from(bounds.size.width));
            let strip_count = (text_width / TOOL_SHIMMER_STRIP_WIDTH).ceil() as usize;
            let mut strips = Vec::with_capacity(strip_count);
            for strip in 0..strip_count {
                let left = strip as f32 * TOOL_SHIMMER_STRIP_WIDTH;
                let right = ((strip + 1) as f32 * TOOL_SHIMMER_STRIP_WIDTH).min(text_width);
                let x = (left + right) * 0.5 / text_width.max(1.);
                let amount = shimmer_amount(x, phase);
                if amount <= 0.001 {
                    continue;
                }
                let line = text_system.shape_line(
                    text.clone(),
                    px(TOOL_TEXT_SIZE),
                    &[run(mix(base, peak, amount))],
                    None,
                );
                strips.push((left, right, line));
            }
            strips
        },
        move |bounds, strips, window, cx| {
            for (left, right, line) in strips {
                let mask = ContentMask {
                    bounds: Bounds {
                        origin: point(bounds.origin.x + px(left), bounds.origin.y),
                        size: size(px(right - left), bounds.size.height),
                    },
                };
                window.with_content_mask(Some(mask), |window| {
                    line.paint(
                        bounds.origin,
                        px(TOOL_LINE_HEIGHT),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .log_err();
                });
            }
        },
    )
    .absolute()
    .inset_0()
}

fn shimmer_title(text: SharedString, id: ElementId, base: Hsla, peak: Hsla) -> AnyElement {
    div()
        .relative()
        .h_full()
        .min_w_0()
        .flex_1()
        .overflow_hidden()
        .child(text.clone())
        .with_animation(
            id,
            Animation::new(GROUP_SHIMMER).repeat(),
            move |title, phase| title.child(shimmer_overlay(text.clone(), phase, base, peak)),
        )
        .into_any_element()
}

fn activity_ribbon(path: &mut PathBuilder, points: &[Point<Pixels>]) {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return;
    };
    let mut left = Vec::with_capacity(points.len());
    let mut right = Vec::with_capacity(points.len());
    for (position, current) in points.iter().enumerate() {
        let previous = points.get(position.wrapping_sub(1)).unwrap_or(first);
        let next = points.get(position + 1).unwrap_or(last);
        let dx = f32::from(next.x - previous.x);
        let dy = f32::from(next.y - previous.y);
        let length = dx.hypot(dy).max(0.0001);
        let normal = point(px(-dy / length * 0.5), px(dx / length * 0.5));
        left.push(*current + normal);
        right.push(*current - normal);
    }
    let Some(start) = left.first() else {
        return;
    };
    path.move_to(*start);
    for corner in left.iter().skip(1).chain(right.iter().rev()) {
        path.line_to(*corner);
    }
    path.close();
}

fn activity_branch_points() -> Vec<Point<f32>> {
    let mut points: Vec<Point<f32>> = (0..=24)
        .map(|step| {
            let t = step as f32 / 24.;
            point(
                TREE_BEND_RADIUS * t * t,
                TREE_BEND_RADIUS * (2. * t - t * t),
            )
        })
        .collect();
    points.push(point(TREE_BRANCH_END_X - TREE_TRUNK_X, TREE_BEND_RADIUS));
    points
}

fn activity_rail(
    icon_name: IconName,
    tint: Hsla,
    continues: bool,
    color: Hsla,
) -> impl IntoElement {
    div()
        .relative()
        .w(px(TREE_GUTTER))
        .flex_none()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let x = bounds.origin.x + px(TREE_TRUNK_X);
                    let bend_y = bounds.origin.y + px(TREE_ROW_HEIGHT / 2. - TREE_BEND_RADIUS);
                    let bottom = if continues { bounds.bottom() } else { bend_y };
                    let mut tree = PathBuilder::fill().with_style(PathStyle::Fill(
                        FillOptions::default().with_fill_rule(FillRule::NonZero),
                    ));
                    activity_ribbon(&mut tree, &[point(x, bounds.origin.y), point(x, bottom)]);
                    let branch: Vec<Point<Pixels>> = activity_branch_points()
                        .into_iter()
                        .map(|offset| point(x + px(offset.x), bend_y + px(offset.y)))
                        .collect();
                    activity_ribbon(&mut tree, &branch);
                    if let Ok(path) = tree.build() {
                        window.paint_path(path, color);
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .child(
            icon(icon_name, TREE_ICON_SIZE)
                .absolute()
                .left(px(TREE_ICON_LEFT))
                .top(px(TREE_ROW_HEIGHT / 2. - TREE_ICON_SIZE / 2.))
                .text_color(tint),
        )
}

fn detail_separator() -> impl IntoElement {
    div().h(px(DETAIL_SEPARATOR)).flex_none()
}

fn detail_lines(
    lines: Vec<String>,
    hidden: usize,
    color: Hsla,
    font: Font,
) -> impl IntoElement {
    v_flex()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .py(px(6.))
        .font(font)
        .text_size(px(TOOL_TEXT_SIZE))
        .children(lines.into_iter().map(move |line| {
            div()
                .h(px(OUTPUT_LINE_HEIGHT))
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .text_color(color)
                .child(div().w_full().min_w_0().truncate().child(line))
        }))
        .when(hidden > 0, |this| {
            this.child(
                div()
                    .h(px(OUTPUT_LINE_HEIGHT))
                    .flex()
                    .items_center()
                    .text_size(px(TOOL_TEXT_SIZE))
                    .text_color(color)
                    .child(format!("… {hidden} more lines")),
            )
        })
}

#[derive(Debug, PartialEq)]
struct ClippedLines {
    lines: Vec<String>,
    hidden: usize,
}

fn wrapped_line_count(line: &str, columns: usize) -> usize {
    line.chars().count().div_ceil(columns).max(1)
}

fn clip_lines(text: &str, columns: usize, max_lines: usize) -> ClippedLines {
    let mut lines = Vec::new();
    let mut hidden = 0;
    for line in text.trim_end().lines() {
        if lines.len() >= max_lines {
            hidden += wrapped_line_count(line, columns);
            continue;
        }
        let mut rest = line;
        loop {
            let split = rest
                .char_indices()
                .nth(columns)
                .map_or(rest.len(), |(index, _)| index);
            let (chunk, tail) = rest.split_at(split);
            lines.push(chunk.to_string());
            rest = tail;
            if rest.is_empty() {
                break;
            }
            if lines.len() >= max_lines {
                hidden += wrapped_line_count(rest, columns);
                break;
            }
        }
    }
    ClippedLines { lines, hidden }
}

fn plain_code_block_renderer() -> CodeBlockRenderer {
    CodeBlockRenderer::Custom {
        render: Arc::new(|_, _, _, _, _, _| div().w_full()),
        transform: None,
        hide_body: None,
    }
}

fn spinner_opacity(t: f32) -> f32 {
    let t = t.rem_euclid(1.);
    if t < 0.45 {
        1. + (SUBAGENT_SPINNER_DIM - 1.) * (t / 0.45)
    } else if t < 0.92 {
        SUBAGENT_SPINNER_DIM
    } else {
        SUBAGENT_SPINNER_DIM + (1. - SUBAGENT_SPINNER_DIM) * ((t - 0.92) / 0.08)
    }
}

fn subagent_spinner(id: SharedString, accent: Hsla, is_light: bool) -> impl IntoElement {
    const RING: [[usize; 2]; 3] = [[0, 1], [5, 2], [4, 3]];
    let mut light = accent;
    let mut deep = accent;
    if is_light {
        light.l = (light.l + 0.11).min(0.76);
        light.s *= 0.78;
        deep.l = (deep.l - 0.09).max(0.22);
    } else {
        light.l = (light.l + 0.14).min(0.9);
        light.s *= 0.72;
        deep.l = (deep.l - 0.08).max(0.22);
    }
    let tints = [light, accent, deep];
    let cell = SUBAGENT_SPINNER_CELL;
    v_flex()
        .flex_none()
        .gap(px(cell / 2.))
        .children(tints.into_iter().enumerate().map(move |(row, tint)| {
            let id = id.clone();
            h_flex()
                .gap(px(cell / 2.))
                .children((0..2).map(move |column| {
                    let phase = RING[row][column] as f32 / 6.;
                    div()
                        .size(px(cell))
                        .rounded(px(cell / 2.))
                        .bg(tint)
                        .with_animation(
                            ElementId::Name(format!("{id}-{row}-{column}").into()),
                            Animation::new(SUBAGENT_SPINNER_PERIOD).repeat(),
                            move |cell, delta| cell.opacity(spinner_opacity(delta + phase)),
                        )
                }))
        }))
}

fn tool_icon(call: &ToolCall) -> IconName {
    match call {
        ToolCall::Exec { .. } => IconName::AgentTerminal,
        ToolCall::ReadFile { .. } | ToolCall::ApplyPatch { .. } => IconName::AgentDocument,
        ToolCall::WriteFile { .. } => IconName::AgentDocumentAdd,
        ToolCall::EditFile { .. } => IconName::AgentPen,
        ToolCall::Search { .. } => IconName::AgentMagnifer,
        ToolCall::Glob { .. } => IconName::AgentFolderWithFiles,
        ToolCall::WebFetch { .. } | ToolCall::WebSearch { .. } => IconName::AgentGlobal,
        ToolCall::Todo { .. } => IconName::AgentChecklist,
        call if call.is_subagent_spawn() => IconName::AgentBot,
        ToolCall::Unknown { name, .. } if name == "Wait for agents" => IconName::AgentBot,
        ToolCall::Mcp { .. } | ToolCall::Unknown { .. } => IconName::AgentWidget,
    }
}

#[derive(Clone, PartialEq)]
struct Turn {
    user: Option<usize>,
    items: Vec<usize>,
}

fn group_turns(entries: &[Entry]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            Entry::User { .. } => turns.push(Turn {
                user: Some(index),
                items: Vec::new(),
            }),
            _ => match turns.last_mut() {
                Some(turn) => turn.items.push(index),
                None => turns.push(Turn {
                    user: None,
                    items: vec![index],
                }),
            },
        }
    }
    turns
}

fn column(content: impl IntoElement) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_center()
        .px(px(SIDE_GUTTER))
        .child(div().w_full().max_w(px(CONTENT_WIDTH)).min_w_0().child(content))
}

impl ChatView {
    fn first_row_inset(&self) -> f32 {
        if self.subagent.is_some() {
            SPACE_LG
        } else {
            SPACE_LG + FIRST_ROW_BREATHING_ROOM
        }
    }

    fn render_transcript(&self, cx: &Context<Self>) -> impl IntoElement {
        let container = self.container_bounds.clone();
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                canvas(
                    move |bounds, _, _| container.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, turn_index, window, cx| {
                        this.render_turn(turn_index, window, cx)
                    }),
                )
                .size_full(),
            )
            .when(self.show_scroll_button, |this| {
                this.child(self.render_scroll_button(cx))
            })
            .children(self.render_outline(cx))
    }

    fn render_subagent(&self, cx: &Context<Self>) -> impl IntoElement {
        let root = v_flex()
            .key_context("AgentChat")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background);
        if self.entries(cx).is_empty() {
            let message = if self.is_working(cx) {
                "Waiting for the subagent…"
            } else {
                "This subagent has no recorded activity."
            };
            return root.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(ui(13.))
                    .text_color(cx.theme().colors().text_muted)
                    .child(message),
            );
        }
        root.child(self.render_transcript(cx))
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.painted_turns.borrow_mut().clear();
        self.ensure_background_tick(cx);
        self.update_scroll_state();
        self.request_visible_diagrams(cx);
        self.refresh_path_links(cx);
        self.observe_todo(cx);
        if self.subagent.is_none()
            && window.is_window_active()
            && self.session.read(cx).metadata().unseen
        {
            let session = self.session.clone();
            cx.defer(move |cx| session.update(cx, |session, cx| session.mark_seen(cx)));
        }
        if self.subagent.is_some() {
            return self.render_subagent(cx).into_any_element();
        }
        let session = self.session.read(cx);
        let is_new_chat = session.entries().is_empty() && !session.is_working();
        let pending = session
            .pending_questions()
            .first()
            .map(|pending| pending.id().to_string());
        let drop_highlight = cx.theme().colors().drop_target_background;
        let root = v_flex()
            .key_context("AgentChat")
            .relative()
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_highlight))
            .drag_over::<DraggedSelection>(move |style, _, _, _| style.bg(drop_highlight))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_external_paths(paths, window, cx)
            }))
            .on_drop(cx.listener(|this, selection: &DraggedSelection, window, cx| {
                let paths = this.selection_paths(selection, cx);
                this.drop_paths(paths, window, cx)
            }))
            .children(self.render_lightbox(cx))
            .children(self.render_link_menu())
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(Self::stop))
            .on_action(cx.listener(Self::select_previous_command))
            .on_action(cx.listener(Self::select_next_command))
            .on_action(cx.listener(Self::accept_command))
            .on_action(cx.listener(Self::dismiss_commands))
            .on_action(cx.listener(Self::toggle_dictation))
            .on_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, _, cx| {
                this.dictation_key_up(event, cx)
            }));
        if is_new_chat {
            return root.child(self.render_new_chat(window, cx)).into_any_element();
        }
        let expanded = self.composer_is_expanded(window, cx);
        let composer_area = match pending.and_then(|id| {
            self.session
                .read(cx)
                .pending_questions()
                .iter()
                .find(|pending| pending.id() == id)
                .map(|pending| self.render_question_panel(pending, cx))
        }) {
            Some(panel) => panel,
            None => self.render_pill(expanded, DOCKED_COMPOSER_RADIUS, window, cx),
        };
        root.child(self.render_transcript(cx)).child(
            h_flex().w_full().justify_center().child(
                v_flex()
                    .w_full()
                    .max_w(px(COMPOSER_WIDTH))
                    .px(px(16.))
                    .pb(px(8.))
                    .child(self.render_status_strip(cx))
                    .children(self.render_todo_panel(cx))
                    .children(self.render_queue_panel(cx))
                    .child(composer_area)
                    .child(div().pt(px(8.)).child(self.render_footer(cx))),
            ),
        )
        .into_any_element()
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.subagent.is_some() {
            return self.focus_handle.clone();
        }
        self.composer.focus_handle(cx)
    }
}

impl EventEmitter<ItemEvent> for ChatView {}

impl Item for ChatView {
    type Event = ItemEvent;

    fn tab_content(&self, params: TabContentParams, _: &Window, cx: &App) -> AnyElement {
        let session = self.session.read(cx);
        let needs_input = self.subagent.is_none() && !session.pending_questions().is_empty();
        let is_working = self.is_working(cx);
        h_flex()
            .gap_1()
            .child(
                Label::new(self.title(cx))
                    .single_line()
                    .color(params.text_color()),
            )
            .when(needs_input, |this| {
                this.child(
                    Icon::new(IconName::AgentChatRoundLine)
                        .size(IconSize::XSmall)
                        .color(Color::Accent),
                )
            })
            .when(!needs_input && is_working, |this| {
                this.child(
                    Icon::new(IconName::LoadCircle)
                        .size(IconSize::XSmall)
                        .color(Color::Muted)
                        .with_rotate_animation(2),
                )
            })
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.title(cx)
    }

    fn tab_icon(&self, _: &Window, cx: &App) -> Option<Icon> {
        if self.subagent.is_some() {
            return Some(Icon::new(IconName::AgentBot));
        }
        Some(agent_icon(self.session.read(cx).kind()))
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        let kind = self.session.read(cx).kind();
        let kind = if self.subagent.is_some() {
            "Subagent"
        } else {
            kind.label()
        };
        Some(format!("{kind} · {}", self.title(cx)).into())
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }

    fn show_toolbar(&self) -> bool {
        true
    }

    fn handle_drop(
        &self,
        active_pane: &workspace::Pane,
        dropped: &dyn std::any::Any,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        if self.subagent.is_some() {
            return false;
        }
        let paths = self.dropped_paths(dropped, cx);
        let has_image = paths.iter().any(|path| attachments::is_image_path(path));
        // Drops on a pane edge split the pane, unless they carry an image to attach.
        if paths.is_empty() || (active_pane.drag_split_direction().is_some() && !has_image) {
            return false;
        }
        // The pane calls this while it is already updating the chat.
        let this = self.this.clone();
        window.defer(cx, move |window, cx| {
            this.update(cx, |chat, cx| chat.drop_paths(paths, window, cx))
                .log_err();
        });
        true
    }

    fn claims_drop(&self, dragged: &dyn std::any::Any, cx: &App) -> bool {
        self.subagent.is_none()
            && self
                .dropped_paths(dragged, cx)
                .iter()
                .any(|path| attachments::is_image_path(path))
    }

    fn as_searchable(
        &self,
        handle: &Entity<Self>,
        _: &App,
    ) -> Option<Box<dyn workspace::searchable::SearchableItemHandle>> {
        Some(Box::new(handle.clone()))
    }
}

impl EventEmitter<workspace::searchable::SearchEvent> for ChatView {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AcceptCommand, AgentKind, DismissCommands, SelectNextCommand, SelectPreviousCommand,
        session::tests::{new_store, run_until},
    };
    use gpui::{KeyBinding, TestAppContext, VisualTestContext};
    use project::FakeFs;

    fn bind_composer_keys(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let composer = Some("AgentComposer > Editor");
            let menu = Some("AgentComposer && command_menu > Editor");
            cx.bind_keys([
                KeyBinding::new("enter", Send, composer),
                KeyBinding::new("up", SelectPreviousCommand, menu),
                KeyBinding::new("down", SelectNextCommand, menu),
                KeyBinding::new("enter", AcceptCommand, menu),
                KeyBinding::new("escape", DismissCommands, menu),
            ]);
        });
    }

    fn composer_text(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> String {
        view.read_with(cx, |view, cx| view.composer.read(cx).text(cx))
    }

    #[test]
    fn clip_lines_wraps_only_the_visible_lines_and_counts_the_rest() {
        let text = format!("{}\nshort\n\n{}\n\n  \n", "a".repeat(25), "b".repeat(21));
        assert_eq!(
            clip_lines(&text, 10, 4),
            ClippedLines {
                lines: vec!["a".repeat(10), "a".repeat(10), "a".repeat(5), "short".into()],
                hidden: 4,
            }
        );
        assert_eq!(
            clip_lines(&text, 10, 2),
            ClippedLines {
                lines: vec!["a".repeat(10), "a".repeat(10)],
                hidden: 6,
            }
        );
        assert_eq!(clip_lines(&text, 10, usize::MAX).hidden, 0);
        assert_eq!(clip_lines("é".repeat(3).as_str(), 2, 1).lines, ["éé"]);
        assert_eq!(clip_lines("one\ntwo", usize::MAX, 1).hidden, 1);
        assert!(clip_lines(" \n\n", 10, 4).lines.is_empty());
    }

    fn matches(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<String> {
        view.read_with(cx, |view, cx| {
            view.completion_names(cx)
        })
    }

    #[gpui::test]
    async fn spawn_rows_open_the_subagent_in_its_own_tab(cx: &mut TestAppContext) {
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
        crate::session::tests::feed_subagent(&session, cx);
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let chat = workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let chat = cx.new(|cx| {
                ChatView::new(session.clone(), store.clone(), project.clone(), weak, window, cx)
            });
            workspace.add_item_to_active_pane(Box::new(chat.clone()), None, true, window, cx);
            chat
        });
        let open = |cx: &mut VisualTestContext| {
            chat.update_in(cx, |chat, window, cx| {
                let call = ToolCall::Unknown {
                    name: "Agent: scan the repo".into(),
                    input: None,
                };
                chat.open_subagent("spawn-1".into(), &call, window, cx)
            });
            cx.run_until_parked();
        };
        open(cx);
        open(cx);
        let views: Vec<Entity<ChatView>> =
            workspace.read_with(cx, |workspace, cx| workspace.items_of_type::<ChatView>(cx).collect());
        assert_eq!(views.len(), 2, "opening twice reuses the subagent tab");
        let subagent_view = views
            .into_iter()
            .find(|view| view.read_with(cx, |view, _| view.is_subagent()))
            .expect("subagent tab");
        subagent_view.read_with(cx, |view, cx| {
            assert_eq!(view.title(cx), "scan the repo");
            assert_eq!(view.entries(cx).len(), 3);
        });
    }

    #[gpui::test]
    async fn pane_keeps_focus_while_the_agent_streams(cx: &mut TestAppContext) {
        use agent_harness::AgentEvent;
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
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let chat = workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let chat = cx.new(|cx| {
                ChatView::new(session.clone(), store.clone(), project.clone(), weak, window, cx)
            });
            workspace.add_item_to_active_pane(Box::new(chat.clone()), None, true, window, cx);
            chat
        });
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        crate::session::tests::feed_events(
            &session,
            vec![AgentEvent::TextDelta { text: "First reply with some words.".into() }],
            cx,
        );
        let draw = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        };
        draw(cx);
        let focus_targets: Vec<(&str, FocusHandle)> = cx.update(|_, cx| {
            let markdown = chat.read(cx).entries(cx).iter().find_map(|entry| match entry {
                Entry::Assistant { markdown, .. } => Some(markdown.focus_handle(cx)),
                _ => None,
            });
            [("composer", Some(chat.read(cx).composer.focus_handle(cx))), ("reply", markdown)]
                .into_iter()
                .filter_map(|(name, handle)| Some((name, handle?)))
                .collect()
        });
        for (name, target) in focus_targets {
            cx.update(|window, cx| window.focus(&target, cx));
            draw(cx);
            let mut lost = Vec::new();
            for step in 0..40 {
                let events = match step % 5 {
                    0 => vec![AgentEvent::ReasoningDelta { text: format!("thinking {step} ") }],
                    1 => vec![AgentEvent::ToolCall {
                        id: format!("tool-{step}"),
                        call: ToolCall::Unknown { name: "Bash".into(), input: None },
                    }],
                    2 => vec![AgentEvent::ToolResult {
                        id: format!("tool-{}", step - 1),
                        is_error: false,
                        output: Some("ok".into()),
                        diff: None,
                    }],
                    3 => vec![AgentEvent::TextDelta { text: format!("more text {step} ") }],
                    _ => vec![AgentEvent::AssistantMessageCompleted { assistant_message_id: String::new() }],
                };
                crate::session::tests::feed_events(&session, events, cx);
                draw(cx);
                let focused = cx.update(|window, cx| pane.read(cx).has_focus(window, cx));
                if !focused {
                    lost.push(step);
                }
            }
            assert!(lost.is_empty(), "pane lost focus ({name} focused) at steps {lost:?}");
        }
    }

    #[gpui::test]
    async fn focus_left_on_a_message_scrolled_out_of_view_moves_to_the_composer(
        cx: &mut TestAppContext,
    ) {
        use agent_harness::AgentEvent;
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
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            workspace::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi, _| multi.workspace().clone());
        let chat = workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let chat = cx.new(|cx| {
                ChatView::new(session.clone(), store.clone(), project.clone(), weak, window, cx)
            });
            workspace.add_item_to_active_pane(Box::new(chat.clone()), None, true, window, cx);
            chat
        });
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        for turn in 0..60 {
            crate::session::tests::feed_user(&session, &format!("question {turn}"), cx);
            crate::session::tests::feed_events(
                &session,
                vec![AgentEvent::TextDelta {
                    text: format!("Answer {turn}.\n\nSecond paragraph.\n\nThird paragraph."),
                }],
                cx,
            );
        }
        let draw = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        };
        chat.update(cx, |chat, cx| {
            chat.list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
        draw(cx);
        let first_reply = chat.read_with(cx, |chat, cx| {
            chat.entries(cx).iter().find_map(|entry| match entry {
                Entry::Assistant { markdown, .. } => Some(markdown.focus_handle(cx)),
                _ => None,
            })
        });
        let first_reply = first_reply.expect("an assistant reply");
        let other_session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let other_chat = workspace.update_in(cx, |workspace, window, cx| {
            let weak = cx.entity().downgrade();
            let other = cx.new(|cx| {
                ChatView::new(other_session, store.clone(), project.clone(), weak, window, cx)
            });
            workspace.split_item(
                workspace::SplitDirection::Right,
                Box::new(other.clone()),
                window,
                cx,
            );
            other
        });
        draw(cx);
        cx.update(|window, cx| window.focus(&other_chat.read(cx).composer.focus_handle(cx), cx));
        draw(cx);
        cx.update(|window, cx| window.focus(&first_reply, cx));
        draw(cx);
        assert!(cx.update(|window, cx| pane.read(cx).has_focus(window, cx)));

        chat.update(cx, |chat, cx| {
            chat.list.set_follow_mode(FollowMode::Tail);
            cx.notify();
        });
        let mut frames = String::new();
        for _ in 0..8 {
            draw(cx);
            frames.push(if cx.update(|window, cx| pane.read(cx).has_focus(window, cx)) {
                '+'
            } else {
                '-'
            });
        }
        assert_eq!(frames, "++++++++", "pane focus per frame");
        let composer_focused = cx.update(|window, cx| {
            chat.read(cx).composer.focus_handle(cx).is_focused(window)
        });
        assert!(composer_focused, "focus falls back to the composer");
    }

    #[gpui::test]
    async fn model_picker_closes_on_a_click_elsewhere(cx: &mut TestAppContext) {
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
        let (view, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(session.clone(), store.clone(), project, WeakEntity::new_invalid(), window, cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| view.model_picker.show(window, cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.model_picker.is_deployed()));
        cx.simulate_click(point(px(4.), px(4.)), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(!view.read_with(cx, |view, _| view.model_picker.is_deployed()));
    }

    #[gpui::test]
    async fn at_mentions_search_project_files(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        bind_composer_keys(cx);
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/proj",
            serde_json::json!({ "src": { "main.rs": "", "lib.rs": "" }, "README.md": "" }),
        )
        .await;
        let project = project::Project::test(fs, [std::path::Path::new("/proj")], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, "/proj".into(), cx)
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(session.clone(), store.clone(), project, WeakEntity::new_invalid(), window, cx)
        });
        view.update_in(cx, |view, window, cx| {
            window.focus(&view.composer.focus_handle(cx), cx)
        });
        cx.simulate_input("look at @mai");
        cx.run_until_parked();
        assert_eq!(matches(&view, cx), ["main.rs"]);
        cx.simulate_keystrokes("enter");
        assert_eq!(composer_text(&view, cx), "look at @src/main.rs ");
    }

    #[gpui::test]
    async fn slash_menu_inserts_agent_commands_and_runs_wu_commands(cx: &mut TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        bind_composer_keys(cx);
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = new_store(directory.path(), cx);
        store.update(cx, |store, _| store.skip_plan_usage());
        let project = project::Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            ChatView::new(
                session.clone(),
                store.clone(),
                project,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        view.update_in(cx, |view, window, cx| {
            window.focus(&view.composer.focus_handle(cx), cx)
        });

        cx.simulate_input("/re");
        run_until(cx, |cx| {
            store.read_with(cx, |store, _| {
                store
                    .commands(AgentKind::Claude, directory.path())
                    .is_some_and(|catalog| catalog.loaded)
            })
        });
        assert_eq!(matches(&view, cx), ["review", "resume"]);
        cx.simulate_keystrokes("down down enter");
        assert_eq!(composer_text(&view, cx), "/review ");
        view.read_with(cx, |view, _| assert!(view.command_menu.is_none()));

        view.update_in(cx, |view, window, cx| {
            view.composer.update(cx, |editor, cx| editor.clear(window, cx))
        });
        cx.simulate_input("/");
        assert!(matches(&view, cx).contains(&"compact".to_string()));
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.command_menu.is_none()));
        cx.simulate_input("sto");
        assert_eq!(matches(&view, cx), ["stop"]);
        cx.simulate_keystrokes("enter");
        assert_eq!(composer_text(&view, cx), "");

        cx.simulate_input("/");
        let names = matches(&view, cx);
        assert!(!names.contains(&"usage".to_string()));
        assert_eq!(names.iter().filter(|name| *name == "model").count(), 1);
        cx.simulate_keystrokes("escape");
        cx.simulate_input("usage now");
        cx.simulate_keystrokes("enter");
        assert_eq!(composer_text(&view, cx), "");
        session.read_with(cx, |session, _| assert!(session.entries().is_empty()));

        cx.simulate_input("/nothing-like-this");
        assert!(matches(&view, cx).is_empty());
        cx.simulate_keystrokes("enter");
        assert_eq!(composer_text(&view, cx), "");
        session.read_with(cx, |session, _| {
            assert!(matches!(
                session.entries().first(),
                Some(Entry::User { text, .. }) if text.as_ref() == "/nothing-like-this"
            ))
        });
    }
}
