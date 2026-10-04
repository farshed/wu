use super::{ChatView, ui};
use crate::{
    chat_style::{ink, popover_card},
    session::Entry,
};
use gpui::{
    Anchor, AnyElement, Context, IntoElement, ListOffset, ParentElement, SharedString, Styled,
    anchored, deferred, div, px,
};
use ui::prelude::*;

const MIN_CONTAINER_WIDTH: f32 = 768.;
const PREVIEW_PROMPT_CHARS: usize = 160;
const PREVIEW_REPLY_CHARS: usize = 200;
const TICK_SLOT: f32 = 10.;
const TICK_GAP: f32 = 3.;
const RAIL_VERTICAL_MARGIN: f32 = 24.;
const MAX_RAIL_TICKS: usize = 12;
const READING_INSET: f32 = 16.;

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

fn rail_slots(height: f32) -> usize {
    let usable = (height - 2. * RAIL_VERTICAL_MARGIN).max(TICK_SLOT);
    (((usable + TICK_GAP) / (TICK_SLOT + TICK_GAP)).floor() as usize).clamp(1, MAX_RAIL_TICKS)
}

fn tick_buckets(count: usize, capacity: usize) -> Vec<(usize, usize)> {
    if count == 0 {
        return Vec::new();
    }
    let capacity = capacity.clamp(1, count);
    (0..capacity)
        .map(|bucket| (bucket * count / capacity, (bucket + 1) * count / capacity))
        .collect()
}

impl ChatView {
    fn jump_to_entry(&mut self, entry_index: usize, cx: &mut Context<Self>) {
        let Some(turn_index) = self
            .turns
            .iter()
            .position(|turn| turn.user == Some(entry_index))
        else {
            return;
        };
        self.list.scroll_to(ListOffset {
            item_ix: turn_index,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    fn active_tick(&self, ticks: &[Tick]) -> Option<usize> {
        let reading_line = self.scroll_top() + px(READING_INSET + 0.5);
        let top_turn = (0..self.turns.len())
            .take_while(|index| self.list.offset_for_item(*index) <= reading_line)
            .last()?;
        let last_entry = self.turns[top_turn].user.or_else(|| self.turns[top_turn].items.first().copied())?;
        Some(
            ticks
                .iter()
                .rposition(|tick| tick.entry_index <= last_entry)
                .unwrap_or(0),
        )
    }

    pub(super) fn render_outline(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let container = self.container_bounds.get()?;
        if f32::from(container.size.width) < MIN_CONTAINER_WIDTH {
            return None;
        }
        let ticks = ticks(self.entries(cx), cx);
        if ticks.len() < 2 {
            return None;
        }
        let colors = cx.theme().colors();
        let active = self.active_tick(&ticks);
        let buckets = tick_buckets(ticks.len(), rail_slots(f32::from(container.size.height)));
        let active_bucket = active.and_then(|active| {
            buckets
                .iter()
                .position(|(start, end)| active >= *start && active < *end)
        });
        let hovered = self.outline_hover;
        let rest_color = ink(0.16, cx);
        let lit_color = colors.text.opacity(0.8);
        Some(
            v_flex()
                .absolute()
                .left(px(16.))
                .top_0()
                .bottom_0()
                .w(px(26.))
                .items_start()
                .justify_center()
                .gap(px(TICK_GAP))
                .children(buckets.into_iter().enumerate().map(|(bucket, (start, end))| {
                    let representative = active
                        .filter(|active| *active >= start && *active < end)
                        .unwrap_or(start);
                    let tick = &ticks[representative];
                    let entry_index = tick.entry_index;
                    let bucket_len = end - start;
                    let is_hovered = hovered == Some(bucket);
                    let is_active = active_bucket == Some(bucket);
                    let preview_prompt: SharedString = tick.prompt.clone().into();
                    let preview_reply = tick.reply.clone().map(SharedString::from);
                    div()
                        .id(("agent-outline", bucket))
                        .relative()
                        .h(px(TICK_SLOT))
                        .w_full()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .child(
                            div()
                                .h(px(2.))
                                .w(px(if is_hovered { 20. } else { 12. }))
                                .rounded(px(1.))
                                .bg(if is_active || is_hovered {
                                    lit_color
                                } else {
                                    rest_color
                                }),
                        )
                        .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                            if *hovering {
                                this.outline_hover = Some(bucket);
                            } else if this.outline_hover == Some(bucket) {
                                this.outline_hover = None;
                            }
                            cx.notify();
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.jump_to_entry(entry_index, cx)
                        }))
                        .when(is_hovered, |this| {
                            this.child(deferred(
                                anchored()
                                    .anchor(Anchor::LeftCenter)
                                    .snap_to_window_with_margin(px(8.))
                                    .child(
                                        div().pl(px(26.)).child(
                                            popover_card(cx)
                                                .w(px(280.))
                                                .p(px(8.))
                                                .gap(px(6.))
                                                .child(
                                                    div()
                                                        .text_size(ui(12.))
                                                        .text_color(colors.text)
                                                        .child(preview_prompt),
                                                )
                                                .when_some(preview_reply, |this, reply| {
                                                    this.child(
                                                        div()
                                                            .text_size(ui(11.))
                                                            .text_color(colors.text_muted)
                                                            .child(reply),
                                                    )
                                                })
                                                .when(bucket_len > 1, |this| {
                                                    this.child(
                                                        div()
                                                            .text_size(ui(10.))
                                                            .text_color(colors.text_muted)
                                                            .child(format!(
                                                                "{bucket_len} prompts"
                                                            )),
                                                    )
                                                }),
                                        ),
                                    ),
                            ))
                        })
                }))
                .into_any_element(),
        )
    }
}
