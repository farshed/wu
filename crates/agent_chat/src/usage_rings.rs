use crate::{
    AgentKind,
    chat_style::{accent, ink, text_faint, ui},
    session::{AgentStore, ContextSnapshot},
};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    IntoElement, ParentElement, PathBuilder, Render, SharedString, Styled, Subscription, Window,
    canvas, div, point, px, relative,
};
use theme::ActiveTheme as _;
use ui::prelude::*;

const CONTEXT_WARNING: f32 = 0.75;
const CONTEXT_DANGER: f32 = 0.9;
const USAGE_WARNING: f32 = 0.8;
const USAGE_DANGER: f32 = 0.95;

fn ring(fraction: f32, color: Hsla, cx: &App) -> impl IntoElement {
    let track = text_faint(cx).opacity(0.25);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let mut arc = |fraction: f32, color| {
                if fraction <= 0.0 {
                    return;
                }
                let steps = (64.0 * fraction).ceil().max(2.0) as usize;
                let mut path = PathBuilder::stroke(px(1.8));
                for i in 0..=steps {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * fraction * i as f32 / steps as f32;
                    let p = point(
                        center.x + px(6.0 * angle.cos()),
                        center.y + px(6.0 * angle.sin()),
                    );
                    if i == 0 {
                        path.move_to(p);
                    } else {
                        path.line_to(p);
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            arc(1.0, track);
            arc(fraction.clamp(0.0, 1.0), color);
        },
    )
    .size(px(16.0))
}

fn ring_chip_content(fraction: Option<f32>, arc: Hsla, text: Hsla, cx: &App) -> gpui::Div {
    let label: SharedString = fraction
        .map(|fraction| format!("{:.0}%", fraction * 100.0))
        .unwrap_or_else(|| "–".into())
        .into();
    div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .h(px(24.0))
        .px(px(6.0))
        .text_size(px(11.0))
        .text_color(text)
        .child(ring(fraction.unwrap_or(0.0), arc, cx))
        .child(label)
}

pub(crate) fn context_chip(fraction: Option<f32>, cx: &App) -> gpui::Div {
    let status = cx.theme().status();
    let color = match fraction {
        Some(fraction) if fraction >= CONTEXT_DANGER => status.error,
        Some(fraction) if fraction >= CONTEXT_WARNING => status.warning,
        Some(_) => cx.theme().colors().text_muted,
        None => text_faint(cx),
    };
    ring_chip_content(fraction, color, color, cx)
}

pub(crate) fn usage_chip(fraction: Option<f32>, cx: &App) -> gpui::Div {
    let status = cx.theme().status();
    let arc = match fraction {
        Some(fraction) if fraction >= USAGE_DANGER => status.error,
        Some(fraction) if fraction >= USAGE_WARNING => status.warning,
        _ => accent(cx),
    };
    let text = if fraction.is_some() {
        cx.theme().colors().text_muted
    } else {
        text_faint(cx)
    };
    ring_chip_content(fraction, arc, text, cx)
}

fn with_separators(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

pub(crate) fn context_details(context: Option<ContextSnapshot>) -> String {
    match context.unwrap_or_default() {
        ContextSnapshot {
            tokens: Some(tokens),
            window: Some(window),
        } if window > 0 => format!(
            "{} / {} tokens\n{} tokens remaining",
            with_separators(tokens),
            with_separators(window),
            with_separators(window.saturating_sub(tokens))
        ),
        ContextSnapshot {
            tokens: Some(tokens),
            ..
        } => format!(
            "{} tokens used\nContext limit not reported",
            with_separators(tokens)
        ),
        ContextSnapshot {
            window: Some(window),
            ..
        } if window > 0 => format!(
            "{} token capacity\nWaiting for context usage",
            with_separators(window)
        ),
        _ => "Context usage not reported by this agent yet".into(),
    }
}

fn popover_card(cx: &App) -> gpui::Div {
    let colors = cx.theme().colors();
    div()
        .flex()
        .flex_col()
        .border_1()
        .border_color(colors.border)
        .rounded(px(12.))
        .shadow_lg()
        .bg(colors.elevated_surface_background)
        .p(px(4.))
        .gap(px(2.))
        .overflow_hidden()
        .text_size(ui(13.))
        .text_color(colors.text)
}

fn menu_heading(label: &str, cx: &App) -> impl IntoElement {
    div()
        .px(px(8.))
        .pt(px(6.))
        .pb(px(4.))
        .text_size(ui(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(cx.theme().colors().text_muted)
        .child(label.to_uppercase())
}

fn resets_in(resets_at: chrono::DateTime<chrono::Utc>) -> Option<String> {
    let minutes = (resets_at - chrono::Utc::now()).num_minutes();
    if minutes <= 0 {
        return None;
    }
    Some(
        match (minutes / (60 * 24), minutes / 60 % 24, minutes % 60) {
            (0, 0, minutes) => format!("Resets in {minutes}m"),
            (0, hours, minutes) => format!("Resets in {hours}h {minutes}m"),
            (days, hours, _) => format!("Resets in {days}d {hours}h"),
        },
    )
}

pub struct ContextCard {
    details: String,
    focus_handle: FocusHandle,
}

impl ContextCard {
    pub fn new(context: Option<ContextSnapshot>, cx: &mut Context<Self>) -> Self {
        Self {
            details: context_details(context),
            focus_handle: cx.focus_handle(),
        }
    }
}

impl EventEmitter<DismissEvent> for ContextCard {}

impl Focusable for ContextCard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ContextCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        popover_card(cx)
            .key_context("menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .child(menu_heading("Context window", cx))
            .child(
                div()
                    .px(px(8.))
                    .pb(px(6.))
                    .text_size(ui(12.))
                    .line_height(ui(19.))
                    .whitespace_nowrap()
                    .text_color(cx.theme().colors().text_muted)
                    .children(
                        self.details
                            .lines()
                            .map(|line| div().child(SharedString::from(line.to_string()))),
                    ),
            )
    }
}

pub struct UsageCard {
    store: Entity<AgentStore>,
    kind: AgentKind,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl UsageCard {
    pub fn new(store: Entity<AgentStore>, kind: AgentKind, cx: &mut Context<Self>) -> Self {
        store.update(cx, |store, cx| store.refresh_plan_usage(kind, true, cx));
        Self {
            _subscription: cx.observe(&store, |_, _, cx| cx.notify()),
            store,
            kind,
            focus_handle: cx.focus_handle(),
        }
    }
}

impl EventEmitter<DismissEvent> for UsageCard {}

impl Focusable for UsageCard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for UsageCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let status = cx.theme().status();
        let state = self.store.read(cx).plan_usage(self.kind);
        let usage = state.and_then(|state| state.usage.clone());
        let error = state.and_then(|state| state.error.clone());
        let loading = state.is_some_and(|state| state.loading);
        let plan_label = usage
            .as_ref()
            .and_then(|usage| usage.plan_label.clone())
            .unwrap_or_else(|| self.kind.label().to_string());
        popover_card(cx)
            .key_context("menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .w(px(240.))
            .child(menu_heading(&plan_label, cx))
            .child(
                v_flex()
                    .px(px(8.))
                    .pb(px(6.))
                    .gap(px(10.))
                    .children(
                        usage
                            .iter()
                            .flat_map(|usage| usage.windows.iter())
                            .map(|window| {
                                let fraction = window.used_fraction.clamp(0.0, 1.0);
                                let color = if fraction >= USAGE_DANGER {
                                    status.error
                                } else if fraction >= USAGE_WARNING {
                                    status.warning
                                } else {
                                    accent(cx)
                                };
                                v_flex()
                                    .gap(px(4.))
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .text_size(ui(12.))
                                            .child(window.label.clone())
                                            .child(
                                                div()
                                                    .text_color(colors.text_muted)
                                                    .child(format!("{:.0}%", fraction * 100.0)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .h(px(4.))
                                            .w_full()
                                            .rounded_full()
                                            .bg(ink(0.08, cx))
                                            .child(
                                                div()
                                                    .h_full()
                                                    .w(relative(fraction))
                                                    .rounded_full()
                                                    .bg(color),
                                            ),
                                    )
                                    .when_some(
                                        window.resets_at.and_then(resets_in),
                                        |this, resets| {
                                            this.child(
                                                div()
                                                    .text_size(ui(11.))
                                                    .text_color(text_faint(cx))
                                                    .child(resets),
                                            )
                                        },
                                    )
                            }),
                    )
                    .when_some(error, |this, error| {
                        this.child(
                            div()
                                .text_size(ui(12.))
                                .text_color(colors.text_muted)
                                .child(error),
                        )
                    })
                    .when(loading && usage.is_none(), |this| {
                        this.child(
                            div()
                                .text_size(ui(12.))
                                .text_color(colors.text_muted)
                                .child("Loading…"),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_are_grouped_by_thousands() {
        assert_eq!(with_separators(0), "0");
        assert_eq!(with_separators(999), "999");
        assert_eq!(with_separators(5417), "5,417");
        assert_eq!(with_separators(1_048_576), "1,048,576");
    }

    #[test]
    fn missing_usage_is_distinct_from_zero_and_overflow() {
        assert!(context_details(None).contains("not reported"));
        assert!(
            context_details(Some(ContextSnapshot {
                tokens: Some(0),
                window: Some(200)
            }))
            .contains("200 tokens remaining")
        );
        assert!(
            context_details(Some(ContextSnapshot {
                tokens: Some(250),
                window: Some(200)
            }))
            .contains("0 tokens remaining")
        );
        assert_eq!(
            context_details(Some(ContextSnapshot {
                tokens: Some(5417),
                window: Some(1_048_576)
            })),
            "5,417 / 1,048,576 tokens\n1,043,159 tokens remaining"
        );
    }
}
