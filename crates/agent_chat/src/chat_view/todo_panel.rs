use super::{
    ChatView,
    composer::{TRAY_COMPOSER_OVERLAP, TRAY_SIDE_INSET, tray_surface},
};
use crate::{
    chat_style::{accent, gradient_spinner, icon, text_faint},
    session::Entry,
};
use agent_harness::{TodoItem, TodoStatus, ToolCall};
use gpui::{
    AnyElement, App, Context, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled,
    div, px,
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    ops::Range,
};
use ui::{IconName, prelude::*};

const FOLD_ABOVE: usize = 6;
const FOCUS_WINDOW: usize = 3;
const HEADER_HEIGHT: f32 = 32.;
const ROW_PAD_X: f32 = 8.;
const ROW_RADIUS: f32 = 8.;
const TEXT_SIZE: f32 = 12.5;
const TEXT_LINE: f32 = 17.;
const GLYPH_SLOT: f32 = 14.;
const ACTION_SIZE: f32 = 24.;

#[derive(Default)]
pub(super) struct TodoPanelState {
    expanded: Option<bool>,
    show_earlier: bool,
    show_later: bool,
    dismissed: Option<DismissedTodo>,
    was_settled: bool,
    was_working: bool,
}

#[derive(Clone, Copy)]
struct DismissedTodo {
    signature: u64,
    finished: bool,
}

impl TodoPanelState {
    fn dismiss(&mut self, signature: u64, finished: bool) {
        self.dismissed = Some(DismissedTodo {
            signature,
            finished,
        });
    }

    fn hides(&self, signature: u64) -> bool {
        self.dismissed
            .is_some_and(|dismissed| dismissed.signature == signature)
    }

    fn observe_working(&mut self, working: bool) {
        if working && !self.was_working && self.dismissed.is_some_and(|dismissed| !dismissed.finished)
        {
            self.dismissed = None;
        }
        self.was_working = working;
    }
}

fn latest_todo(entries: &[Entry]) -> Option<Vec<TodoItem>> {
    let items = entries.iter().rev().find_map(|entry| match entry {
        Entry::Tool(tool) => match &tool.call {
            ToolCall::Todo { items } => Some(items),
            _ => None,
        },
        _ => None,
    })?;
    (!items.is_empty()).then(|| items.clone())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TodoSummary {
    total: usize,
    done: usize,
    active: Option<usize>,
    next: Option<usize>,
}

impl TodoSummary {
    fn of(items: &[TodoItem]) -> Self {
        let mut summary = Self {
            total: items.len(),
            done: 0,
            active: None,
            next: None,
        };
        for (index, item) in items.iter().enumerate() {
            match item.status() {
                TodoStatus::Completed => summary.done += 1,
                TodoStatus::InProgress => {
                    summary.active.get_or_insert(index);
                    summary.next.get_or_insert(index);
                }
                TodoStatus::Pending => {
                    summary.next.get_or_insert(index);
                }
            }
        }
        summary
    }

    fn finished(&self) -> bool {
        self.total > 0 && self.done == self.total
    }

    fn headline(&self) -> Option<usize> {
        self.active.or(self.next)
    }
}

fn focus_window(items: &[TodoItem]) -> Range<usize> {
    let total = items.len();
    if total <= FOLD_ABOVE {
        return 0..total;
    }
    let focus = TodoSummary::of(items).headline().unwrap_or(total - 1);
    let start = focus.saturating_sub(1).min(total - FOCUS_WINDOW);
    start..start + FOCUS_WINDOW
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fold {
    Earlier,
    Later,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Item(usize),
    Fold { side: Fold, count: usize, open: bool },
}

fn rows(items: &[TodoItem], show_earlier: bool, show_later: bool) -> Vec<Row> {
    let window = focus_window(items);
    let earlier = window.start;
    let later = items.len() - window.end;
    let mut rows = Vec::with_capacity(items.len() + 2);
    if earlier > 0 {
        rows.push(Row::Fold {
            side: Fold::Earlier,
            count: earlier,
            open: show_earlier,
        });
        if show_earlier {
            rows.extend((0..earlier).map(Row::Item));
        }
    }
    rows.extend(window.clone().map(Row::Item));
    if later > 0 {
        if show_later {
            rows.extend((window.end..items.len()).map(Row::Item));
        }
        rows.push(Row::Fold {
            side: Fold::Later,
            count: later,
            open: show_later,
        });
    }
    rows
}

fn signature(items: &[TodoItem]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for item in items {
        item.text.hash(&mut hasher);
        item.status().hash(&mut hasher);
    }
    hasher.finish()
}

impl ChatView {
    pub(super) fn observe_todo(&mut self, cx: &App) {
        let working = self.is_working(cx);
        let settled = !working
            && latest_todo(self.entries(cx))
                .is_some_and(|items| TodoSummary::of(&items).finished());
        let state = &mut self.todo_panel;
        state.observe_working(working);
        if settled && !state.was_settled {
            state.expanded = None;
        }
        state.was_settled = settled;
    }

    pub(super) fn render_todo_panel(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let items = latest_todo(self.entries(cx))?;
        let summary = TodoSummary::of(&items);
        let state = &self.todo_panel;
        let signature = signature(&items);
        if state.hides(signature) {
            return None;
        }
        let live = self.is_working(cx);
        let expanded = state.expanded.unwrap_or(!summary.finished());
        let colors = cx.theme().colors();
        let faint = text_faint(cx);
        let success = cx.theme().status().success;
        let hover = colors.element_hover;
        let headline: Option<(SharedString, Hsla)> = if summary.finished() {
            Some(("All done".into(), faint))
        } else {
            summary
                .headline()
                .and_then(|index| items.get(index))
                .map(|item| (SharedString::from(item.text.clone()), colors.text_muted))
        };
        let toggle = h_flex()
            .id("agent-todo-toggle")
            .flex_1()
            .min_w_0()
            .h(px(HEADER_HEIGHT))
            .px(px(ROW_PAD_X))
            .gap(px(8.))
            .rounded(px(ROW_RADIUS))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(if summary.finished() {
                icon(IconName::AgentCheck, px(14.), success)
            } else {
                icon(IconName::AgentChecklist, px(14.), colors.text_muted)
            })
            .child(
                h_flex()
                    .flex_none()
                    .items_baseline()
                    .gap(px(6.))
                    .text_size(px(TEXT_SIZE))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text)
                            .child("Todo"),
                    )
                    .child(
                        div()
                            .text_color(faint)
                            .child(format!("{}/{}", summary.done, summary.total)),
                    ),
            )
            .map(|this| match headline.filter(|_| !expanded) {
                Some((text, color)) => this.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(TEXT_SIZE))
                        .text_color(color)
                        .child(text),
                ),
                None => this.child(div().flex_1()),
            })
            .child(icon(
                if expanded {
                    IconName::AgentArrowDown
                } else {
                    IconName::AgentArrowUp
                },
                px(13.),
                colors.text_muted.opacity(0.7),
            ))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.todo_panel.expanded = Some(!expanded);
                cx.notify();
            }));
        let header = h_flex()
            .gap(px(2.))
            .child(toggle)
            .when(!live, |this| {
                this.child(
                    div()
                        .id("agent-todo-dismiss")
                        .size(px(ACTION_SIZE))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .child(icon(
                            IconName::AgentClose,
                            px(11.),
                            colors.text_muted.opacity(0.8),
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.todo_panel.dismiss(signature, summary.finished());
                            cx.notify();
                        })),
                )
            });
        let list = expanded.then(|| {
            v_flex().mt(px(2.)).pb(px(6.)).children(
                rows(&items, state.show_earlier, state.show_later)
                    .into_iter()
                    .map(|row| match row {
                        Row::Item(index) => {
                            let item = &items[index];
                            let status = item.status();
                            let (color, weight) = match status {
                                TodoStatus::Completed => (faint, FontWeight::NORMAL),
                                TodoStatus::InProgress => (colors.text, FontWeight::MEDIUM),
                                TodoStatus::Pending => (colors.text_muted, FontWeight::NORMAL),
                            };
                            h_flex()
                                .items_start()
                                .min_h(px(26.))
                                .px(px(ROW_PAD_X))
                                .py(px(4.))
                                .gap(px(8.))
                                .child(
                                    div()
                                        .w(px(GLYPH_SLOT))
                                        .h(px(TEXT_LINE))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(status_glyph(index, status, live, cx)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(TEXT_SIZE))
                                        .line_height(px(TEXT_LINE))
                                        .font_weight(weight)
                                        .text_color(color)
                                        .child(item.text.clone()),
                                )
                                .into_any_element()
                        }
                        Row::Fold { side, count, open } => {
                            let noun = match side {
                                Fold::Earlier => "earlier",
                                Fold::Later => "later",
                            };
                            let glyph = match (side, open) {
                                (Fold::Earlier, false) | (Fold::Later, true) => {
                                    IconName::AgentArrowUp
                                }
                                _ => IconName::AgentArrowDown,
                            };
                            h_flex()
                                .id(match side {
                                    Fold::Earlier => "agent-todo-earlier",
                                    Fold::Later => "agent-todo-later",
                                })
                                .h(px(24.))
                                .px(px(ROW_PAD_X))
                                .gap(px(8.))
                                .rounded(px(6.))
                                .cursor_pointer()
                                .hover(move |style| style.bg(hover))
                                .child(
                                    div()
                                        .w(px(GLYPH_SLOT))
                                        .flex_none()
                                        .flex()
                                        .justify_center()
                                        .child(icon(glyph, px(12.), faint)),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(faint)
                                        .child(format!("{count} {noun}")),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    match side {
                                        Fold::Earlier => this.todo_panel.show_earlier = !open,
                                        Fold::Later => this.todo_panel.show_later = !open,
                                    }
                                    cx.notify();
                                }))
                                .into_any_element()
                        }
                    }),
            )
        });
        let has_queue = !self.session.read(cx).queue().is_empty();
        let inset = if has_queue {
            TRAY_SIDE_INSET * 2.
        } else {
            TRAY_SIDE_INSET
        };
        Some(
            div()
                .mx(px(inset))
                .mb(px(-TRAY_COMPOSER_OVERLAP))
                .child(tray_surface(cx).child(header).children(list))
                .into_any_element(),
        )
    }
}

fn status_glyph(index: usize, status: TodoStatus, live: bool, cx: &App) -> AnyElement {
    match status {
        TodoStatus::Completed => {
            icon(IconName::AgentCheck, px(12.), cx.theme().status().success).into_any_element()
        }
        TodoStatus::InProgress if live => {
            gradient_spinner(format!("agent-todo-active-{index}").into(), 2.5).into_any_element()
        }
        TodoStatus::InProgress => div()
            .size(px(10.))
            .rounded_full()
            .border_1()
            .border_color(accent(cx))
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(4.)).rounded_full().bg(accent(cx)))
            .into_any_element(),
        TodoStatus::Pending => div()
            .size(px(10.))
            .rounded_full()
            .border_1()
            .border_color(text_faint(cx).opacity(0.7))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(statuses: &[TodoStatus]) -> Vec<TodoItem> {
        statuses
            .iter()
            .enumerate()
            .map(|(index, status)| TodoItem::new(format!("step {index}"), *status))
            .collect()
    }

    #[test]
    fn long_lists_fold_around_the_current_item() {
        use TodoStatus::*;
        let list = items(&[Completed, Completed, Completed, InProgress, Pending, Pending, Pending, Pending]);
        assert_eq!(focus_window(&list), 2..5);
        assert_eq!(
            rows(&list, false, false),
            vec![
                Row::Fold { side: Fold::Earlier, count: 2, open: false },
                Row::Item(2),
                Row::Item(3),
                Row::Item(4),
                Row::Fold { side: Fold::Later, count: 3, open: false },
            ]
        );
        let short = items(&[Pending, Pending]);
        assert_eq!(rows(&short, false, false), vec![Row::Item(0), Row::Item(1)]);
        assert!(TodoSummary::of(&items(&[Completed, Completed])).finished());
    }

    #[test]
    fn a_dismissed_unfinished_list_returns_when_the_agent_works_again() {
        use TodoStatus::*;
        let unfinished = signature(&items(&[Completed, Completed, Pending, Pending, Pending]));
        let mut state = TodoPanelState::default();
        state.dismiss(unfinished, false);
        state.observe_working(false);
        assert!(state.hides(unfinished));
        state.observe_working(true);
        assert!(!state.hides(unfinished));

        let finished = signature(&items(&[Completed, Completed]));
        state.observe_working(false);
        state.dismiss(finished, true);
        state.observe_working(true);
        state.observe_working(false);
        assert!(state.hides(finished));
        let changed = signature(&items(&[Completed, Pending]));
        assert!(!state.hides(changed));
    }
}
