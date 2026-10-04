use super::{CONTENT_WIDTH, ChatView, ui};
use crate::{chat_style::popover_card, session::Entry};
use gpui::{
    AnyElement, Bounds, Context, IntoElement, ParentElement, Pixels, SharedString, Styled, canvas,
    deferred, div, point, px,
};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use ui::prelude::*;

const MIN_CONTAINER_WIDTH: f32 = 768.;
const PREVIEW_PROMPT_CHARS: usize = 160;
const PREVIEW_REPLY_CHARS: usize = 200;

#[derive(Default, Clone)]
pub(super) struct TurnBounds {
    pub turns: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    pub container: Rc<RefCell<Option<Bounds<Pixels>>>>,
}

struct Tick {
    entry_index: usize,
    prompt: String,
    reply: Option<String>,
}

fn clip(text: &str, max: usize) -> String {
    let line = agent_harness::view::single_line(text);
    if line.chars().count() <= max {
        return line;
    }
    let mut clipped: String = line.chars().take(max - 1).collect();
    clipped.push('…');
    clipped
}

fn ticks(entries: &[Entry], cx: &gpui::App) -> Vec<Tick> {
    let mut ticks: Vec<Tick> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            Entry::User {
                text, attachments, ..
            } => ticks.push(Tick {
                entry_index: index,
                prompt: if text.trim().is_empty() && !attachments.is_empty() {
                    "Attached images".into()
                } else {
                    clip(text, PREVIEW_PROMPT_CHARS)
                },
                reply: None,
            }),
            Entry::Assistant { markdown, .. } => {
                if let Some(tick) = ticks.last_mut()
                    && tick.reply.is_none()
                {
                    let source = markdown.read(cx).source();
                    if !source.trim().is_empty() {
                        tick.reply = Some(clip(source, PREVIEW_REPLY_CHARS));
                    }
                }
            }
            _ => {}
        }
    }
    ticks
}

pub(super) fn record_bounds(
    store: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    key: usize,
) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            store.borrow_mut().insert(key, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

impl ChatView {
    fn jump_to_entry(&mut self, entry_index: usize, cx: &mut Context<Self>) {
        let turn = self.turn_bounds.turns.borrow().get(&entry_index).copied();
        let container = *self.turn_bounds.container.borrow();
        let (Some(turn), Some(container)) = (turn, container) else {
            return;
        };
        let offset = self.scroll_handle.offset();
        let target = offset.y - (turn.origin.y - container.origin.y) + px(16.);
        self.scroll_handle.set_offset(point(offset.x, target.min(px(0.))));
        self.follow_tail = false;
        cx.notify();
    }

    pub(super) fn render_outline(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let container = (*self.turn_bounds.container.borrow())?;
        if f32::from(container.size.width) < MIN_CONTAINER_WIDTH {
            return None;
        }
        let ticks = ticks(self.entries(cx), cx);
        if ticks.len() < 2 {
            return None;
        }
        let colors = cx.theme().colors();
        let left = ((f32::from(container.size.width) - CONTENT_WIDTH) / 2. - 36.).max(8.);
        let hovered = self.outline_hover;
        Some(
            v_flex()
                .absolute()
                .left(px(left))
                .top_0()
                .bottom_0()
                .justify_center()
                .gap(px(6.))
                .children(ticks.into_iter().map(|tick| {
                    let entry_index = tick.entry_index;
                    let is_hovered = hovered == Some(entry_index);
                    let preview_prompt: SharedString = tick.prompt.into();
                    let preview_reply = tick.reply.map(SharedString::from);
                    div()
                        .id(("agent-outline", entry_index))
                        .relative()
                        .w(px(20.))
                        .h(px(6.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .child(
                            div()
                                .h(px(2.))
                                .rounded_full()
                                .w(px(if is_hovered { 16. } else { 10. }))
                                .bg(if is_hovered {
                                    colors.text
                                } else {
                                    colors.text_muted.opacity(0.45)
                                }),
                        )
                        .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                            if *hovering {
                                this.outline_hover = Some(entry_index);
                            } else if this.outline_hover == Some(entry_index) {
                                this.outline_hover = None;
                            }
                            cx.notify();
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.jump_to_entry(entry_index, cx)
                        }))
                        .when(is_hovered, |this| {
                            this.child(deferred(
                                div().absolute().left(px(26.)).top(px(-12.)).child(
                                    popover_card(cx)
                                        .w(px(300.))
                                        .p(px(10.))
                                        .gap(px(6.))
                                        .child(
                                            div()
                                                .text_size(ui(12.5))
                                                .text_color(colors.text)
                                                .child(preview_prompt.clone()),
                                        )
                                        .when_some(preview_reply.clone(), |this, reply| {
                                            this.child(
                                                div()
                                                    .text_size(ui(12.))
                                                    .text_color(colors.text_muted)
                                                    .child(reply),
                                            )
                                        }),
                                ),
                            ))
                        })
                }))
                .into_any_element(),
        )
    }
}
