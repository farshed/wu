use super::{ChatView, ui};
use crate::{chat_style::ink, session::Entry};
use agent_harness::{TodoItem, TodoStatus, ToolCall};
use gpui::{
    AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, div,
    px,
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    ops::Range,
};
use ui::{Icon, IconName, IconSize, prelude::*};

const FOLD_ABOVE: usize = 6;
const FOCUS_WINDOW: usize = 3;

#[derive(Default)]
pub(super) struct TodoPanelState {
    expanded: Option<bool>,
    show_earlier: bool,
    show_later: bool,
    dismissed: Option<u64>,
    was_settled: bool,
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
        let settled = !self.is_working(cx)
            && latest_todo(self.entries(cx))
                .is_some_and(|items| TodoSummary::of(&items).finished());
        let state = &mut self.todo_panel;
        if settled && !state.was_settled {
            state.expanded = None;
        }
        state.was_settled = settled;
    }

    pub(super) fn render_todo_panel(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let items = latest_todo(self.entries(cx))?;
        let summary = TodoSummary::of(&items);
        let state = &self.todo_panel;
        if summary.finished() && state.dismissed == Some(signature(&items)) {
            return None;
        }
        let expanded = state.expanded.unwrap_or(!summary.finished());
        let colors = cx.theme().colors();
        let headline: SharedString = if summary.finished() {
            "All done".into()
        } else {
            summary
                .headline()
                .and_then(|index| items.get(index))
                .map(|item| SharedString::from(item.text.clone()))
                .unwrap_or_default()
        };
        let signature = signature(&items);
        let header = h_flex()
            .id("agent-todo-header")
            .h(px(30.))
            .px(px(10.))
            .gap(px(8.))
            .cursor_pointer()
            .child(
                Icon::new(IconName::AgentChecklist)
                    .size(IconSize::Small)
                    .color(if summary.finished() {
                        Color::Success
                    } else {
                        Color::Muted
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(ui(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.text_muted)
                    .child(format!("{} of {}", summary.done, summary.total)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui(12.5))
                    .text_color(colors.text)
                    .child(headline),
            )
            .child(
                Icon::new(if expanded {
                    IconName::AgentArrowDown
                } else {
                    IconName::AgentArrowUp
                })
                .size(IconSize::XSmall)
                .color(Color::Muted),
            )
            .when(summary.finished(), |this| {
                this.child(
                    div()
                        .id("agent-todo-dismiss")
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.))
                        .hover(|style| style.bg(ink(0.08, cx)))
                        .child(
                            Icon::new(IconName::AgentClose)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.todo_panel.dismissed = Some(signature);
                            cx.notify();
                        })),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.todo_panel.expanded = Some(!expanded);
                cx.notify();
            }));
        let list = expanded.then(|| {
            v_flex()
                .px(px(10.))
                .pb(px(8.))
                .gap(px(2.))
                .children(
                    rows(&items, state.show_earlier, state.show_later)
                        .into_iter()
                        .map(|row| match row {
                            Row::Item(index) => {
                                let item = &items[index];
                                let status = item.status();
                                let (icon, color) = match status {
                                    TodoStatus::Completed => (IconName::AgentCheck, Color::Success),
                                    TodoStatus::InProgress => (IconName::AgentArrowRight, Color::Accent),
                                    TodoStatus::Pending => (IconName::Circle, Color::Muted),
                                };
                                h_flex()
                                    .gap(px(8.))
                                    .min_h(px(22.))
                                    .child(
                                        div()
                                            .flex_none()
                                            .size(px(14.))
                                            .child(Icon::new(icon).size(IconSize::XSmall).color(color)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(ui(12.5))
                                            .text_color(match status {
                                                TodoStatus::Completed => colors.text_muted,
                                                TodoStatus::InProgress => colors.text,
                                                TodoStatus::Pending => colors.text.opacity(0.85),
                                            })
                                            .when(status == TodoStatus::Completed, |this| {
                                                this.line_through()
                                            })
                                            .child(item.text.clone()),
                                    )
                                    .into_any_element()
                            }
                            Row::Fold { side, count, open } => div()
                                .id(match side {
                                    Fold::Earlier => "agent-todo-earlier",
                                    Fold::Later => "agent-todo-later",
                                })
                                .pl(px(22.))
                                .cursor_pointer()
                                .text_size(ui(11.5))
                                .text_color(colors.text_muted)
                                .hover(|style| style.text_color(colors.text))
                                .child(match (side, open) {
                                    (Fold::Earlier, false) => format!("{count} earlier"),
                                    (Fold::Later, false) => format!("{count} later"),
                                    (_, true) => "Show less".to_string(),
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    match side {
                                        Fold::Earlier => {
                                            this.todo_panel.show_earlier = !open
                                        }
                                        Fold::Later => this.todo_panel.show_later = !open,
                                    }
                                    cx.notify();
                                }))
                                .into_any_element(),
                        }),
                )
        });
        Some(
            v_flex()
                .mb(px(6.))
                .rounded(px(12.))
                .border_1()
                .border_color(colors.border)
                .bg(colors.elevated_surface_background)
                .child(header)
                .children(list)
                .into_any_element(),
        )
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
}
