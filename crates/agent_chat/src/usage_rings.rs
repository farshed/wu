use crate::{
    AgentKind,
    chat_style::{accent, hairline, popover_card, selected_row, text_faint, ui, wash},
    session::{AgentStore, ContextSnapshot},
};
use agent_harness::usage::UsageWindow;
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Hsla, IntoElement, ParentElement, PathBuilder, Render, SharedString, Styled,
    Subscription, Window, canvas, div, point, px, relative,
};
use theme::ActiveTheme as _;
use ui::prelude::*;

const CONTEXT_WARNING: f32 = 0.75;
const CONTEXT_DANGER: f32 = 0.9;
const USAGE_WARNING: f32 = 0.8;
const USAGE_DANGER: f32 = 0.95;
const USAGE_LABEL_WIDTH: f32 = 52.0;
const USAGE_BAR_WIDTH: f32 = 88.0;
const USAGE_PERCENT_WIDTH: f32 = 34.0;
const USAGE_CARD_WIDTH: f32 = 400.0;
const CARD_INSET: f32 = 4.0;
const MENU_ITEM_RADIUS: f32 = 7.0;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum UsageLevel {
    Normal,
    Warn,
    Critical,
}

fn usage_level(fraction: f32) -> UsageLevel {
    if fraction >= USAGE_DANGER {
        UsageLevel::Critical
    } else if fraction >= USAGE_WARNING {
        UsageLevel::Warn
    } else {
        UsageLevel::Normal
    }
}

fn usage_color(level: UsageLevel, cx: &App) -> Hsla {
    match level {
        UsageLevel::Normal => accent(cx),
        UsageLevel::Warn => cx.theme().status().warning,
        UsageLevel::Critical => cx.theme().status().error,
    }
}

fn usage_text_color(level: UsageLevel, cx: &App) -> Hsla {
    match level {
        UsageLevel::Normal => cx.theme().colors().text_muted,
        _ => usage_color(level, cx),
    }
}

pub(crate) fn usage_chip(fraction: Option<f32>, cx: &App) -> gpui::Div {
    let level = fraction.map(usage_level);
    let arc = usage_color(level.unwrap_or(UsageLevel::Normal), cx);
    let text = match level {
        Some(level) => usage_text_color(level, cx),
        None => text_faint(cx),
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

fn tracked_upper(label: &str) -> String {
    let mut tracked = String::with_capacity(label.len() * 2);
    for (index, character) in label.to_uppercase().chars().enumerate() {
        if index > 0 {
            tracked.push('\u{200A}');
        }
        tracked.push(character);
    }
    tracked
}

fn menu_heading(label: &str, cx: &App) -> impl IntoElement {
    div()
        .px(px(8.))
        .pt(px(6.))
        .pb(px(4.))
        .text_size(ui(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(cx.theme().colors().text_muted)
        .child(tracked_upper(label))
}

fn menu_separator(cx: &App) -> impl IntoElement {
    div()
        .h(px(1.))
        .mx(px(-CARD_INSET))
        .my(px(2.))
        .bg(hairline(0.07, cx))
}

fn usage_meter(window: &UsageWindow, cx: &App) -> gpui::Div {
    let fraction = window.used_fraction.clamp(0.0, 1.0);
    let level = usage_level(fraction);
    let fill = usage_color(level, cx).opacity(match level {
        UsageLevel::Normal => 0.8,
        _ => 0.9,
    });
    h_flex()
        .h(px(16.))
        .gap(px(8.))
        .text_size(ui(11.5))
        .child(
            div()
                .w(px(USAGE_LABEL_WIDTH))
                .flex_none()
                .truncate()
                .text_color(cx.theme().colors().text_muted)
                .child(window.label.clone()),
        )
        .child(
            div()
                .w(px(USAGE_BAR_WIDTH))
                .flex_none()
                .h(px(4.))
                .rounded_full()
                .overflow_hidden()
                .bg(wash(0.08, cx))
                .when(fraction > 0.0, |this| {
                    this.child(
                        div()
                            .h_full()
                            .w(relative(fraction.max(0.015)))
                            .rounded_full()
                            .bg(fill),
                    )
                }),
        )
        .child(
            div()
                .w(px(USAGE_PERCENT_WIDTH))
                .flex_none()
                .text_right()
                .text_color(usage_text_color(level, cx))
                .child(format!("{}%", (fraction * 100.0).round() as u32)),
        )
}

fn meta_line(fragments: Vec<AnyElement>, cx: &App) -> gpui::Div {
    let muted = cx.theme().colors().text_muted;
    let mut line = h_flex()
        .mt(px(1.))
        .min_w_0()
        .max_w_full()
        .flex_wrap()
        .gap_x(px(8.))
        .gap_y(px(2.))
        .text_size(ui(12.))
        .line_height(ui(16.))
        .text_color(muted);
    for (index, fragment) in fragments.into_iter().enumerate() {
        if index > 0 {
            line = line.child(div().text_color(muted.opacity(0.3)).child("·"));
        }
        line = line.child(div().min_w_0().max_w_full().child(fragment));
    }
    line
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
                    .text_size(px(12.))
                    .line_height(px(19.))
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
        store.update(cx, |store, cx| {
            store.refresh_plan_usage(kind, true, cx);
            store.refresh_accounts(kind, cx);
        });
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
            .w(px(USAGE_CARD_WIDTH))
            .child(menu_heading(&plan_label, cx))
            .child(
                v_flex()
                    .px(px(8.))
                    .pb(px(6.))
                    .gap(px(2.))
                    .children(
                        usage
                            .iter()
                            .flat_map(|usage| usage.windows.iter())
                            .map(|window| {
                                usage_meter(window, cx).when_some(
                                    window.resets_at.and_then(resets_in),
                                    |this, resets| {
                                        this.child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
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
                                .text_size(px(12.))
                                .text_color(colors.text_muted)
                                .child(error),
                        )
                    })
                    .when(loading && usage.is_none(), |this| {
                        this.child(
                            div()
                                .text_size(px(12.))
                                .text_color(colors.text_muted)
                                .child("Loading…"),
                        )
                    }),
            )
            .children(self.render_accounts(cx).unwrap_or_default())
    }
}

impl UsageCard {
    fn render_accounts(&self, cx: &Context<Self>) -> Option<Vec<AnyElement>> {
        let state = self.store.read(cx).accounts(self.kind)?;
        if state.accounts.is_empty() && state.error.is_none() {
            return None;
        }
        let colors = cx.theme().colors();
        let switching = state.switching.clone();
        let rows = state.accounts.iter().map(|account| {
            let account_usage = state.usage.get(&account.id);
            let windows: Vec<UsageWindow> = if account.active {
                Vec::new()
            } else {
                account_usage
                    .and_then(|usage| usage.as_ref().ok())
                    .map(|usage| usage.windows.iter().take(2).cloned().collect())
                    .unwrap_or_default()
            };
            let usage_error = account_usage
                .and_then(|usage| usage.as_ref().err())
                .filter(|_| !account.active)
                .cloned();
            let mut meta: Vec<AnyElement> = account
                .plan
                .iter()
                .map(|plan| div().child(plan.clone()).into_any_element())
                .collect();
            if account.active {
                meta.push(div().text_color(accent(cx)).child("In use").into_any_element());
            } else if let Some(reason) = usage_error {
                meta.push(div().child(reason).into_any_element());
            }
            if switching.as_deref() == Some(account.id.as_str()) {
                meta.push(div().child("Switching…").into_any_element());
            }
            let account_id = account.id.clone();
            let active = account.active;
            let text = colors.text;
            let hover = selected_row(cx);
            h_flex()
                .id(SharedString::from(format!("agent-account-{}", account.id)))
                .gap(px(10.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(MENU_ITEM_RADIUS))
                .text_size(ui(13.))
                .map(|this| {
                    if active {
                        this.bg(hover).text_color(text)
                    } else {
                        this.cursor_pointer()
                            .text_color(text.opacity(0.9))
                            .hover(move |style| style.bg(hover).text_color(text))
                    }
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .child(account.label.clone()),
                        )
                        .when(!meta.is_empty(), |this| this.child(meta_line(meta, cx))),
                )
                .child(
                    v_flex()
                        .flex_none()
                        .gap(px(2.))
                        .children(windows.iter().map(|window| usage_meter(window, cx))),
                )
                .when(!active, |this| {
                    this.on_click(cx.listener(move |this, _, _, cx| {
                        let account_id = account_id.clone();
                        this.store.update(cx, |store, cx| {
                            store.activate_account(this.kind, account_id, cx)
                        });
                    }))
                })
                .into_any_element()
        });
        let mut elements = vec![
            menu_separator(cx).into_any_element(),
            menu_heading(&format!("{} accounts", self.kind.label()), cx).into_any_element(),
        ];
        elements.extend(rows);
        if let Some(error) = state.error.clone() {
            elements.push(
                div()
                    .px(px(8.))
                    .py(px(4.))
                    .text_size(px(12.))
                    .text_color(cx.theme().status().error)
                    .child(error)
                    .into_any_element(),
            );
        }
        elements.push(
            div()
                .px(px(8.))
                .pb(px(6.))
                .text_size(ui(11.))
                .text_color(text_faint(cx))
                .child("Sign in to another account in the CLI and Wu remembers it here.")
                .into_any_element(),
        );
        Some(elements)
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
