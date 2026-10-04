use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClickEvent, CursorStyle, ElementId, Hsla,
    IntoElement, ParentElement, Rems, RenderOnce, Styled, Window, div, hsla, px, rems, rgb,
};
use std::time::Duration;
use theme::ActiveTheme as _;
use ui::{Clickable, Toggleable, Tooltip, prelude::*};

pub(crate) fn ui(pixels_at_default: f32) -> Rems {
    rems(pixels_at_default / 16.0)
}

const INK_HAIRLINE_SCALE: f32 = 1.35;

fn is_dark(cx: &App) -> bool {
    !cx.theme().appearance.is_light()
}

pub(crate) fn ink(alpha: f32, cx: &App) -> Hsla {
    if is_dark(cx) {
        hsla(0.0, 0.0, 1.0, alpha)
    } else {
        hsla(0.0, 0.0, 0.0, alpha)
    }
}

/// Hairline ink, scaled up on light so 1px edges stay legible.
pub(crate) fn hairline(alpha: f32, cx: &App) -> Hsla {
    if is_dark(cx) {
        hsla(0.0, 0.0, 1.0, alpha)
    } else {
        hsla(0.0, 0.0, 0.0, (alpha * INK_HAIRLINE_SCALE).min(0.5))
    }
}

pub(crate) fn wash(alpha: f32, cx: &App) -> Hsla {
    if is_dark(cx) {
        hsla(0.0, 0.0, 0.92, alpha)
    } else {
        hsla(0.0, 0.0, 0.10, alpha)
    }
}

pub(crate) fn page(cx: &App) -> Hsla {
    cx.theme().colors().background.opacity(1.0)
}

pub(crate) fn mix(from: Hsla, to: Hsla, amount: f32) -> Hsla {
    let from = from.to_rgb();
    let to = to.to_rgb();
    let amount = amount.clamp(0.0, 1.0);
    gpui::Rgba {
        r: lerp(from.r, to.r, amount),
        g: lerp(from.g, to.g, amount),
        b: lerp(from.b, to.b, amount),
        a: lerp(from.a, to.a, amount),
    }
    .into()
}

pub(crate) fn text_faint(cx: &App) -> Hsla {
    cx.theme().colors().text_placeholder
}

pub(crate) fn accent(cx: &App) -> Hsla {
    cx.theme().colors().text_accent
}

pub(crate) fn selected_row(cx: &App) -> Hsla {
    if is_dark(cx) {
        wash(0.11, cx)
    } else {
        wash(0.06, cx)
    }
}

pub(crate) fn popover_card(cx: &App) -> gpui::Div {
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

#[derive(IntoElement)]
pub(crate) struct Chip {
    id: ElementId,
    child: AnyElement,
    radius: f32,
    selected: bool,
    tooltip: Option<SharedString>,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
}

impl Chip {
    pub fn new(id: impl Into<ElementId>, radius: f32, child: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            child: child.into_any_element(),
            radius,
            selected: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }
}

impl Clickable for Chip {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    fn cursor_style(self, _: CursorStyle) -> Self {
        self
    }
}

impl Toggleable for Chip {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover = cx.theme().colors().element_hover;
        div()
            .id(self.id)
            .flex_none()
            .rounded(px(self.radius))
            .cursor_pointer()
            .when(self.selected, |this| this.bg(hover))
            .hover(|style| style.bg(hover))
            .child(self.child)
            .when_some(self.tooltip, |this, text| this.tooltip(Tooltip::text(text)))
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
    }
}

const GSPIN_ROW_TINTS: [u32; 3] = [0xB6D3EF, 0xEDB185, 0xF888A0];
const GSPIN_DIM: f32 = 0.1;
const GRADIENT_SPIN_MS: u64 = 750;

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

fn gspin_opacity(t: f32, dim: f32) -> f32 {
    let t = t.rem_euclid(1.0);
    if t < 0.45 {
        lerp(1.0, dim, t / 0.45)
    } else if t < 0.92 {
        dim
    } else {
        lerp(dim, 1.0, (t - 0.92) / 0.08)
    }
}

fn gspin_cell_phase(row: usize, col: usize) -> f32 {
    let centre = 1.0;
    let max = 2.0 + centre;
    let d = 2.0 - row as f32 + (col as f32 - centre).abs();
    d / (max + 1.0)
}

pub(crate) fn gradient_spinner(id: SharedString, cell: f32) -> impl IntoElement {
    v_flex()
        .flex_none()
        .gap(px(cell / 2.0))
        .children((0..3).map(move |row| {
            let id = id.clone();
            h_flex()
                .gap(px(cell / 2.0))
                .children((0..3).map(move |col| {
                    let phase = gspin_cell_phase(row, col);
                    div()
                        .size(px(cell))
                        .rounded_full()
                        .bg(rgb(GSPIN_ROW_TINTS[row]))
                        .with_animation(
                            ElementId::Name(format!("{id}-{row}-{col}").into()),
                            Animation::new(Duration::from_millis(GRADIENT_SPIN_MS)).repeat(),
                            move |cell, delta| {
                                cell.opacity(gspin_opacity(delta - phase, GSPIN_DIM))
                            },
                        )
                }))
        }))
}

pub(crate) const FLAVOUR_WORDS: [&str; 21] = [
    "Reckoning",
    "Thinking",
    "Pondering",
    "Scheming",
    "Brewing",
    "Weaving",
    "Tinkering",
    "Musing",
    "Composing",
    "Sifting",
    "Untangling",
    "Distilling",
    "Sketching",
    "Plotting",
    "Riffing",
    "Combobulating",
    "Percolating",
    "Marinating",
    "Noodling",
    "Puzzling",
    "Conjuring",
];
const FLAVOUR_ROTATE_SECS: u64 = 7;

pub(crate) fn flavour_word(seed: u64, elapsed_secs: u64) -> &'static str {
    let step = elapsed_secs / FLAVOUR_ROTATE_SECS;
    FLAVOUR_WORDS[(seed.wrapping_add(step) % FLAVOUR_WORDS.len() as u64) as usize]
}

pub(crate) fn flavour_seed(chat_id: &str) -> u64 {
    chat_id.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    })
}

pub(crate) fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{}h {}m", secs / 3_600, (secs % 3_600) / 60)
    } else {
        format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3_600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_uses_two_units() {
        assert_eq!(format_elapsed(12), "12s");
        assert_eq!(format_elapsed(184), "3m 4s");
        assert_eq!(format_elapsed(3_720), "1h 2m");
        assert_eq!(format_elapsed(90_000), "1d 1h");
    }

    #[test]
    fn spinner_wave_rises_from_the_bottom_row() {
        assert!(gspin_cell_phase(2, 1) < gspin_cell_phase(0, 1));
        assert_eq!(gspin_opacity(0.0, GSPIN_DIM), 1.0);
        assert_eq!(gspin_opacity(0.6, GSPIN_DIM), GSPIN_DIM);
    }

    #[test]
    fn flavour_words_rotate_every_seven_seconds() {
        let seed = flavour_seed("chat-1");
        assert_eq!(flavour_word(seed, 0), flavour_word(seed, 6));
        assert_ne!(flavour_word(seed, 0), flavour_word(seed, 7));
    }
}
