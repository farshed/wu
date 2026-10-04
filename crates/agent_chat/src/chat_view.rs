use crate::{
    Send, Stop,
    chat_style::{
        Chip, accent, flavour_seed, flavour_word, format_elapsed, gradient_spinner, hairline, ink,
        mix, page, text_faint, ui, wash,
    },
    model_picker::{ModelPicker, Selection, agent_icon},
    session::{AgentSession, AgentStore, Entry, PendingQuestion, ToolEntry, ToolStatus},
    usage_rings::{ContextCard, UsageCard, context_chip, usage_chip},
};
use agent_harness::{ToolCall, UserInputAnswer, view};
use collections::{HashMap, HashSet};
use editor::Editor;
use gpui::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, ClipboardItem, Context, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, IntoElement, ParentElement,
    PathBuilder, Render, ScrollHandle, SharedString, StyleRefinement, Styled, Subscription, Task,
    TextStyleRefinement, UnderlineStyle, WeakEntity, Window, canvas, div, point, px,
};
use markdown::{
    CodeBlockRenderer, HeadingLevelStyles, MarkdownElement, MarkdownFont, MarkdownStyle,
    parser::CodeBlockKind,
};
use project::Project;
use std::{sync::Arc, time::Duration};
use theme::ActiveTheme as _;
use ui::{
    CommonAnimationExt as _, Icon, IconName, IconSize, Label, PopoverMenu, Tooltip, prelude::*,
};
use workspace::item::{Item, ItemEvent, TabContentParams};

const CONTENT_WIDTH: f32 = 736.;
const COMPOSER_WIDTH: f32 = CONTENT_WIDTH + 32.;
const SIDE_GUTTER: f32 = 48.;
const STICK_THRESHOLD: f32 = 70.;
const SCROLL_BUTTON_THRESHOLD: f32 = 320.;
const AT_BOTTOM: f32 = 2.;
const COMPOSER_RADIUS: f32 = 26.;
const DOCKED_COMPOSER_RADIUS: f32 = 22.;
const SEND_BUTTON_SIZE: f32 = 28.;
const USER_COLLAPSED_LINES: usize = 5;
const USER_COLLAPSE_CHARS: usize = 400;
const USER_LINE_HEIGHT: f32 = 22.;
const TREE_ROW_HEIGHT: f32 = 32.;
const TREE_GUTTER: f32 = 48.;
const TREE_TRUNK_X: f32 = 12.5;
const TREE_BEND_RADIUS: f32 = 6.;
const TREE_BRANCH_END_X: f32 = 28.;
const TREE_ICON_LEFT: f32 = 32.;
const TREE_TEXT_GAP: f32 = 8.;
const OUTPUT_MAX_LINES: usize = 24;
const COPIED_FEEDBACK: Duration = Duration::from_millis(1200);
const AUTO_ADVANCE: Duration = Duration::from_millis(220);
const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const GROUP_SHIMMER: Duration = Duration::from_millis(3400);
const CODE_FONT: &str = "Geist Mono";

pub struct ChatView {
    session: Entity<AgentSession>,
    store: Entity<AgentStore>,
    project: Entity<Project>,
    composer: Entity<Editor>,
    scroll_handle: ScrollHandle,
    follow_tail: bool,
    show_scroll_button: bool,
    expanded_rows: HashSet<String>,
    group_overrides: HashMap<String, bool>,
    expanded_users: HashSet<usize>,
    selected_options: HashMap<String, Vec<String>>,
    question_pages: HashMap<String, usize>,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let kind = session.read(cx).kind();
        let flavour = flavour_seed(&session.read(cx).metadata().id);
        let composer = cx.new(|cx| {
            let mut editor = Editor::auto_height(1, 12, window, cx);
            editor.set_placeholder_text("Do anything…", window, cx);
            editor.set_text_style_refinement(TextStyleRefinement {
                font_size: Some(ui(14.).into()),
                line_height: Some(ui(22.75).into()),
                color: Some(cx.theme().colors().text),
                ..Default::default()
            });
            editor
        });
        store.update(cx, |store, cx| {
            store.ensure_models(kind, cx);
            store.refresh_plan_usage(kind, false, cx);
        });
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
        let scroll_handle = ScrollHandle::new();
        scroll_handle.scroll_to_bottom();
        let subscriptions = vec![
            cx.observe(&session, |this, _, cx| {
                if this.follow_tail {
                    this.scroll_handle.scroll_to_bottom();
                }
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }),
            cx.observe(&composer, |_, _, cx| cx.notify()),
            cx.observe(&store, |this, _, cx| {
                this.pin_default_settings(cx);
                cx.notify();
            }),
        ];
        let mut view = Self {
            session,
            store,
            project,
            composer,
            scroll_handle,
            follow_tail: true,
            show_scroll_button: false,
            expanded_rows: HashSet::default(),
            group_overrides: HashMap::default(),
            expanded_users: HashSet::default(),
            selected_options: HashMap::default(),
            question_pages: HashMap::default(),
            copied: None,
            flavour_seed: flavour,
            _copied_reset: Task::ready(()),
            _auto_advance: Task::ready(()),
            _poll_usage: poll_usage,
            _subscriptions: subscriptions,
        };
        view.pin_default_settings(cx);
        view
    }

    fn pin_default_settings(&mut self, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        if session.settings().model.is_some() {
            return;
        }
        let Some(model) = view::default_model(self.store.read(cx).models(session.kind())).cloned()
        else {
            return;
        };
        self.session.update(cx, |session, cx| {
            session.update_settings(
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

    fn title(&self, cx: &App) -> SharedString {
        let title = &self.session.read(cx).metadata().title;
        if title.is_empty() {
            "New chat".into()
        } else {
            title.clone().into()
        }
    }

    fn send(&mut self, _: &Send, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text(cx);
        if text.trim().is_empty() {
            return;
        }
        self.composer
            .update(cx, |editor, cx| editor.clear(window, cx));
        self.follow_tail = true;
        self.scroll_handle.scroll_to_bottom();
        self.session
            .update(cx, |session, cx| session.send_message(text, cx));
    }

    fn stop(&mut self, _: &Stop, _: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| session.stop(cx));
    }

    fn update_scroll_state(&mut self) {
        let offset = -self.scroll_handle.offset().y;
        let max_offset = self.scroll_handle.max_offset().y;
        let distance = f32::from(max_offset - offset).max(0.);
        self.follow_tail = distance <= STICK_THRESHOLD;
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
        style.paragraph_spacing = px(12.);
        style.inline_code = TextStyleRefinement {
            font_family: Some(CODE_FONT.into()),
            color: Some(accent),
            background_color: Some(accent.opacity(0.22)),
            ..Default::default()
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
            font_family: Some(CODE_FONT.into()),
            font_size: Some(px(12.5).into()),
            line_height: Some(px(18.).into()),
            ..Default::default()
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
        style.selection_background_color = accent.opacity(0.35);
        style
    }

    fn thought_style(window: &Window, cx: &App) -> MarkdownStyle {
        let mut style = Self::message_style(window, cx);
        style.base_text_style.font_size = ui(12.).into();
        style.base_text_style.line_height = ui(18.).into();
        style.base_text_style.color = text_faint(cx);
        style.paragraph_spacing = px(6.);
        style
    }

    fn code_block_renderer(&self, message_key: usize, cx: &Context<Self>) -> CodeBlockRenderer {
        let view: WeakEntity<Self> = cx.entity().downgrade();
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
                div()
                    .w_full()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(ink(0.035, cx))
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .h(px(28.))
                            .pl(px(12.))
                            .pr(px(5.))
                            .justify_between()
                            .border_b_1()
                            .border_color(colors.border)
                            .bg(ink(0.02, cx))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.text_muted)
                                    .child(language),
                            )
                            .child(
                                h_flex()
                                    .id(ElementId::Name(key.clone()))
                                    .h(px(22.))
                                    .px(px(6.))
                                    .gap(px(4.))
                                    .rounded(px(5.))
                                    .cursor_pointer()
                                    .text_size(px(10.5))
                                    .text_color(colors.text_muted)
                                    .hover(|style| style.bg(ink(0.08, cx)))
                                    .child(
                                        Icon::new(if is_copied {
                                            IconName::AgentCheck
                                        } else {
                                            IconName::AgentCopy
                                        })
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                    )
                                    .child(if is_copied { "Copied" } else { "Copy" })
                                    .on_click(move |_, _, cx| {
                                        let key = key.clone();
                                        let code = code.clone();
                                        view.update(cx, |this, cx| this.copy(key, code, cx)).ok();
                                    }),
                            ),
                    )
            }),
            transform: None,
        }
    }

    fn render_turns(&self, window: &Window, cx: &Context<Self>) -> Vec<AnyElement> {
        let session = self.session.read(cx);
        let entries = session.entries();
        let mut turns: Vec<(Option<usize>, Vec<usize>)> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            match entry {
                Entry::User { .. } => turns.push((Some(index), Vec::new())),
                _ => {
                    if turns.is_empty() {
                        turns.push((None, Vec::new()));
                    }
                    if let Some((_, items)) = turns.last_mut() {
                        items.push(index);
                    }
                }
            }
        }
        let turn_count = turns.len();
        turns
            .into_iter()
            .enumerate()
            .map(|(turn_index, (user, items))| {
                let is_last_turn = turn_index + 1 == turn_count;
                v_flex()
                    .w_full()
                    .gap(px(16.))
                    .when_some(user, |this, index| {
                        this.child(self.render_user_message(index, cx))
                    })
                    .when(
                        !items.is_empty() || (is_last_turn && session.is_working()),
                        |this| {
                            this.child(self.render_assistant_turn(
                                turn_index,
                                &items,
                                is_last_turn,
                                window,
                                cx,
                            ))
                        },
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn hover_strip(
        &self,
        group: SharedString,
        at: i64,
        copy_key: SharedString,
        copy_text: String,
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
        let is_copied = self.copied.as_ref() == Some(&copy_key);
        h_flex()
            .h(px(32.))
            .pt(px(8.))
            .gap(px(8.))
            .when(align_end, |this| this.justify_end())
            .visible_on_hover(group)
            .when_some(timestamp, |this, timestamp| {
                this.child(
                    div()
                        .text_size(ui(12.))
                        .text_color(cx.theme().colors().text_muted.opacity(0.55))
                        .child(timestamp),
                )
            })
            .child(
                div()
                    .id(ElementId::Name(copy_key.clone()))
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|style| style.bg(ink(0.08, cx)))
                    .tooltip(Tooltip::text("Copy message"))
                    .child(
                        Icon::new(if is_copied {
                            IconName::AgentCheck
                        } else {
                            IconName::AgentCopy
                        })
                        .size(IconSize::Small)
                        .color(Color::Muted),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy(copy_key.clone(), copy_text.clone(), cx)
                    })),
            )
    }

    fn render_user_message(&self, index: usize, cx: &Context<Self>) -> AnyElement {
        let Some(Entry::User { text, at }) = self.session.read(cx).entries().get(index) else {
            return div().into_any_element();
        };
        let text = text.clone();
        let line_count = text.lines().count();
        let collapsible =
            line_count > USER_COLLAPSED_LINES || text.chars().count() > USER_COLLAPSE_CHARS;
        let expanded = self.expanded_users.contains(&index);
        let collapsed = collapsible && !expanded;
        let group: SharedString = format!("agent-user-{index}").into();
        let colors = cx.theme().colors();
        v_flex()
            .group(group.clone())
            .w_full()
            .items_end()
            .child(
                v_flex()
                    .min_w_0()
                    .max_w(px(CONTENT_WIDTH * 0.8))
                    .px(px(16.))
                    .py(px(10.))
                    .rounded(px(16.))
                    .bg(wash(0.08, cx))
                    .text_size(ui(14.))
                    .line_height(ui(USER_LINE_HEIGHT))
                    .text_color(colors.text)
                    .map(|this| {
                        if collapsed {
                            this.child(
                                div()
                                    .max_h(ui(USER_LINE_HEIGHT * USER_COLLAPSED_LINES as f32))
                                    .overflow_hidden()
                                    .child(text.clone()),
                            )
                            .child(div().h(ui(USER_LINE_HEIGHT)).child("..."))
                        } else {
                            this.child(text.clone())
                        }
                    })
                    .when(collapsible, |this| {
                        this.child(
                            h_flex()
                                .id(("agent-user-expand", index))
                                .mt(px(8.))
                                .gap(px(5.))
                                .cursor_pointer()
                                .text_color(colors.text_muted)
                                .hover(|style| style.text_color(colors.text))
                                .child(if expanded { "Show less" } else { "Show more" })
                                .child(
                                    Icon::new(if expanded {
                                        IconName::AgentArrowUp
                                    } else {
                                        IconName::AgentArrowDown
                                    })
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.expanded_users.remove(&index) {
                                        this.expanded_users.insert(index);
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(self.hover_strip(
                group,
                *at,
                format!("copy-user-{index}").into(),
                text.to_string(),
                true,
                cx,
            ))
            .into_any_element()
    }

    fn render_assistant_turn(
        &self,
        turn_index: usize,
        items: &[usize],
        is_last_turn: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let session = self.session.read(cx);
        let entries = session.entries();
        let is_working = session.is_working();
        let last_entry = entries.len().saturating_sub(1);
        let group: SharedString = format!("agent-turn-{turn_index}").into();
        let mut children: Vec<AnyElement> = Vec::new();
        let mut cursor = 0;
        while cursor < items.len() {
            let index = items[cursor];
            match &entries[index] {
                Entry::Tool(_) | Entry::Thinking(_) => {
                    let end = items[cursor..]
                        .iter()
                        .position(|index| {
                            !matches!(entries[*index], Entry::Tool(_) | Entry::Thinking(_))
                        })
                        .map_or(items.len(), |offset| cursor + offset);
                    let group_items = &items[cursor..end];
                    let is_live = is_working && group_items.contains(&last_entry);
                    children.push(self.render_tool_group(group_items, is_live, window, cx));
                    cursor = end;
                }
                Entry::Assistant { markdown, .. } => {
                    children.push(
                        div()
                            .w_full()
                            .child(
                                MarkdownElement::new(
                                    markdown.clone(),
                                    Self::message_style(window, cx),
                                )
                                .code_block_renderer(self.code_block_renderer(index, cx)),
                            )
                            .into_any_element(),
                    );
                    cursor += 1;
                }
                Entry::Notice { text, is_error } => {
                    children.push(if *is_error {
                        self.render_error(index, text.clone(), cx)
                    } else {
                        div()
                            .text_size(ui(12.))
                            .text_color(cx.theme().colors().text_muted)
                            .child(text.clone())
                            .into_any_element()
                    });
                    cursor += 1;
                }
                Entry::User { .. } => cursor += 1,
            }
        }

        let settled = !(is_working && is_last_turn);
        let message_text: Vec<String> = items
            .iter()
            .filter_map(|index| match &entries[*index] {
                Entry::Assistant { markdown, .. } => Some(markdown.read(cx).source().to_string()),
                _ => None,
            })
            .collect();
        let last_at = items
            .iter()
            .rev()
            .find_map(|index| match &entries[*index] {
                Entry::Assistant { at, .. } => Some(*at),
                _ => None,
            })
            .unwrap_or_default();

        v_flex()
            .group(group.clone())
            .w_full()
            .gap(px(12.))
            .children(children)
            .when(is_working && is_last_turn, |this| {
                this.child(self.render_working_trailer(cx))
            })
            .when(settled && !message_text.is_empty(), |this| {
                this.child(div().mt(px(-12.)).child(self.hover_strip(
                    group,
                    last_at,
                    format!("copy-turn-{turn_index}").into(),
                    message_text.join("\n\n"),
                    false,
                    cx,
                )))
            })
            .into_any_element()
    }

    fn render_working_trailer(&self, cx: &Context<Self>) -> AnyElement {
        let session = self.session.read(cx);
        let elapsed = session
            .working_since()
            .map(|since| since.elapsed().as_secs())
            .unwrap_or(0);
        let colors = cx.theme().colors();
        h_flex()
            .mt(px(4.))
            .gap(px(8.))
            .child(gradient_spinner(
                format!("agent-working-{}", cx.entity_id()).into(),
                2.5,
            ))
            .child(
                div()
                    .text_size(ui(12.))
                    .text_color(colors.text_muted)
                    .child(format!("{}…", flavour_word(self.flavour_seed, elapsed))),
            )
            .child(
                div()
                    .mt(px(1.))
                    .text_size(ui(11.))
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
            .child(
                v_flex()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(danger.opacity(0.16))
                    .bg(danger.opacity(0.05))
                    .px(px(10.))
                    .py(px(8.))
                    .gap(px(6.))
                    .text_size(ui(12.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .size(px(20.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.))
                                    .bg(danger.opacity(0.12))
                                    .child(
                                        Icon::new(IconName::AgentDangerTriangle)
                                            .size(IconSize::XSmall)
                                            .color(Color::Custom(danger_muted)),
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
                                    .size(px(20.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(danger.opacity(0.12)))
                                    .child(
                                        Icon::new(if is_copied {
                                            IconName::AgentCheck
                                        } else {
                                            IconName::AgentCopy
                                        })
                                        .size(IconSize::XSmall)
                                        .color(Color::Custom(danger_muted)),
                                    )
                                    .on_click(cx.listener({
                                        let text = text.to_string();
                                        move |this, _, _, cx| {
                                            this.copy(copy_key.clone(), text.clone(), cx)
                                        }
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_color(cx.theme().colors().text.opacity(0.8))
                            .child(text),
                    ),
            )
            .into_any_element()
    }

    fn group_summary(&self, items: &[usize], cx: &App) -> String {
        let entries = self.session.read(cx).entries();
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
        let thought = match thoughts {
            0 => None,
            1 => Some("thought process".to_string()),
            count => Some(format!("thought {count} times")),
        };
        match (tools.is_empty(), thought) {
            (true, Some(thought)) => {
                let mut chars = thought.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            }
            (false, Some(thought)) => format!("{} · {thought}", view::tool_group_summary(&tools)),
            (_, None) => view::tool_group_summary(&tools),
        }
    }

    fn render_tool_group(
        &self,
        items: &[usize],
        is_live: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entries = self.session.read(cx).entries();
        let group_key = match items.first().map(|index| &entries[*index]) {
            Some(Entry::Tool(tool)) => format!("tools-{}", tool.id),
            _ => format!("tools-at-{}", items.first().copied().unwrap_or_default()),
        };
        let open = self
            .group_overrides
            .get(&group_key)
            .copied()
            .unwrap_or(is_live);
        let summary = self.group_summary(items, cx);
        let colors = cx.theme().colors();
        let muted = colors.text_muted;
        let text = colors.text;
        let header_key: SharedString = format!("{group_key}-header").into();
        let header = h_flex()
            .id(ElementId::Name(header_key))
            .h(px(26.))
            .gap(px(6.))
            .pr(px(4.))
            .cursor_pointer()
            .text_size(ui(12.))
            .line_height(ui(18.))
            .text_color(muted)
            .hover(|style| style.text_color(text))
            .child(
                div().relative().w(px(22.)).h(px(18.)).child(
                    div().absolute().left(px(5.5)).top(px(2.)).child(
                        Icon::new(if open {
                            IconName::AgentArrowDown
                        } else {
                            IconName::AgentArrowRight
                        })
                        .size(IconSize::Small)
                        .color(Color::Muted),
                    ),
                ),
            )
            .child(if is_live {
                div()
                    .child(summary)
                    .with_animation(
                        ElementId::Name(format!("{group_key}-shimmer").into()),
                        Animation::new(GROUP_SHIMMER).repeat(),
                        move |label, delta| {
                            let amount = 0.5 - 0.5 * (delta * std::f32::consts::TAU).cos();
                            label.text_color(mix(muted, text, amount))
                        },
                    )
                    .into_any_element()
            } else {
                div().child(summary).into_any_element()
            })
            .on_click(cx.listener({
                move |this, _, _, cx| {
                    this.group_overrides.insert(group_key.clone(), !open);
                    cx.notify();
                }
            }));

        let row_count = items.len();
        v_flex()
            .w_full()
            .child(header)
            .when(open, |this| {
                this.child(v_flex().pt(px(2.)).children(items.iter().enumerate().map(
                    |(position, index)| {
                        let is_last = position + 1 == row_count;
                        self.render_tree_row(*index, is_last, is_live, window, cx)
                    },
                )))
            })
            .into_any_element()
    }

    fn render_tree_row(
        &self,
        index: usize,
        is_last: bool,
        is_live: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let session = self.session.read(cx);
        let entries = session.entries();
        let colors = cx.theme().colors();
        let danger = cx.theme().status().error;
        let line_color = hairline(0.12, cx);
        let (key, icon, label, detail, failed, body): (
            String,
            IconName,
            SharedString,
            Option<String>,
            bool,
            Option<AnyElement>,
        ) = match &entries[index] {
            Entry::Tool(tool) => {
                let (label, detail) = view::tool_chip_content(&tool.call);
                (
                    format!("tool-{}", tool.id),
                    tool_icon(&tool.call),
                    label.into(),
                    (!detail.is_empty()).then_some(detail),
                    tool.status == ToolStatus::Failed,
                    Some(self.render_tool_detail(tool, cx)),
                )
            }
            Entry::Thinking(markdown) => (
                format!("thought-{index}"),
                IconName::AgentChatRoundLine,
                "Thought process".into(),
                None,
                false,
                Some(
                    div()
                        .py(px(6.))
                        .child(MarkdownElement::new(
                            markdown.clone(),
                            Self::thought_style(window, cx),
                        ))
                        .into_any_element(),
                ),
            ),
            _ => return div().into_any_element(),
        };
        let streaming_thought =
            is_live && index + 1 == entries.len() && matches!(entries[index], Entry::Thinking(_));
        let expanded = self.expanded_rows.contains(&key) || streaming_thought;
        let row_color = if failed { danger } else { colors.text_muted };
        let hover_group: SharedString = format!("{key}-row").into();
        let continues = !is_last;
        v_flex()
            .w_full()
            .child(
                div()
                    .id(ElementId::Name(hover_group.clone()))
                    .group(hover_group.clone())
                    .relative()
                    .w_full()
                    .h(px(TREE_ROW_HEIGHT))
                    .cursor_pointer()
                    .child(tree_lines(continues || expanded, line_color))
                    .child(
                        div()
                            .absolute()
                            .left(px(TREE_ICON_LEFT))
                            .top(px((TREE_ROW_HEIGHT - 16.) / 2.))
                            .child(
                                Icon::new(icon)
                                    .size(IconSize::Medium)
                                    .color(Color::Custom(row_color)),
                            ),
                    )
                    .child(
                        h_flex()
                            .size_full()
                            .pl(px(TREE_GUTTER + TREE_TEXT_GAP))
                            .gap(px(8.))
                            .text_size(ui(12.))
                            .line_height(ui(18.))
                            .text_color(row_color)
                            .group_hover(hover_group.clone(), |style| {
                                if failed {
                                    style
                                } else {
                                    style.text_color(colors.text)
                                }
                            })
                            .child(div().flex_none().child(label))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .when_some(detail, |this, detail| this.child(detail)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(18.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .visible_on_hover(hover_group)
                                    .child(
                                        Icon::new(if expanded {
                                            IconName::AgentArrowDown
                                        } else {
                                            IconName::AgentArrowRight
                                        })
                                        .size(IconSize::XSmall)
                                        .color(Color::Custom(text_faint(cx))),
                                    ),
                            ),
                    )
                    .on_click(cx.listener({
                        move |this, _, _, cx| {
                            if !this.expanded_rows.remove(&key) {
                                this.expanded_rows.insert(key.clone());
                            }
                            cx.notify();
                        }
                    })),
            )
            .when(expanded, |this| {
                this.child(
                    div()
                        .relative()
                        .w_full()
                        .pl(px(TREE_GUTTER + TREE_TEXT_GAP))
                        .when(continues, |this| this.child(trunk_line(line_color)))
                        .children(body),
                )
            })
            .into_any_element()
    }

    fn render_tool_detail(&self, tool: &ToolEntry, cx: &Context<Self>) -> AnyElement {
        let call_text = view::tool_call_text(&tool.call);
        let output_lines: Vec<String> = tool
            .output
            .as_ref()
            .map(|output| output.lines().map(str::to_string).collect())
            .unwrap_or_default();
        let hidden = output_lines.len().saturating_sub(OUTPUT_MAX_LINES);
        v_flex()
            .py(px(6.))
            .font_family(CODE_FONT)
            .text_size(px(12.))
            .line_height(px(18.))
            .text_color(text_faint(cx))
            .child(div().child(call_text))
            .when(!output_lines.is_empty(), |this| {
                this.child(
                    div()
                        .pt(px(6.))
                        .children(
                            output_lines
                                .into_iter()
                                .take(OUTPUT_MAX_LINES)
                                .map(|line| div().truncate().child(line)),
                        )
                        .when(hidden > 0, |this| {
                            this.child(div().child(format!("… {hidden} more lines")))
                        }),
                )
            })
            .into_any_element()
    }

    fn render_model_chip(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let kind = session.kind();
        let settings = session.settings();
        let selection = Selection::resolve(self.store.read(cx).models(kind), settings);
        let model_label = selection.model_label(settings, kind);
        let effort = selection.reasoning.map(view::reasoning_label);
        let fast = selection.fast_on();
        let colors = cx.theme().colors();
        let session = self.session.clone();
        let store = self.store.clone();
        PopoverMenu::new(SharedString::from(format!(
            "agent-model-picker-{}",
            cx.entity_id()
        )))
        .trigger(Chip::new(
            "agent-model-chip",
            8.,
            h_flex()
                .h(px(32.))
                .max_w(px(248.))
                .px(px(6.))
                .gap(px(6.))
                .text_size(ui(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.text.opacity(0.9))
                .child(agent_icon(kind).size(IconSize::Medium))
                .child(div().min_w_0().truncate().child(model_label))
                .when_some(effort, |this, effort| {
                    this.child(
                        div()
                            .flex_none()
                            .text_color(colors.text_muted.opacity(0.7))
                            .child(effort),
                    )
                })
                .when(fast, |this| {
                    this.child(
                        Icon::new(IconName::AgentFastFilled)
                            .size(IconSize::XSmall)
                            .color(Color::Accent),
                    )
                }),
        ))
        .menu(move |window, cx| {
            let session = session.clone();
            let store = store.clone();
            Some(cx.new(|cx| ModelPicker::new(session, store, window, cx)))
        })
        .anchor(Anchor::BottomRight)
        .attach(Anchor::TopRight)
        .offset(point(px(0.), px(-6.)))
    }

    fn render_send_button(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let is_working = self.session.read(cx).is_working();
        let has_text = !self.composer.read(cx).text(cx).trim().is_empty();
        let focus_handle = self.composer.focus_handle(cx);
        let plate = page(cx);
        let circle = |id: &'static str| {
            div()
                .id(id)
                .size(px(SEND_BUTTON_SIZE))
                .flex_none()
                .rounded_full()
                .bg(colors.text)
                .flex()
                .items_center()
                .justify_center()
        };
        if is_working && !has_text {
            return circle("agent-stop")
                .cursor_pointer()
                .hover(|style| style.opacity(0.85))
                .tooltip(move |_, cx| Tooltip::for_action_in("Stop", &Stop, &focus_handle, cx))
                .on_click(cx.listener(|this, _, window, cx| this.stop(&Stop, window, cx)))
                .child(div().size(px(11.)).rounded(px(3.)).bg(plate))
                .into_any_element();
        }
        circle("agent-send")
            .when(!has_text, |this| this.opacity(0.35))
            .when(has_text, |this| {
                this.cursor_pointer()
                    .hover(|style| style.opacity(0.85))
                    .on_click(cx.listener(|this, _, window, cx| this.send(&Send, window, cx)))
            })
            .tooltip(move |_, cx| {
                Tooltip::for_action_in(
                    if is_working {
                        "Queue message"
                    } else {
                        "Send message"
                    },
                    &Send,
                    &focus_handle,
                    cx,
                )
            })
            .child(
                Icon::new(IconName::AgentSend)
                    .size(IconSize::Small)
                    .color(Color::Custom(plate)),
            )
            .into_any_element()
    }

    fn composer_is_expanded(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.composer.read(cx).text(cx);
        if text.contains('\n') {
            return true;
        }
        self.composer.update(cx, |editor, cx| {
            editor
                .snapshot(window, cx)
                .display_snapshot
                .max_point()
                .row()
                .0
                > 0
        })
    }

    fn render_pill(&self, expanded: bool, radius: f32, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let pill = div()
            .key_context("AgentComposer")
            .w_full()
            .rounded(px(radius))
            .border_1()
            .border_color(colors.border)
            .bg(colors.element_background);
        if expanded {
            pill.flex()
                .flex_col()
                .min_h(px(120.))
                .child(
                    div()
                        .px(px(16.))
                        .pt(px(16.))
                        .pb(px(4.))
                        .min_h(px(76.))
                        .max_h(px(260.))
                        .child(self.composer.clone()),
                )
                .child(
                    h_flex()
                        .h(px(42.))
                        .pt(px(2.))
                        .pb(px(8.))
                        .px(px(12.))
                        .justify_end()
                        .gap(px(2.))
                        .child(self.render_model_chip(cx))
                        .child(div().pl(px(6.)).child(self.render_send_button(cx))),
                )
                .into_any_element()
        } else {
            pill.flex()
                .items_center()
                .h(px(49.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(px(16.))
                        .pr(px(8.))
                        .child(self.composer.clone()),
                )
                .child(self.render_model_chip(cx))
                .child(
                    div()
                        .pl(px(8.))
                        .pr(px(10.))
                        .child(self.render_send_button(cx)),
                )
                .into_any_element()
        }
    }

    fn branch_name(&self, cx: &App) -> Option<SharedString> {
        let repository = self.project.read(cx).active_repository(cx)?;
        let repository = repository.read(cx);
        Some(
            repository
                .branch
                .as_ref()
                .map(|branch| SharedString::from(branch.name().to_string()))
                .unwrap_or_else(|| "No ref".into()),
        )
    }

    fn footer_label(icon: IconName, label: SharedString, cx: &App) -> impl IntoElement {
        let color = cx.theme().colors().text_muted.opacity(0.6);
        h_flex()
            .h(px(20.))
            .max_w(px(160.))
            .px(px(8.))
            .gap(px(6.))
            .text_size(ui(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(color)
            .child(
                Icon::new(icon)
                    .size(IconSize::XSmall)
                    .color(Color::Custom(color)),
            )
            .child(div().min_w_0().truncate().child(label))
    }

    fn render_checkout(&self, cx: &App) -> impl IntoElement {
        h_flex()
            .px(px(10.))
            .gap(px(4.))
            .when_some(self.branch_name(cx), |this, branch| {
                this.child(Self::footer_label(
                    IconName::AgentFolder,
                    "Local checkout".into(),
                    cx,
                ))
                .child(Self::footer_label(IconName::AgentGitBranch, branch, cx))
            })
    }

    fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let kind = session.kind();
        let context = session.context();
        let context_fraction = context
            .and_then(|context| context.fraction())
            .map(|fraction| fraction as f32);
        let usage_fraction = self
            .store
            .read(cx)
            .plan_usage(kind)
            .and_then(|state| state.usage.as_ref())
            .and_then(|usage| usage.used_fraction());
        let store = self.store.clone();
        h_flex()
            .h(px(24.))
            .justify_between()
            .child(self.render_checkout(cx))
            .child(
                h_flex()
                    .pl(px(4.))
                    .pr(px(10.))
                    .gap(px(4.))
                    .child(
                        PopoverMenu::new(SharedString::from(format!(
                            "agent-plan-usage-{}",
                            cx.entity_id()
                        )))
                        .trigger(Chip::new(
                            "agent-plan-usage-chip",
                            6.,
                            usage_chip(usage_fraction, cx),
                        ))
                        .menu(move |_, cx| {
                            let store = store.clone();
                            Some(cx.new(|cx| UsageCard::new(store, kind, cx)))
                        })
                        .anchor(Anchor::BottomRight)
                        .attach(Anchor::TopRight)
                        .offset(point(px(0.), px(-4.))),
                    )
                    .child(
                        PopoverMenu::new(SharedString::from(format!(
                            "agent-context-usage-{}",
                            cx.entity_id()
                        )))
                        .trigger(Chip::new(
                            "agent-context-chip",
                            6.,
                            context_chip(context_fraction, cx),
                        ))
                        .menu(move |_, cx| Some(cx.new(|cx| ContextCard::new(context, cx))))
                        .anchor(Anchor::BottomRight)
                        .attach(Anchor::TopRight)
                        .offset(point(px(0.), px(-4.))),
                    ),
            )
    }

    fn render_status_strip(&self, cx: &Context<Self>) -> impl IntoElement {
        let failed = self.session.read(cx).run_failed();
        h_flex()
            .h(px(24.))
            .px(px(24.))
            .text_size(ui(11.))
            .when(failed, |this| {
                this.text_color(cx.theme().status().error)
                    .child("Run failed")
            })
    }

    fn render_scroll_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        h_flex()
            .absolute()
            .bottom(px(6.))
            .left_0()
            .right(px(10.))
            .justify_center()
            .child(
                h_flex()
                    .id("agent-scroll-to-bottom")
                    .h(px(30.))
                    .pl(px(11.))
                    .pr(px(13.))
                    .gap(px(6.))
                    .rounded_full()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.elevated_surface_background)
                    .shadow_md()
                    .cursor_pointer()
                    .text_size(ui(13.))
                    .hover(|style| style.bg(colors.element_hover))
                    .child(div().text_color(colors.text_muted).child("↓"))
                    .child(div().text_color(colors.text).child("Scroll to bottom"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.follow_tail = true;
                        this.show_scroll_button = false;
                        this.scroll_handle.scroll_to_bottom();
                        cx.notify();
                    })),
            )
    }

    fn pick_option(
        &mut self,
        pending_id: SharedString,
        question_id: String,
        option: String,
        multi_select: bool,
        question_count: usize,
        cx: &mut Context<Self>,
    ) {
        let labels = self.selected_options.entry(question_id).or_default();
        if multi_select {
            if let Some(position) = labels.iter().position(|label| *label == option) {
                labels.remove(position);
            } else {
                labels.push(option);
            }
            cx.notify();
            return;
        }
        *labels = vec![option];
        cx.notify();
        self._auto_advance = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTO_ADVANCE).await;
            this.update(cx, |this, cx| {
                let page = this
                    .question_pages
                    .get(pending_id.as_ref())
                    .copied()
                    .unwrap_or(0);
                if page + 1 < question_count {
                    this.question_pages.insert(pending_id.to_string(), page + 1);
                    cx.notify();
                } else {
                    this.submit_answers(&pending_id, cx);
                }
            })
            .ok();
        });
    }

    fn submit_answers(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(pending) = self
            .session
            .read(cx)
            .pending_questions()
            .iter()
            .find(|pending| pending.id() == id)
        else {
            return;
        };
        let answers: Vec<UserInputAnswer> = pending
            .questions
            .iter()
            .map(|question| UserInputAnswer {
                question_id: question.id.clone(),
                labels: self
                    .selected_options
                    .remove(&question.id)
                    .unwrap_or_default(),
            })
            .collect();
        self.question_pages.remove(id);
        self.session
            .update(cx, |session, cx| session.answer(id, answers, cx));
    }

    fn render_option_row(
        &self,
        key: SharedString,
        number: usize,
        label: SharedString,
        picked: bool,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = cx.theme().colors();
        let hover = ink(0.06, cx);
        h_flex()
            .id(ElementId::Name(key))
            .px(px(14.))
            .py(px(10.))
            .gap(px(12.))
            .rounded(px(12.))
            .border_1()
            .border_color(if picked {
                ink(0.16, cx)
            } else {
                gpui::transparent_black()
            })
            .bg(if picked {
                ink(0.09, cx)
            } else {
                ink(0.025, cx)
            })
            .when(!picked, |this| this.hover(move |style| style.bg(hover)))
            .cursor_pointer()
            .child(
                div()
                    .flex_none()
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(if picked { ink(0.16, cx) } else { ink(0.05, cx) })
                    .text_size(ui(11.))
                    .text_color(colors.text_muted)
                    .child(number.to_string()),
            )
            .child(
                div()
                    .text_size(ui(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if picked {
                        colors.text
                    } else {
                        colors.text.opacity(0.9)
                    })
                    .child(label),
            )
    }

    fn render_question_panel(&self, pending: &PendingQuestion, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let pending_id: SharedString = pending.id().to_string().into();
        let count = pending.questions.len();
        let page_index = self
            .question_pages
            .get(pending_id.as_ref())
            .copied()
            .unwrap_or(0)
            .min(count.saturating_sub(1));
        let Some(question) = pending.questions.get(page_index) else {
            return div().into_any_element();
        };
        let is_permission = pending.is_permission();
        let selected = self
            .selected_options
            .get(&question.id)
            .cloned()
            .unwrap_or_default();
        let options: Vec<AnyElement> = if is_permission {
            [
                ("Allow", Some(true)),
                ("Allow all in this chat", None),
                ("Deny", Some(false)),
            ]
            .into_iter()
            .enumerate()
            .map(|(position, (label, allow))| {
                let pending_id = pending_id.clone();
                self.render_option_row(
                    format!("permission-{pending_id}-{position}").into(),
                    position + 1,
                    label.into(),
                    false,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.session.update(cx, |session, cx| match allow {
                        Some(allow) => session.answer_permission(&pending_id, allow, cx),
                        None => session.set_auto_approve(true, cx),
                    })
                }))
                .into_any_element()
            })
            .collect()
        } else {
            question
                .options
                .iter()
                .enumerate()
                .map(|(position, option)| {
                    let picked = selected.contains(option);
                    let pending_id = pending_id.clone();
                    let question_id = question.id.clone();
                    let option = option.clone();
                    let multi_select = question.multi_select;
                    self.render_option_row(
                        format!("option-{}-{position}", question.id).into(),
                        position + 1,
                        option.clone().into(),
                        picked,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pick_option(
                            pending_id.clone(),
                            question_id.clone(),
                            option.clone(),
                            multi_select,
                            count,
                            cx,
                        )
                    }))
                    .into_any_element()
                })
                .collect()
        };
        let answered = !selected.is_empty();
        let is_last_page = page_index + 1 >= count;
        let header = if is_permission {
            "Permission".to_string()
        } else {
            question.header.clone()
        };
        v_flex()
            .w_full()
            .rounded(px(COMPOSER_RADIUS))
            .border_1()
            .border_color(colors.border)
            .bg(colors.element_background)
            .child(
                v_flex()
                    .px(px(16.))
                    .pt(px(16.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(ui(10.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(colors.text_muted.opacity(0.6))
                                    .child(header.to_uppercase()),
                            )
                            .when(count > 1, |this| {
                                this.child(
                                    div()
                                        .h(px(20.))
                                        .px(px(6.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(6.))
                                        .bg(ink(0.06, cx))
                                        .text_size(ui(10.))
                                        .text_color(colors.text_muted)
                                        .child(format!("{} of {count}", page_index + 1)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .text_size(ui(15.))
                            .line_height(ui(20.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text)
                            .child(question.question.clone()),
                    )
                    .when(question.multi_select, |this| {
                        this.child(
                            div()
                                .mt(px(4.))
                                .text_size(ui(12.))
                                .text_color(colors.text_muted.opacity(0.65))
                                .child("Select one or more options."),
                        )
                    })
                    .child(v_flex().mt(px(12.)).gap(px(4.)).children(options)),
            )
            .child(
                h_flex()
                    .px(px(16.))
                    .pt(px(4.))
                    .pb(px(16.))
                    .justify_between()
                    .child(if page_index > 0 {
                        let pending_id = pending_id.clone();
                        div()
                            .id("agent-question-back")
                            .px(px(12.))
                            .py(px(6.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .text_size(ui(13.))
                            .text_color(colors.text_muted)
                            .hover(|style| style.bg(ink(0.06, cx)).text_color(colors.text))
                            .child("Back")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.question_pages
                                    .insert(pending_id.to_string(), page_index - 1);
                                cx.notify();
                            }))
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .when(!is_permission, |this| {
                        let pending_id = pending_id.clone();
                        this.child(
                            div()
                                .id("agent-question-next")
                                .px(px(16.))
                                .py(px(6.))
                                .rounded(px(8.))
                                .bg(colors.text)
                                .text_size(ui(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(page(cx))
                                .when(!answered, |this| this.opacity(0.4))
                                .when(answered, |this| {
                                    this.cursor_pointer()
                                        .hover(|style| style.opacity(0.9))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if is_last_page {
                                                this.submit_answers(&pending_id, cx);
                                            } else {
                                                this.question_pages
                                                    .insert(pending_id.to_string(), page_index + 1);
                                                cx.notify();
                                            }
                                        }))
                                })
                                .child(if is_last_page { "Submit" } else { "Next" }),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_new_chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let project_name: SharedString = self
            .session
            .read(cx)
            .metadata()
            .cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned().into())
            .unwrap_or_default();
        let colors = cx.theme().colors();
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
                        h_flex().h(px(20.)).px(px(26.)).justify_end().child(
                            h_flex()
                                .h(px(20.))
                                .px(px(8.))
                                .gap(px(6.))
                                .rounded(px(6.))
                                .text_size(ui(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors.text_muted.opacity(0.7))
                                .child(
                                    Icon::new(IconName::AgentFolder)
                                        .size(IconSize::XSmall)
                                        .color(Color::Custom(colors.text_muted.opacity(0.7))),
                                )
                                .child(project_name),
                        ),
                    )
                    .child(self.render_pill(true, COMPOSER_RADIUS, cx))
                    .child(h_flex().h(px(24.)).child(self.render_checkout(cx))),
            )
            .into_any_element()
    }
}

fn tree_lines(continues: bool, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let x = bounds.left() + px(TREE_TRUNK_X);
            let top = bounds.top();
            let middle = top + bounds.size.height / 2.;
            let radius = px(TREE_BEND_RADIUS);
            let mut elbow = PathBuilder::stroke(px(1.));
            elbow.move_to(point(x, top));
            elbow.line_to(point(x, middle - radius));
            elbow.curve_to(point(x + radius, middle), point(x, middle));
            elbow.line_to(point(bounds.left() + px(TREE_BRANCH_END_X), middle));
            if let Ok(path) = elbow.build() {
                window.paint_path(path, color);
            }
            if continues {
                let mut trunk = PathBuilder::stroke(px(1.));
                trunk.move_to(point(x, middle - radius));
                trunk.line_to(point(x, bounds.bottom()));
                if let Ok(path) = trunk.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(TREE_GUTTER))
    .h_full()
}

fn trunk_line(color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let x = bounds.left() + px(TREE_TRUNK_X);
            let mut trunk = PathBuilder::stroke(px(1.));
            trunk.move_to(point(x, bounds.top()));
            trunk.line_to(point(x, bounds.bottom()));
            if let Ok(path) = trunk.build() {
                window.paint_path(path, color);
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(TREE_GUTTER))
    .h_full()
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
        ToolCall::Mcp { .. } | ToolCall::Unknown { .. } => IconName::AgentWidget,
    }
}

fn column(content: impl IntoElement) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_center()
        .px(px(SIDE_GUTTER))
        .child(div().w_full().max_w(px(CONTENT_WIDTH)).child(content))
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_scroll_state();
        let session = self.session.read(cx);
        let is_new_chat = session.entries().is_empty() && !session.is_working();
        let pending = session
            .pending_questions()
            .first()
            .map(|pending| pending.id().to_string());
        let root = v_flex()
            .key_context("AgentChat")
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(Self::stop));
        if is_new_chat {
            return root.child(self.render_new_chat(cx));
        }
        let expanded = self.composer_is_expanded(window, cx);
        let turns = self.render_turns(window, cx);
        let composer_area = match pending.and_then(|id| {
            self.session
                .read(cx)
                .pending_questions()
                .iter()
                .find(|pending| pending.id() == id)
                .map(|pending| self.render_question_panel(pending, cx))
        }) {
            Some(panel) => panel,
            None => self.render_pill(expanded, DOCKED_COMPOSER_RADIUS, cx),
        };
        root.child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("agent-transcript")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_handle)
                        .child(column(
                            v_flex()
                                .w_full()
                                .pt(px(16.))
                                .pb(px(32.))
                                .gap(px(16.))
                                .children(turns),
                        )),
                )
                .when(self.show_scroll_button, |this| {
                    this.child(self.render_scroll_button(cx))
                }),
        )
        .child(
            h_flex().w_full().justify_center().child(
                v_flex()
                    .w_full()
                    .max_w(px(COMPOSER_WIDTH))
                    .px(px(16.))
                    .pb(px(8.))
                    .child(self.render_status_strip(cx))
                    .child(composer_area)
                    .child(div().pt(px(8.)).child(self.render_footer(cx))),
            ),
        )
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl EventEmitter<ItemEvent> for ChatView {}

impl Item for ChatView {
    type Event = ItemEvent;

    fn tab_content(&self, params: TabContentParams, _: &Window, cx: &App) -> AnyElement {
        let session = self.session.read(cx);
        let needs_input = !session.pending_questions().is_empty();
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
            .when(!needs_input && session.is_working(), |this| {
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
        Some(agent_icon(self.session.read(cx).kind()))
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        let kind = self.session.read(cx).kind();
        Some(format!("{} · {}", kind.label(), self.title(cx)).into())
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }

    fn show_toolbar(&self) -> bool {
        false
    }
}
