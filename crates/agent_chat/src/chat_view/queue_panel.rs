use super::{ChatView, ui};
use crate::chat_style::{ink, selected_row};
use gpui::{
    AnyElement, Context, Focusable as _, IntoElement, ObjectFit, ParentElement, Render, SharedString, Styled,
    Window, div, img, px,
};
use ui::{Icon, IconName, IconSize, Tooltip, prelude::*};

#[derive(Clone)]
pub(super) struct DraggedQueued {
    id: u64,
    text: SharedString,
}

impl Render for DraggedQueued {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(420.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(10.))
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .shadow_lg()
            .text_size(ui(13.))
            .truncate()
            .child(self.text.clone())
    }
}

impl ChatView {
    pub(super) fn edit_queued(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((index, original)) = self.editing_queued.take() {
            self.session
                .update(cx, |session, cx| session.restore_queued(index, original, cx));
        }
        let Some((index, prompt)) = self
            .session
            .update(cx, |session, cx| session.take_queued_for_edit(id, cx))
        else {
            return;
        };
        self.composer.update(cx, |editor, cx| {
            editor.set_text(prompt.text.clone(), window, cx);
        });
        self.attachments = prompt.attachments.clone();
        self.composer_skills = prompt.skills.clone();
        self.editing_queued = Some((index, prompt));
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn cancel_queued_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((index, original)) = self.editing_queued.take() else {
            return;
        };
        self.session
            .update(cx, |session, cx| session.restore_queued(index, original, cx));
        self.composer
            .update(cx, |editor, cx| editor.clear(window, cx));
        self.attachments.clear();
        self.composer_skills.clear();
        cx.notify();
    }

    pub(super) fn render_queue_panel(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        let queue = session.queue();
        if queue.is_empty() && self.editing_queued.is_none() {
            return None;
        }
        let colors = cx.theme().colors();
        let can_steer = session.is_working();
        let rows = queue.iter().enumerate().map(|(index, queued)| {
            let id = queued.id;
            let text: SharedString = if queued.prompt.text.trim().is_empty() {
                "Image".into()
            } else {
                agent_harness::view::single_line(&queued.prompt.text).into()
            };
            let group: SharedString = format!("agent-queued-{id}").into();
            let action_button = |key: &str, label: &'static str, tooltip: &'static str| {
                div()
                    .id(SharedString::from(format!("agent-queued-{key}-{id}")))
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(ui(12.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text)
                    .bg(ink(0.06, cx))
                    .hover(|style| style.bg(ink(0.1, cx)))
                    .tooltip(Tooltip::text(tooltip))
                    .child(label)
            };
            let icon_button = |key: &str, icon: IconName, tooltip: &'static str| {
                div()
                    .id(SharedString::from(format!("agent-queued-{key}-{id}")))
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|style| style.bg(ink(0.08, cx)))
                    .tooltip(Tooltip::text(tooltip))
                    .child(Icon::new(icon).size(IconSize::XSmall).color(Color::Muted))
            };
            let hover = selected_row(cx);
            h_flex()
                .id(group.clone())
                .group(group)
                .w_full()
                .gap(px(8.))
                .px(px(8.))
                .py(px(5.))
                .rounded(px(8.))
                .hover(move |style| style.bg(hover))
                .cursor_grab()
                .on_drag(
                    DraggedQueued {
                        id,
                        text: text.clone(),
                    },
                    |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                )
                .drag_over::<DraggedQueued>(move |style, _, _, _| style.bg(hover))
                .on_drop(cx.listener(move |this, dragged: &DraggedQueued, _, cx| {
                    this.session
                        .update(cx, |session, cx| session.move_queued(dragged.id, index, cx))
                }))
                .child(
                    div()
                        .flex_none()
                        .text_size(ui(11.))
                        .text_color(colors.text_muted)
                        .child(format!("{}", index + 1)),
                )
                .children(queued.prompt.attachments.first().map(|path| {
                    div()
                        .flex_none()
                        .size(px(22.))
                        .rounded(px(5.))
                        .overflow_hidden()
                        .child(img(path.clone()).size_full().object_fit(ObjectFit::Cover))
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(ui(13.))
                        .text_color(colors.text.opacity(0.9))
                        .child(text),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap(px(2.))
                        .child(
                            icon_button("edit", IconName::AgentPen, "Edit").on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.edit_queued(id, window, cx)
                                }),
                            ),
                        )
                        .child(
                            icon_button("remove", IconName::AgentClose, "Remove").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.session
                                        .update(cx, |session, cx| session.remove_queued(id, cx))
                                }),
                            ),
                        )
                        .child(
                            action_button(
                                "now",
                                "Send now",
                                "Stop the agent and send this right away",
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.session
                                    .update(cx, |session, cx| session.send_queued_now(id, cx))
                            })),
                        )
                        .when(can_steer, |this| {
                            this.child(
                                action_button(
                                    "steer",
                                    "Steer",
                                    "Send into the running turn without stopping it",
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.session.update(cx, |session, cx| {
                                            session.steer_queued(id, cx)
                                        })
                                    },
                                )),
                            )
                        }),
                )
        });
        Some(
            v_flex()
                .mb(px(6.))
                .p(px(4.))
                .gap(px(2.))
                .rounded(px(12.))
                .border_1()
                .border_color(colors.border)
                .bg(colors.elevated_surface_background)
                .when(!queue.is_empty(), |this| {
                    this.child(
                        div()
                            .px(px(8.))
                            .pt(px(4.))
                            .pb(px(2.))
                            .text_size(ui(10.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(colors.text_muted)
                            .child("QUEUED · SENDS WHEN THE AGENT FINISHES"),
                    )
                })
                .children(rows)
                .when(self.editing_queued.is_some(), |this| {
                    this.child(
                        h_flex()
                            .px(px(8.))
                            .py(px(5.))
                            .gap(px(8.))
                            .text_size(ui(12.))
                            .text_color(colors.text_muted)
                            .child(div().flex_1().child("Editing a queued message"))
                            .child(
                                div()
                                    .id("agent-queued-cancel-edit")
                                    .cursor_pointer()
                                    .text_color(colors.text)
                                    .hover(|style| style.opacity(0.8))
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_queued_edit(window, cx)
                                    })),
                            ),
                    )
                })
                .into_any_element(),
        )
    }
}
