use super::*;
use crate::{
    AgentKind,
    chat_style::{composer_surface, icon, icon_size_px, tracked_upper},
};

const NEW_CHAT_TEXTAREA_MIN: f32 = 76.;
const THREAD_TEXTAREA_MIN: f32 = 60.;
const TEXTAREA_MAX: f32 = 260.;
const ACTIONS_ROW_HEIGHT: f32 = 42.;
const COMPACT_ROW_HEIGHT: f32 = 47.;
const ACTION_BUTTON_SIZE: f32 = 28.;
const ACTION_UTILITY_GAP: f32 = 2.;
const ACTION_PRIMARY_GAP: f32 = 8.;
const MODEL_CHIP_PADDING: f32 = 6.;
const COMPACT_ACTION_INSET: f32 = 8.;
const EXPANDED_ACTION_INSET: f32 = 12.;
const MODEL_CHIP_MAX_FRACTION: f32 = 0.45;
const TRAY_RADIUS: f32 = 16.;
pub(super) const TRAY_SIDE_INSET: f32 = 16.;
pub(super) const TRAY_COMPOSER_OVERLAP: f32 = 18.;
const COLLAPSE_HYSTERESIS: f32 = 32.;

#[derive(Default)]
pub(super) struct ComposerLayout {
    expanded: bool,
    compact_capacity: Option<f32>,
    expanded_anchor: Option<f32>,
    width_before_flip: Option<f32>,
}

fn composer_flip(expanded: bool, wraps: bool, text_width: f32, capacity: Option<f32>) -> bool {
    match capacity {
        Some(capacity) if expanded => text_width >= capacity - COLLAPSE_HYSTERESIS,
        _ => wraps,
    }
}

fn single_line_width(text: &str, window: &Window) -> f32 {
    let font_size = ui(14.).to_pixels(window.rem_size());
    let run = TextRun {
        len: text.len(),
        font: window.text_style().font(),
        ..Default::default()
    };
    let line = window
        .text_system()
        .shape_line(text.to_string().into(), font_size, &[run], None);
    f32::from(line.width)
}

pub(super) fn tray_surface(cx: &App) -> gpui::Div {
    div()
        .rounded_t(px(TRAY_RADIUS))
        .bg(composer_surface(cx))
        .border_1()
        .border_color(cx.theme().colors().border)
        .shadow_lg()
        .px(px(4.))
        .pt(px(4.))
        .pb(px(TRAY_COMPOSER_OVERLAP))
        .flex()
        .flex_col()
}

impl ChatView {
    pub(super) fn render_model_chip(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let kind = session.kind();
        let settings = session.settings();
        let selection = Selection::resolve(self.store.read(cx).models(kind), settings);
        let model_label = selection.model_label(settings, kind);
        let effort = selection.reasoning.map(view::reasoning_label);
        let fast = selection.fast_on();
        let colors = cx.theme().colors();
        let brand = match kind {
            AgentKind::Codex => agent_icon(kind).color(Color::Muted),
            AgentKind::Claude => agent_icon(kind),
        }
        .size(icon_size_px(16., window));
        let session = self.session.clone();
        let store = self.store.clone();
        PopoverMenu::new(SharedString::from(format!(
            "agent-model-picker-{}",
            cx.entity_id()
        )))
        .trigger(
            Chip::new(
                "agent-model-chip",
                8.,
                h_flex()
                    .h(px(32.))
                    .max_w(px(248.))
                    .min_w_0()
                    .px(px(MODEL_CHIP_PADDING))
                    .gap(px(6.))
                    .text_size(ui(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(brand)
                    .child(div().min_w_0().truncate().child(model_label))
                    .when_some(effort, |this, effort| {
                        this.child(
                            div()
                                .map(|mut suffix| {
                                    suffix.style().flex_shrink = Some(1000.);
                                    suffix
                                })
                                .min_w_0()
                                .truncate()
                                .text_color(colors.text_muted.opacity(0.7))
                                .child(effort),
                        )
                    })
                    .when(fast, |this| {
                        this.child(icon(IconName::AgentFastFilled, px(13.), accent(cx)))
                    }),
            )
            .shrinkable()
            .text_colors(colors.text.opacity(0.9), colors.text),
        )
        .menu(move |window, cx| {
            let session = session.clone();
            let store = store.clone();
            Some(cx.new(|cx| ModelPicker::new(session, store, window, cx)))
        })
        .with_handle(self.model_picker.clone())
        .anchor(Anchor::BottomRight)
        .attach(Anchor::TopRight)
        .offset(point(px(0.), px(-6.)))
    }

    pub(super) fn render_send_button(&self, cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let is_working = self.session.read(cx).is_working();
        let has_text = self.staging_count == 0
            && (!self.composer.read(cx).text(cx).trim().is_empty()
                || !self.attachments.is_empty());
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
            .child(icon(IconName::AgentSend, px(14.), plate))
            .into_any_element()
    }

    pub(super) fn composer_is_expanded(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let text = self.composer.read(cx).text(cx);
        let (wraps, width) = self.composer.update(cx, |editor, cx| {
            let wraps = editor
                .snapshot(window, cx)
                .display_snapshot
                .max_point()
                .row()
                .0
                > 0;
            (wraps, editor.last_bounds().map(|bounds| f32::from(bounds.size.width)))
        });
        let layout = &mut self.composer_layout;
        let fresh_width = width.filter(|width| Some(*width) != layout.width_before_flip);
        if let Some(width) = fresh_width {
            layout.width_before_flip = None;
            if layout.expanded {
                layout.expanded_anchor.get_or_insert(width);
            } else {
                layout.compact_capacity = Some(width);
            }
        }
        let capacity = layout.compact_capacity.map(|capacity| {
            match (layout.expanded_anchor, fresh_width) {
                (Some(anchor), Some(width)) => capacity + width - anchor,
                _ => capacity,
            }
        });
        let next = if text.contains('\n') {
            true
        } else {
            let text_width = match capacity {
                Some(_) if layout.expanded => single_line_width(&text, window),
                _ => 0.,
            };
            composer_flip(layout.expanded, wraps, text_width, capacity)
        };
        if next != layout.expanded {
            layout.expanded = next;
            layout.width_before_flip = width;
            layout.expanded_anchor = None;
        }
        next
    }

    fn render_attach_button(&self, dictating: bool, cx: &Context<Self>) -> impl IntoElement {
        let hover = ink(0.10, cx);
        let muted = cx.theme().colors().text_muted;
        div()
            .id("agent-attach")
            .size(px(ACTION_BUTTON_SIZE))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(Tooltip::text(if dictating {
                "Cancel dictation"
            } else {
                "Attach images"
            }))
            .child(if dictating {
                icon(IconName::AgentClose, px(16.), muted)
            } else {
                icon(IconName::AgentPaperclip, px(18.), muted)
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if dictating {
                    this.cancel_dictation(cx);
                } else {
                    this.pick_images(window, cx);
                }
            }))
    }

    pub(super) fn render_pill(
        &self,
        expanded: bool,
        radius: f32,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let show_commands = self.command_menu.is_some()
            && self.composer.focus_handle(cx).is_focused(window);
        let mut key_context = KeyContext::default();
        key_context.add("AgentComposer");
        if AgentChatSettings::get_global(cx).send_with == AgentChatSendKey::ModifierEnter {
            key_context.add("send_with_modifier");
        }
        if show_commands {
            key_context.add("command_menu");
        }
        let new_chat = {
            let session = self.session.read(cx);
            session.entries().is_empty() && !session.is_working()
        };
        let dictating = self.dictation_active();
        let microphone = self.render_dictation_button(cx);
        let model_gap = if microphone.is_some() {
            ACTION_PRIMARY_GAP - MODEL_CHIP_PADDING
        } else {
            ACTION_PRIMARY_GAP
        };
        let inset = if expanded {
            EXPANDED_ACTION_INSET
        } else {
            COMPACT_ACTION_INSET
        };
        let voice_track = if microphone.is_some() {
            let leading = inset + ACTION_BUTTON_SIZE + dictation::VOICE_TRACK_GAP;
            let trailing = leading + ACTION_BUTTON_SIZE + ACTION_PRIMARY_GAP;
            let top = if expanded {
                2.
            } else {
                (COMPACT_ROW_HEIGHT - dictation::VOICE_TRACK_HEIGHT) / 2.
            };
            self.render_voice_track(leading, trailing, top, cx)
        } else {
            None
        };
        let beneath_voice = if voice_track.is_some() { 0. } else { 1. };
        let model_picker = div()
            .min_w_0()
            .max_w(gpui::relative(MODEL_CHIP_MAX_FRACTION))
            .opacity(beneath_voice)
            .child(self.render_model_chip(window, cx));
        let trailing = h_flex()
            .flex_none()
            .gap(px(ACTION_PRIMARY_GAP))
            .children(microphone)
            .child(self.render_send_button(cx));
        let attach = h_flex()
            .gap(px(ACTION_UTILITY_GAP))
            .child(self.render_attach_button(dictating, cx));
        let pill = div()
            .key_context(key_context)
            .relative()
            .capture_action(cx.listener(|this, _: &editor::actions::Paste, _, cx| {
                if this.paste_images(cx) {
                    cx.stop_propagation();
                }
            }))
            .when(show_commands, |this| this.child(self.render_command_menu(cx)))
            .w_full()
            .rounded(px(radius))
            .border_1()
            .border_color(colors.border)
            .bg(composer_surface(cx))
            .shadow_lg()
            .flex()
            .flex_col()
            .children(self.render_attachment_strip(cx));
        let pill = if expanded {
            let textarea_min = if new_chat {
                NEW_CHAT_TEXTAREA_MIN
            } else {
                THREAD_TEXTAREA_MIN
            };
            pill.child(
                div()
                    .flex_none()
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(4.))
                    .min_h(px(textarea_min))
                    .max_h(px(TEXTAREA_MAX))
                    .child(self.composer.clone()),
            )
            .child(
                h_flex()
                    .relative()
                    .h(px(ACTIONS_ROW_HEIGHT))
                    .pt(px(2.))
                    .pb(px(8.))
                    .px(px(inset))
                    .gap(px(model_gap))
                    .child(attach.flex_1().min_w_0())
                    .child(model_picker)
                    .child(trailing)
                    .children(voice_track),
            )
        } else {
            pill.justify_end().child(
                h_flex()
                    .relative()
                    .h(px(COMPACT_ROW_HEIGHT))
                    .child(attach.flex_none().pl(px(inset)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(px(8.))
                            .opacity(beneath_voice)
                            .child(self.composer.clone()),
                    )
                    .child(model_picker)
                    .child(trailing.pl(px(model_gap)).pr(px(inset)))
                    .children(voice_track),
            )
        };
        v_flex()
            .w_full()
            .gap(px(8.))
            .child(pill)
            .children(self.render_dictation_status(cx))
            .into_any_element()
    }

    pub(super) fn branch_name(&self, cx: &App) -> Option<SharedString> {
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

    pub(super) fn footer_label_shell(icon_name: IconName, cx: &App) -> gpui::Div {
        let color = cx.theme().colors().text_muted.opacity(0.6);
        h_flex()
            .h(px(20.))
            .min_w_0()
            .px(px(8.))
            .gap(px(6.))
            .text_size(ui(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(color)
            .child(icon(icon_name, px(12.), color))
    }

    pub(super) fn footer_label(icon: IconName, label: SharedString, cx: &App) -> impl IntoElement {
        Self::footer_label_shell(icon, cx)
            .max_w(px(160.))
            .child(div().min_w_0().truncate().child(label))
    }

    pub(super) fn render_checkout(&self, cx: &App) -> impl IntoElement {
        let metadata = self.session.read(cx).metadata();
        let label: SharedString = if metadata.project_root.is_some() {
            "Worktree".into()
        } else {
            "Local checkout".into()
        };
        let branch = metadata
            .branch
            .clone()
            .map(SharedString::from)
            .or_else(|| self.branch_name(cx));
        h_flex()
            .flex_1()
            .min_w_0()
            .pl(px(10.))
            .gap(px(4.))
            .when_some(branch, |this, branch| {
                let folder = if metadata.project_root.is_some() {
                    IconName::AgentFolderWithFiles
                } else {
                    IconName::AgentFolder
                };
                this.child(Self::footer_label(folder, label, cx)).child(
                    Self::footer_label_shell(IconName::AgentGitBranch, cx)
                        .child(div().min_w_0().truncate().child(branch)),
                )
            })
    }

    pub(super) fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement {
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
        let ring_hover = ink(0.05, cx);
        h_flex()
            .h(px(24.))
            .child(self.render_checkout(cx))
            .children(self.render_background_indicator(cx))
            .child(
                h_flex()
                    .flex_none()
                    .pl(px(4.))
                    .pr(px(10.))
                    .gap(px(4.))
                    .child(
                        PopoverMenu::new(SharedString::from(format!(
                            "agent-plan-usage-{}",
                            cx.entity_id()
                        )))
                        .trigger(
                            Chip::new(
                                "agent-plan-usage-chip",
                                6.,
                                usage_chip(usage_fraction, cx),
                            )
                            .hover_background(ring_hover),
                        )
                        .menu(move |_, cx| {
                            let store = store.clone();
                            Some(cx.new(|cx| UsageCard::new(store, kind, cx)))
                        })
                        .anchor(Anchor::BottomRight)
                        .attach(Anchor::TopRight)
                        .offset(point(px(0.), px(-6.))),
                    )
                    .child(
                        PopoverMenu::new(SharedString::from(format!(
                            "agent-context-usage-{}",
                            cx.entity_id()
                        )))
                        .trigger(
                            Chip::new(
                                "agent-context-chip",
                                6.,
                                context_chip(context_fraction, cx),
                            )
                            .hover_background(ring_hover),
                        )
                        .menu(move |_, cx| Some(cx.new(|cx| ContextCard::new(context, cx))))
                        .anchor(Anchor::BottomRight)
                        .attach(Anchor::TopRight)
                        .offset(point(px(0.), px(-6.))),
                    ),
            )
    }

    pub(super) fn render_status_strip(&self, cx: &Context<Self>) -> impl IntoElement {
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

    pub(super) fn render_scroll_button(&self, cx: &Context<Self>) -> impl IntoElement {
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
                        this.show_scroll_button = false;
                        this.list.set_follow_mode(gpui::FollowMode::Tail);
                        cx.notify();
                    })),
            )
    }

    pub(super) fn pick_option(
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

    pub(super) fn submit_answers(&mut self, id: &str, cx: &mut Context<Self>) {
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
                labels: self.answer_labels(&question.id, cx),
            })
            .collect();
        for answer in &answers {
            self.selected_options.remove(&answer.question_id);
        }
        self.question_pages.remove(id);
        self.session
            .update(cx, |session, cx| session.answer(id, answers, cx));
    }

    pub(super) fn render_option_row(
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
                    .flex_1()
                    .min_w_0()
                    .text_size(ui(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if picked {
                        colors.text
                    } else {
                        colors.text.opacity(0.9)
                    })
                    .child(label),
            )
            .when(number <= 9, |this| {
                this.child(
                    div()
                        .flex_none()
                        .size(px(22.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .bg(if picked { ink(0.16, cx) } else { ink(0.05, cx) })
                        .text_size(ui(11.))
                        .text_color(if picked {
                            colors.text
                        } else {
                            colors.text_muted.opacity(0.6)
                        })
                        .child(number.to_string()),
                )
            })
    }

    pub(super) fn render_question_panel(&self, pending: &PendingQuestion, cx: &Context<Self>) -> AnyElement {
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
        let answer_editor = self
            .answer_editors
            .get(&question.id)
            .map(|(editor, _)| editor.clone());
        let options: Vec<AnyElement> = if answer_editor.is_some() {
            Vec::new()
        } else if is_permission {
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
        let answered = !self.answer_labels(&question.id, cx).is_empty();
        let is_last_page = page_index + 1 >= count;
        let header = if is_permission {
            "Permission".to_string()
        } else {
            question.header.clone()
        };
        let muted_label = colors.text_muted.opacity(0.6);
        let back_hover = ink(0.06, cx);
        v_flex()
            .w_full()
            .rounded(px(COMPOSER_RADIUS))
            .border_1()
            .border_color(colors.border)
            .bg(composer_surface(cx))
            .shadow_lg()
            .child(
                v_flex()
                    .px(px(16.))
                    .pt(px(16.))
                    .child(
                        h_flex()
                            .gap(px(10.))
                            .child(
                                div()
                                    .text_size(ui(10.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(muted_label)
                                    .child(tracked_upper(&header)),
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
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(muted_label)
                                        .child(format!("{} of {count}", page_index + 1)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .text_size(ui(15.))
                            .line_height(px(20.))
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
                    .child(v_flex().mt(px(12.)).gap(px(4.)).children(options))
                    .when_some(answer_editor, |this, editor| {
                        this.child(
                            div()
                                .mt(px(12.))
                                .border_t_1()
                                .border_color(hairline(0.06, cx))
                                .pt(px(12.))
                                .pb(px(4.))
                                .px(px(4.))
                                .child(editor),
                        )
                    }),
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
                            .hover(move |style| style.bg(back_hover).text_color(colors.text))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanding_and_collapsing_do_not_share_a_threshold() {
        let capacity = Some(300.);
        assert!(!composer_flip(false, false, 0., capacity));
        assert!(composer_flip(false, true, 0., capacity));
        let in_band = 300. - COLLAPSE_HYSTERESIS + 1.;
        assert!(composer_flip(true, false, in_band, capacity));
        assert!(!composer_flip(true, false, 300. - COLLAPSE_HYSTERESIS - 1., capacity));
        assert!(composer_flip(true, true, 0., None));
        assert!(!composer_flip(true, false, 0., None));
    }
}
