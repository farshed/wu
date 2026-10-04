use super::{
    ChatView,
    composer::{TRAY_COMPOSER_OVERLAP, TRAY_SIDE_INSET, tray_surface},
    ui,
};
use crate::{
    Send,
    chat_style::{hairline, icon, ink},
    session::Prompt,
};
use gpui::{
    AnyElement, Context, Focusable as _, Hsla, IntoElement, ObjectFit, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, img, px,
};
use ui::{IconName, Tooltip, prelude::*};

const ROW_HEIGHT: f32 = 36.;
const ROW_PAD_X: f32 = 8.;
const ROW_RADIUS: f32 = 8.;
const PANEL_PAD_X: f32 = 4.;
const QUEUE_TEXT_SIZE: f32 = 12.5;
const QUEUE_ICON_SIZE: f32 = 13.;
const QUEUE_ACTION_SIZE: f32 = 28.;
const DRAG_DOT_SIZE: f32 = 1.25;
const DRAG_DOT_COLUMN_PITCH: f32 = 3.25;
const DRAG_DOT_ROW_PITCH: f32 = 2.7;

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

pub(super) struct QueuedEdit {
    id: u64,
    draft: Prompt,
    _end_on_close: Subscription,
}

impl ChatView {
    pub(super) fn edit_queued(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let draft = match self.editing_queued.take() {
            Some(edit) => {
                self.session
                    .update(cx, |session, cx| session.finish_queued_edit(edit.id, None, cx));
                edit.draft
            }
            None => Prompt {
                text: self.composer.read(cx).text(cx),
                attachments: std::mem::take(&mut self.attachments),
                skills: std::mem::take(&mut self.composer_skills),
            },
        };
        let Some(prompt) = self
            .session
            .update(cx, |session, cx| session.begin_queued_edit(id, cx))
        else {
            self.restore_draft(draft, window, cx);
            return;
        };
        self.set_composer_prompt(prompt, window, cx);
        let session = self.session.clone();
        self.editing_queued = Some(QueuedEdit {
            id,
            draft,
            _end_on_close: cx.on_release(move |_, cx| {
                session.update(cx, |session, cx| session.finish_queued_edit(id, None, cx))
            }),
        });
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn cancel_queued_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.editing_queued.take() else {
            return;
        };
        self.session
            .update(cx, |session, cx| session.finish_queued_edit(edit.id, None, cx));
        self.restore_draft(edit.draft, window, cx);
    }

    pub(super) fn save_queued_edit(
        &mut self,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.editing_queued.take() else {
            return;
        };
        self.session.update(cx, |session, cx| {
            session.finish_queued_edit(edit.id, Some(prompt), cx)
        });
        self.restore_draft(edit.draft, window, cx);
    }

    fn restore_draft(&mut self, draft: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        self.set_composer_prompt(draft, window, cx);
        cx.notify();
    }

    fn set_composer_prompt(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |editor, cx| editor.set_text(prompt.text, window, cx));
        self.attachments = prompt.attachments;
        self.composer_skills = prompt.skills;
    }

    pub(super) fn render_queue_panel(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        let queue = session.queue();
        if queue.is_empty() && self.editing_queued.is_none() {
            return None;
        }
        let colors = cx.theme().colors();
        let can_steer = session.is_working();
        let muted_glyph = colors.text_muted.opacity(0.8);
        let action_hover = ink(0.07, cx);
        let glyph_button = |key: &str, glyph: IconName, tooltip: &'static str| {
            let group: SharedString = format!("agent-queued-{key}-group").into();
            div()
                .id(SharedString::from(format!("agent-queued-{key}")))
                .group(group.clone())
                .size(px(QUEUE_ACTION_SIZE))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .opacity(0.72)
                .cursor_pointer()
                .hover(move |style| style.opacity(1.).bg(action_hover))
                .tooltip(Tooltip::text(tooltip))
                .child(
                    icon(glyph, px(QUEUE_ICON_SIZE), muted_glyph)
                        .group_hover(group, |style| style.text_color(colors.text)),
                )
        };
        let primary_button = |key: &str, label: &'static str, tooltip: &'static str| {
            div()
                .id(SharedString::from(format!("agent-queued-{key}")))
                .w(px(72.))
                .h(px(QUEUE_ACTION_SIZE))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .cursor_pointer()
                .text_size(px(11.5))
                .text_color(colors.text_muted)
                .hover(move |style| style.bg(action_hover).text_color(colors.text))
                .tooltip(Tooltip::text(tooltip))
                .child(label)
        };
        let row_hover = ink(0.04, cx);
        let frame = hairline(0.1, cx);
        let thumbnail_plate = ink(0.035, cx);
        let editing_id = self.editing_queued.as_ref().map(|edit| edit.id);
        let mut rows: Vec<AnyElement> = queue
            .iter()
            .enumerate()
            .filter(|(_, queued)| Some(queued.id) != editing_id)
            .map(|(index, queued)| {
                let id = queued.id;
                let text: SharedString = if queued.prompt.text.trim().is_empty() {
                    "Image".into()
                } else {
                    agent_harness::view::single_line(&queued.prompt.text).into()
                };
                h_flex()
                    .id(SharedString::from(format!("agent-queued-{id}")))
                    .h(px(ROW_HEIGHT))
                    .flex_none()
                    .px(px(ROW_PAD_X - PANEL_PAD_X))
                    .gap(px(8.))
                    .rounded(px(ROW_RADIUS))
                    .hover(move |style| style.bg(row_hover))
                    .on_drag(
                        DraggedQueued {
                            id,
                            text: text.clone(),
                        },
                        |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                    )
                    .drag_over::<DraggedQueued>(move |style, _, _, _| style.bg(row_hover))
                    .on_drop(cx.listener(move |this, dragged: &DraggedQueued, _, cx| {
                        this.session
                            .update(cx, |session, cx| session.move_queued(dragged.id, index, cx))
                    }))
                    .child(
                        div()
                            .w(px(14.))
                            .h(px(22.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_grab()
                            .child(drag_handle(colors.text_muted.opacity(0.5))),
                    )
                    .children(queued.prompt.attachments.first().map(|path| {
                        div()
                            .w(px(40.))
                            .h(px(28.))
                            .flex_none()
                            .rounded(px(5.))
                            .border_1()
                            .border_color(frame)
                            .bg(thumbnail_plate)
                            .overflow_hidden()
                            .child(
                                img(path.clone())
                                    .w(px(38.))
                                    .h(px(26.))
                                    .rounded(px(4.))
                                    .object_fit(ObjectFit::Cover),
                            )
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(QUEUE_TEXT_SIZE))
                            .line_height(px(16.))
                            .text_color(colors.text.opacity(0.9))
                            .child(text),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(3.))
                            .child(
                                glyph_button(&format!("remove-{id}"), IconName::Trash, "Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.session
                                            .update(cx, |session, cx| session.remove_queued(id, cx))
                                    })),
                            )
                            .child(
                                glyph_button(&format!("edit-{id}"), IconName::AgentPen, "Edit")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.edit_queued(id, window, cx)
                                    })),
                            )
                            .child(
                                primary_button(
                                    &format!("now-{id}"),
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
                                    primary_button(
                                        &format!("steer-{id}"),
                                        "Steer",
                                        "Send into the running turn without stopping it",
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.session.update(cx, |session, cx| {
                                            session.steer_queued(id, cx)
                                        })
                                    })),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        if let Some(editing_id) = editing_id {
            let index = queue
                .iter()
                .position(|queued| queued.id == editing_id)
                .unwrap_or(rows.len());
            let editing_row = h_flex()
                .id("agent-queued-editing")
                .h(px(ROW_HEIGHT))
                .flex_none()
                .px(px(ROW_PAD_X - PANEL_PAD_X))
                .gap(px(8.))
                .rounded(px(ROW_RADIUS))
                .bg(ink(0.06, cx))
                .child(div().w(px(14.)).flex_none())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(QUEUE_TEXT_SIZE))
                        .text_color(colors.text_muted)
                        .child("Editing in composer"),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap(px(3.))
                        .child(
                            glyph_button("save", IconName::AgentCheck, "Save to queue").on_click(
                                cx.listener(|this, _, window, cx| this.send(&Send, window, cx)),
                            ),
                        )
                        .child(
                            glyph_button("cancel-edit", IconName::AgentClose, "Cancel").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.cancel_queued_edit(window, cx)
                                }),
                            ),
                        ),
                )
                .into_any_element();
            rows.insert(index.min(rows.len()), editing_row);
        }
        Some(
            div()
                .mx(px(TRAY_SIDE_INSET))
                .mb(px(-TRAY_COMPOSER_OVERLAP))
                .child(tray_surface(cx).children(rows))
                .into_any_element(),
        )
    }
}

fn drag_handle(color: Hsla) -> impl IntoElement {
    let dot = move || {
        div()
            .size(px(DRAG_DOT_SIZE))
            .rounded_full()
            .bg(color)
    };
    v_flex()
        .gap(px(DRAG_DOT_ROW_PITCH - DRAG_DOT_SIZE))
        .children((0..3).map(move |_| {
            h_flex()
                .gap(px(DRAG_DOT_COLUMN_PITCH - DRAG_DOT_SIZE))
                .child(dot())
                .child(dot())
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentKind,
        session::tests::{enqueue, new_store},
    };
    use gpui::{Entity, TestAppContext, WeakEntity};
    use project::FakeFs;

    fn queued_texts(view: &Entity<ChatView>, cx: &mut gpui::VisualTestContext) -> Vec<String> {
        view.read_with(cx, |view, cx| {
            view.session
                .read(cx)
                .queue()
                .iter()
                .map(|queued| queued.prompt.text.clone())
                .collect()
        })
    }

    fn composer_text(view: &Entity<ChatView>, cx: &mut gpui::VisualTestContext) -> String {
        view.read_with(cx, |view, cx| view.composer.read(cx).text(cx))
    }

    #[gpui::test]
    async fn editing_a_queued_message_keeps_it_and_the_draft(cx: &mut TestAppContext) {
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
        enqueue(&session, "queued", cx);
        let id = session.read_with(cx, |session, _| session.queue()[0].id);
        let (_, cx) = cx.add_window_view(|_, _| gpui::EmptyView);
        let view = cx.update(|window, cx| {
            cx.new(|cx| {
                ChatView::new(
                    session.clone(),
                    store.clone(),
                    project,
                    WeakEntity::new_invalid(),
                    window,
                    cx,
                )
            })
        });
        let set_text = |text: &'static str, cx: &mut gpui::VisualTestContext| {
            view.update_in(cx, |view, window, cx| {
                view.composer
                    .update(cx, |editor, cx| editor.set_text(text, window, cx))
            })
        };
        let start_edit = |cx: &mut gpui::VisualTestContext| {
            view.update_in(cx, |view, window, cx| view.edit_queued(id, window, cx))
        };
        let send = |cx: &mut gpui::VisualTestContext| {
            view.update_in(cx, |view, window, cx| view.send(&Send, window, cx))
        };

        set_text("my draft", cx);
        start_edit(cx);
        assert_eq!(composer_text(&view, cx), "queued");
        assert_eq!(queued_texts(&view, cx), ["queued"]);

        set_text("", cx);
        send(cx);
        assert_eq!(queued_texts(&view, cx), ["queued"]);
        assert_eq!(composer_text(&view, cx), "my draft");
        view.read_with(cx, |view, _| assert!(view.editing_queued.is_none()));

        start_edit(cx);
        set_text("queued, edited", cx);
        send(cx);
        assert_eq!(queued_texts(&view, cx), ["queued, edited"]);
        assert_eq!(composer_text(&view, cx), "my draft");

        start_edit(cx);
        view.update_in(cx, |view, window, cx| view.cancel_queued_edit(window, cx));
        assert_eq!(queued_texts(&view, cx), ["queued, edited"]);
        assert_eq!(composer_text(&view, cx), "my draft");

        start_edit(cx);
        session.read_with(cx, |session, _| assert_eq!(session.editing_queued(), Some(id)));
        drop(view);
        cx.update(|_, _| {});
        cx.run_until_parked();
        session.read_with(cx, |session, _| {
            assert_eq!(session.editing_queued(), None);
            assert_eq!(session.queue().len(), 1);
            assert_eq!(session.queue()[0].prompt.text, "queued, edited");
        });
    }
}
