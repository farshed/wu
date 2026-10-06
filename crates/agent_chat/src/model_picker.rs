use crate::{
    AgentChatSettings, AgentKind,
    agent_chat_settings::AgentChatModelPickerLayout,
    chat_style::{accent, hairline, ink, mix, popover_card, selected_row, ui},
    session::{AgentSession, AgentStore, ChatListPrefs, RunSettings},
};
use agent_harness::{Model, ModelOption, PermissionMode, ReasoningLevel, view};
use editor::Editor;
use gpui::{
    AnyElement, App, Background, Bounds, BoxShadow, Context, Corners, DismissEvent, DispatchPhase,
    Div, ElementId, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render,
    SharedString, Stateful, Styled, Subscription, Svg, TextStyleRefinement,
    Transformation, Window, canvas, div, hsla, linear_color_stop, linear_gradient, point, px,
    radians, relative, rgb, size, svg,
};
use settings::Settings as _;
use std::{cell::Cell, rc::Rc};
use theme::ActiveTheme as _;
use ui::{Icon, IconName, Tooltip, prelude::*};

const COMPACT_WIDTH: f32 = 256.;
const FULL_WIDTH: f32 = 304.;
const CARD_RADIUS: f32 = 12.;
const CARD_INSET: f32 = 4.;
const MENU_GAP: f32 = 2.;
const MENU_ITEM_RADIUS: f32 = CARD_RADIUS - 1. - CARD_INSET;
const COMPACT_ROW_HEIGHT: f32 = 32.;
const COMPACT_LIST_ROWS: f32 = 7.;
const LIST_HEADER_HEIGHT: f32 = 40.;
const HEADER_HEIGHT: f32 = 48.;
const HEADER_HEIGHT_SINGLE: f32 = 36.;
const SIDE_BUTTON_WIDTH: f32 = 32.;
const SLIDER_HEIGHT: f32 = 36.;
const RAIL_HEIGHT: f32 = 28.;
const RAIL_TOP: f32 = (SLIDER_HEIGHT - RAIL_HEIGHT) / 2.;
const THUMB_WIDTH: f32 = 44.;
const THUMB_HEIGHT: f32 = 32.;
const THUMB_INSET: f32 = THUMB_WIDTH / 2.;
const FILL_PAST: f32 = RAIL_HEIGHT / 2.;
const SLIDER_TOP: f32 = 2.;
const SLIDER_BOTTOM: f32 = 4.;
const OPTIONS_GAP: f32 = 4.;
const OPTIONS_MAX_ROWS: usize = 6;
const MAX_SEGMENTED_CHOICES: usize = 3;
const SEGMENT_INSET: f32 = 2.;
const FULL_SLIDER_LABEL_HEIGHT: f32 = 24.;
const COMPACT_SETTING_HEIGHT: f32 = 26.;
const FULL_SETTING_HEIGHT: f32 = 30.;
const FULL_CHROME_HEIGHT: f32 = 82.;
const FULL_LIST_HEIGHT: f32 = 216.;
const FULL_TRAY_MAX_HEIGHT: f32 = 236.;
const SKELETON_WIDTHS: [f32; 4] = [0.42, 0.58, 0.48, 0.66];
const CLAUDE_BRAND: u32 = 0xD97757;
const EFFORT_OPTION_IDS: [&str; 3] = ["effort", "reasoning", "reasoning_effort"];

pub(crate) fn agent_icon(kind: AgentKind) -> Icon {
    match kind {
        AgentKind::Claude => {
            Icon::new(IconName::AgentClaude).color(Color::Custom(rgb(CLAUDE_BRAND).into()))
        }
        AgentKind::Codex => Icon::new(IconName::AgentCodex),
        AgentKind::Opencode => Icon::new(IconName::AgentOpencode),
    }
}

fn agent_icon_name(kind: AgentKind) -> IconName {
    match kind {
        AgentKind::Claude => IconName::AgentClaude,
        AgentKind::Codex => IconName::AgentCodex,
        AgentKind::Opencode => IconName::AgentOpencode,
    }
}

fn brand_tint(kind: AgentKind) -> Option<Hsla> {
    match kind {
        AgentKind::Claude => Some(rgb(CLAUDE_BRAND).into()),
        AgentKind::Codex | AgentKind::Opencode => None,
    }
}

fn glyph(name: IconName, size: f32, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
}

fn brand_glyph(kind: AgentKind, size: f32, untinted: Hsla) -> Svg {
    glyph(
        agent_icon_name(kind),
        size,
        brand_tint(kind).unwrap_or(untinted),
    )
}

fn back_glyph(size: f32, color: Hsla) -> Svg {
    glyph(IconName::AgentArrowRight, size, color)
        .with_transformation(Transformation::rotate(radians(std::f32::consts::PI)))
}

fn is_dark(cx: &App) -> bool {
    !cx.theme().appearance.is_light()
}

fn menu_row(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let text = cx.theme().colors().text;
    let hover = selected_row(cx);
    h_flex()
        .id(id)
        .gap(px(10.))
        .px(px(8.))
        .py(px(6.))
        .rounded(px(MENU_ITEM_RADIUS))
        .text_size(ui(13.))
        .cursor_pointer()
        .text_color(text.opacity(0.9))
        .hover(move |style| style.bg(hover).text_color(text))
}

fn permission_color(mode: PermissionMode, resting: Hsla, cx: &App) -> Hsla {
    if mode == PermissionMode::FullAccess {
        cx.theme().status().error
    } else {
        resting
    }
}

fn selected_ring(cx: &App) -> Vec<BoxShadow> {
    let color = if is_dark(cx) {
        hairline(0.09, cx)
    } else {
        hsla(0., 0., 0., 0.07)
    };
    vec![BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(1.),
        inset: true,
    }]
}

fn empty_list_note(copy: &'static str, cx: &App) -> AnyElement {
    div()
        .px(px(8.))
        .py(px(24.))
        .text_size(ui(12.))
        .text_color(cx.theme().colors().text_muted)
        .text_center()
        .child(copy)
        .into_any_element()
}

fn skeleton_rows(cx: &App) -> AnyElement {
    let wash = ink(0.05, cx);
    v_flex()
        .gap(px(8.))
        .py(px(6.))
        .px(px(4.))
        .children((0..5).map(move |index| {
            div()
                .h(px(14.))
                .w(relative(SKELETON_WIDTHS[index % SKELETON_WIDTHS.len()]))
                .rounded(px(7.))
                .bg(wash)
                .opacity(0.55)
        }))
        .into_any_element()
}

fn compact_list_height(rows: usize) -> f32 {
    let rows = if rows == 0 {
        4.
    } else {
        (rows as f32).min(COMPACT_LIST_ROWS)
    };
    rows * (COMPACT_ROW_HEIGHT + MENU_GAP) + 2. * CARD_INSET
}

fn full_tray_height(setting_count: usize, has_slider: bool) -> f32 {
    if setting_count == 0 && !has_slider {
        return 0.;
    }
    let slider = if has_slider {
        FULL_SLIDER_LABEL_HEIGHT + SLIDER_HEIGHT + MENU_GAP
    } else {
        0.
    };
    (setting_count as f32 * (FULL_SETTING_HEIGHT + MENU_GAP) + slider + 7.).min(FULL_TRAY_MAX_HEIGHT)
}

fn compact_options_height(count: usize) -> f32 {
    CARD_INSET - MENU_GAP + count.min(OPTIONS_MAX_ROWS) as f32 * (COMPACT_SETTING_HEIGHT + MENU_GAP)
}

fn shadow(color: Hsla, y: f32, blur: f32, spread: f32, inset: bool) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset,
    }
}

fn white(alpha: f32) -> Hsla {
    hsla(0., 0., 1., alpha)
}

fn black(alpha: f32) -> Hsla {
    hsla(0., 0., 0., alpha)
}

fn vertical(top: Hsla, bottom: Hsla) -> Background {
    linear_gradient(
        180.,
        linear_color_stop(top, 0.),
        linear_color_stop(bottom, 1.),
    )
}

struct Plate {
    background: Background,
    rim: Hsla,
    shadows: Vec<BoxShadow>,
}

impl Plate {
    fn light(dark: bool) -> Self {
        if dark {
            return Self {
                background: vertical(white(0.08), white(0.05)),
                rim: white(0.09),
                shadows: vec![
                    shadow(white(0.07), 1., 0., 0., true),
                    shadow(black(0.16), 1., 2., 0., false),
                ],
            };
        }
        Self {
            background: vertical(black(0.075), black(0.04)),
            rim: black(0.08),
            shadows: vec![
                shadow(black(0.08), 1., 2., 0., true),
                shadow(white(0.55), -1., 0., 0., true),
            ],
        }
    }

    fn accent(base: Hsla, dark: bool, glow: f32) -> Self {
        let top = mix(base, white(base.a), if dark { 0.06 } else { 0.22 });
        let rim = if dark {
            mix(base, white(base.a), 0.35).opacity(0.35)
        } else {
            mix(base, black(1.), 0.14)
        };
        let highlight = white(if dark { 0.12 } else { 0.32 });
        let ring = white(if dark { 0. } else { 0.22 });
        let halo = base.opacity(if dark { 0. } else { 0.14 } + 0.3 * glow);
        Self {
            background: vertical(top, base),
            rim,
            shadows: vec![
                shadow(highlight, 1., 0., 0., true),
                shadow(ring, 0., 0., 1., true),
                shadow(halo, 2. + 2. * glow, 6. + 14. * glow, 0., false),
            ],
        }
    }

    fn thumb(dark: bool) -> Self {
        let mut shadows = vec![
            shadow(white(0.8), 1., 0., 0., true),
            shadow(black(if dark { 0.22 } else { 0.14 }), 1., 2., 0., false),
        ];
        if !dark {
            shadows.push(shadow(black(0.06), 2., 6., 0., false));
        }
        Self {
            background: vertical(hsla(0., 0., 1., 1.), hsla(0., 0., 0.965, 1.)),
            rim: black(if dark { 0.12 } else { 0.11 }),
            shadows,
        }
    }

    fn apply<E: Styled>(self, element: E) -> E {
        element
            .bg(self.background)
            .border_1()
            .border_color(self.rim)
            .shadow(self.shadows)
    }

    fn paint(&self, window: &mut Window, bounds: Bounds<Pixels>, radius: f32) {
        let corners = Corners::all(px(radius));
        window.paint_drop_shadows(bounds, corners, &self.shadows);
        window.paint_quad(gpui::quad(
            bounds,
            corners,
            self.background,
            px(1.),
            self.rim,
            gpui::BorderStyle::default(),
        ));
        window.paint_inset_shadows(bounds, corners, &self.shadows);
    }
}

fn next_choice(option: &ModelOption, current: &str) -> Option<String> {
    option
        .choices
        .iter()
        .position(|choice| choice.id == current)
        .and_then(|index| option.choices.get((index + 1) % option.choices.len()))
        .or_else(|| option.choices.first())
        .map(|choice| choice.id.clone())
}

enum SettingAction {
    Option { id: String, choice: String },
}

struct SettingRow {
    id: SharedString,
    label: SharedString,
    value: SharedString,
    action: Option<SettingAction>,
    choices: Option<OptionChoices>,
}

struct OptionChoices {
    option_id: String,
    current: String,
    choices: Vec<(String, SharedString)>,
}

impl SettingRow {
    fn option(selection: &Selection, option: &ModelOption) -> Self {
        let current = selection.choice(option).to_string();
        let value = option
            .choices
            .iter()
            .find(|choice| choice.id == current)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| current.clone());
        Self {
            id: SharedString::from(format!("option-{}", option.id)),
            label: option.label.clone().into(),
            value: value.into(),
            action: next_choice(option, &current).map(|choice| SettingAction::Option {
                id: option.id.clone(),
                choice,
            }),
            choices: (option.choices.len() <= MAX_SEGMENTED_CHOICES).then(|| OptionChoices {
                option_id: option.id.clone(),
                current,
                choices: option
                    .choices
                    .iter()
                    .map(|choice| (choice.id.clone(), choice.label.clone().into()))
                    .collect(),
            }),
        }
    }
}

pub(crate) struct Selection {
    pub model: Option<Model>,
    pub reasoning: Option<ReasoningLevel>,
    pub options: serde_json::Map<String, serde_json::Value>,
}

impl Selection {
    pub fn resolve(models: &[Model], settings: &RunSettings) -> Self {
        let model = view::selected_catalog_model(models, settings.model.as_deref()).cloned();
        let reasoning = model
            .as_ref()
            .and_then(|model| view::clamp_reasoning(settings.reasoning, &model.reasoning_levels));
        let options = model
            .as_ref()
            .map(|model| view::offered_options(model, settings.options.clone()))
            .unwrap_or_default();
        Self {
            model,
            reasoning,
            options,
        }
    }

    pub fn model_label(&self, settings: &RunSettings, kind: AgentKind) -> SharedString {
        match (&self.model, &settings.model) {
            (Some(model), _) => model.label.clone().into(),
            (None, Some(id)) => id.clone().into(),
            (None, None) => kind.label().into(),
        }
    }

    pub fn choice<'a>(&'a self, option: &'a ModelOption) -> &'a str {
        self.options
            .get(&option.id)
            .and_then(|choice| choice.as_str())
            .unwrap_or(&option.default_choice)
    }

    pub fn fast_on(&self) -> bool {
        self.fast().is_some_and(|(_, _, _, is_on)| is_on)
    }

    fn fast(&self) -> Option<(String, String, String, bool)> {
        let model = self.model.as_ref()?;
        model.options.iter().find_map(|option| {
            let (on, off) = view::fast_mode_values(option)?;
            let is_on = self.choice(option) == on;
            Some((option.id.clone(), on.to_string(), off.to_string(), is_on))
        })
    }

    fn ladder(&self) -> Vec<ReasoningLevel> {
        self.model
            .as_ref()
            .map(|model| model.reasoning_levels.clone())
            .unwrap_or_default()
    }

    fn option_rows(&self, hidden: impl Fn(&ModelOption) -> bool) -> Vec<SettingRow> {
        self.model
            .iter()
            .flat_map(|model| model.options.iter())
            .filter(|option| !option.choices.is_empty() && !hidden(option))
            .map(|option| SettingRow::option(self, option))
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Settings,
    Models,
    Permissions,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModelTab {
    Favorites,
    Agent,
}

pub struct ModelPicker {
    session: Entity<AgentSession>,
    store: Entity<AgentStore>,
    page: Page,
    tab: ModelTab,
    search: Entity<Editor>,
    focus_handle: FocusHandle,
    slider_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    dragging_slider: bool,
    _subscriptions: Vec<Subscription>,
}

impl ModelPicker {
    pub fn new(
        session: Entity<AgentSession>,
        store: Entity<AgentStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search models…", window, cx);
            editor.set_text_style_refinement(TextStyleRefinement {
                font_size: Some(ui(13.).into()),
                ..Default::default()
            });
            editor
        });
        let kind = session.read(cx).kind();
        store.update(cx, |store, cx| store.ensure_models(kind, cx));
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.observe(&session, |_, _, cx| cx.notify()),
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.on_focus_out(&focus_handle, window, |_, _, _, cx| cx.emit(DismissEvent)),
        ];
        Self {
            session,
            store,
            page: Page::Settings,
            tab: ModelTab::Agent,
            search,
            focus_handle,
            slider_bounds: Rc::default(),
            dragging_slider: false,
            _subscriptions: subscriptions,
        }
    }

    fn update_settings(&self, cx: &mut Context<Self>, change: impl FnOnce(&mut RunSettings)) {
        self.session
            .update(cx, |session, cx| session.update_settings(change, cx));
    }

    fn pick_model(&mut self, model: Model, window: &mut Window, cx: &mut Context<Self>) {
        self.update_settings(cx, |settings| {
            settings.reasoning = view::clamp_reasoning(settings.reasoning, &model.reasoning_levels);
            settings.options = view::offered_options(&model, std::mem::take(&mut settings.options));
            settings.model = Some(model.id.clone());
        });
        self.search
            .update(cx, |editor, cx| editor.clear(window, cx));
        self.page = Page::Settings;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn pick_option(&self, option_id: String, choice: String, cx: &mut Context<Self>) {
        self.update_settings(cx, |settings| {
            settings
                .options
                .insert(option_id, serde_json::Value::String(choice));
        });
    }

    fn apply_setting(&self, action: &SettingAction, cx: &mut Context<Self>) {
        let SettingAction::Option { id, choice } = action;
        self.pick_option(id.clone(), choice.clone(), cx)
    }

    fn show_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Models;
        window.focus(&self.search.focus_handle(cx), cx);
        cx.notify();
    }

    fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Settings;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn show_permissions(&mut self, cx: &mut Context<Self>) {
        self.page = Page::Permissions;
        cx.notify();
    }

    fn pick_permission(
        &mut self,
        mode: PermissionMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_settings(cx, |settings| settings.permission = mode);
        self.show_settings(window, cx);
    }

    fn render_permission_row(
        &self,
        kind: AgentKind,
        current: PermissionMode,
        height: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().colors().text_muted;
        menu_row("picker-setting-permissions", cx)
            .h(px(height))
            .py(px(0.))
            .child(div().flex_1().min_w_0().truncate().child("Permissions"))
            .child(
                div()
                    .max_w(px(120.))
                    .truncate()
                    .text_color(permission_color(current, muted, cx))
                    .child(view::permission_label(current, kind.harness_id())),
            )
            .child(glyph(IconName::AgentArrowRight, 12., muted))
            .on_click(cx.listener(|this, _, _, cx| this.show_permissions(cx)))
            .into_any_element()
    }

    fn render_permissions(
        &self,
        kind: AgentKind,
        current: PermissionMode,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let harness = kind.harness_id();
        let wash = selected_row(cx);
        let ring = selected_ring(cx);
        let rows = PermissionMode::choices(harness).iter().map(|&mode| {
            let selected = mode == current;
            menu_row(
                SharedString::from(format!("picker-permission-{mode:?}")),
                cx,
            )
            .when(selected, |this| this.bg(wash).shadow(ring.clone()))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .child(
                        div()
                            .text_color(permission_color(mode, colors.text, cx))
                            .child(view::permission_label(mode, harness)),
                    )
                    .child(
                        div()
                            .text_size(ui(11.5))
                            .text_color(colors.text_muted)
                            .child(view::permission_description(mode, harness)),
                    ),
            )
            .when(selected, |this| {
                this.child(glyph(IconName::AgentCheck, 12., colors.text_muted))
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.pick_permission(mode, window, cx)),
            )
        });
        v_flex()
            .child(
                h_flex()
                    .h(px(LIST_HEADER_HEIGHT))
                    .flex_none()
                    .px(px(CARD_INSET))
                    .border_b_1()
                    .border_color(hairline(0.08, cx))
                    .gap(px(4.))
                    .child(
                        menu_row("picker-permissions-back", cx)
                            .flex_none()
                            .tooltip(Tooltip::text("Back"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.show_settings(window, cx)),
                            )
                            .child(back_glyph(14., colors.text_muted)),
                    )
                    .child(
                        div()
                            .text_size(ui(13.))
                            .text_color(colors.text.opacity(0.9))
                            .child("Permissions"),
                    ),
            )
            .child(v_flex().p(px(CARD_INSET)).gap(px(MENU_GAP)).children(rows))
    }

    fn render_setting_row(&self, row: SettingRow, height: f32, cx: &Context<Self>) -> AnyElement {
        if let Some(choices) = row.choices {
            return self.render_choice_row(&row.id, row.label, choices, height, cx);
        }
        let muted = cx.theme().colors().text_muted;
        let action = row.action;
        menu_row(SharedString::from(format!("picker-setting-{}", row.id)), cx)
            .h(px(height))
            .py(px(0.))
            .child(div().flex_1().min_w_0().truncate().child(row.label))
            .child(
                div()
                    .max_w(px(100.))
                    .truncate()
                    .text_color(muted)
                    .child(row.value),
            )
            .child(glyph(IconName::AgentArrowRight, 12., muted))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(action) = action.as_ref() {
                    this.apply_setting(action, cx);
                }
            }))
            .into_any_element()
    }

    fn render_choice_row(
        &self,
        id: &str,
        label: SharedString,
        choices: OptionChoices,
        height: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let text = colors.text;
        let muted = colors.text_muted;
        let wash = selected_row(cx);
        let ring = selected_ring(cx);
        let segment_height = height - 4. * SEGMENT_INSET;
        let OptionChoices {
            option_id,
            current,
            choices,
        } = choices;
        h_flex()
            .id(SharedString::from(format!("picker-setting-{id}")))
            .h(px(height))
            .gap(px(10.))
            .pl(px(8.))
            .pr(px(SEGMENT_INSET))
            .text_size(ui(13.))
            .text_color(text.opacity(0.9))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                h_flex()
                    .flex_none()
                    .p(px(SEGMENT_INSET))
                    .gap(px(SEGMENT_INSET))
                    .rounded(px(MENU_ITEM_RADIUS))
                    .bg(ink(0.05, cx))
                    .children(choices.into_iter().map(|(choice_id, choice_label)| {
                        let selected = choice_id == current;
                        let option_id = option_id.clone();
                        div()
                            .id(SharedString::from(format!("picker-choice-{option_id}-{choice_id}")))
                            .h(px(segment_height))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(MENU_ITEM_RADIUS - SEGMENT_INSET))
                            .text_size(ui(12.))
                            .cursor_pointer()
                            .map(|this| {
                                if selected {
                                    this.bg(wash).text_color(text).shadow(ring.clone())
                                } else {
                                    this.text_color(muted).hover(|style| style.text_color(text))
                                }
                            })
                            .child(choice_label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pick_option(option_id.clone(), choice_id.clone(), cx)
                            }))
                    })),
            )
            .into_any_element()
    }

    fn stop_dragging(&mut self, cx: &mut Context<Self>) {
        if self.dragging_slider {
            self.dragging_slider = false;
            cx.notify();
        }
    }

    fn set_effort_at(&mut self, x: Pixels, ladder: &[ReasoningLevel], cx: &mut Context<Self>) {
        let (Some(bounds), Some(last)) = (self.slider_bounds.get(), ladder.len().checked_sub(1))
        else {
            return;
        };
        let run = (f32::from(bounds.size.width) - 2. * THUMB_INSET).max(1.);
        let fraction = ((f32::from(x - bounds.origin.x) - THUMB_INSET) / run).clamp(0., 1.);
        let Some(level) = ladder.get((fraction * last as f32).round() as usize).copied() else {
            return;
        };
        if self.session.read(cx).settings().reasoning != Some(level) {
            self.update_settings(cx, |settings| settings.reasoning = Some(level));
        }
    }

    fn render_settings(
        &self,
        kind: AgentKind,
        selection: &Selection,
        settings: &RunSettings,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let ladder = selection.ladder();
        let selected_index = selection
            .reasoning
            .and_then(|level| ladder.iter().position(|candidate| *candidate == level))
            .unwrap_or(0);
        let effort: SharedString = ladder
            .get(selected_index)
            .map(|level| view::reasoning_label(*level).into())
            .unwrap_or_else(|| "Default".into());
        let model_label = selection.model_label(settings, kind);
        let fast = selection.fast();
        let fast_id = fast.as_ref().map(|(id, ..)| id.clone());
        let options = selection.option_rows(|option| {
            Some(&option.id) == fast_id.as_ref() || EFFORT_OPTION_IDS.contains(&option.id.as_str())
        });
        let has_ladder = !ladder.is_empty();

        let provider = div()
            .id("picker-provider")
            .w(px(SIDE_BUTTON_WIDTH))
            .h_full()
            .flex_none()
            .rounded(px(MENU_ITEM_RADIUS))
            .flex()
            .items_center()
            .justify_center()
            .tooltip(Tooltip::text(kind.label()))
            .child(brand_glyph(kind, 16., colors.text_muted));

        let title_group = "picker-title";
        let title = div()
            .id("picker-title")
            .group(title_group)
            .flex_1()
            .min_w_0()
            .h_full()
            .px(px(8.))
            .rounded(px(MENU_ITEM_RADIUS))
            .hover(|style| style.bg(ink(0.05, cx)))
            .flex()
            .flex_col()
            .items_start()
            .justify_center()
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| this.show_models(window, cx)))
            .when(has_ladder, |this| {
                this.child(
                    div()
                        .text_size(ui(14.))
                        .line_height(px(17.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.text)
                        .child(effort),
                )
            })
            .child(
                h_flex()
                    .max_w_full()
                    .min_w_0()
                    .gap(px(3.))
                    .map(|this| {
                        if has_ladder {
                            this.text_size(ui(12.))
                                .line_height(px(15.))
                                .text_color(colors.text_muted)
                                .group_hover(title_group, |style| style.text_color(colors.text))
                        } else {
                            this.text_size(ui(14.))
                                .line_height(px(17.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors.text)
                        }
                    })
                    .child(div().min_w_0().truncate().child(model_label))
                    .child(
                        div()
                            .flex_none()
                            .relative()
                            .opacity(0.55)
                            .group_hover(title_group, |style| style.left(px(3.)).opacity(1.))
                            .child(
                                glyph(IconName::AgentArrowRight, 10., colors.text_muted)
                                    .group_hover(title_group, |style| {
                                        style.text_color(colors.text)
                                    }),
                            ),
                    ),
            );

        let fast_button = fast.as_ref().map(|(option_id, on, off, is_on)| {
            let is_on = *is_on;
            let option_id = option_id.clone();
            let next = if is_on { off.clone() } else { on.clone() };
            let fast_group = "picker-fast";
            let resting = if is_on { accent(cx) } else { colors.text_muted };
            let hovered = if is_on { accent(cx) } else { colors.text };
            div()
                .id("picker-fast")
                .group(fast_group)
                .w(px(SIDE_BUTTON_WIDTH))
                .h_full()
                .flex_none()
                .rounded(px(MENU_ITEM_RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|style| style.bg(ink(0.05, cx)))
                .tooltip(Tooltip::text(if is_on {
                    "Fast mode on · Turn off"
                } else {
                    "Fast mode off · Turn on"
                }))
                .child(
                    glyph(
                        if is_on {
                            IconName::AgentFastFilled
                        } else {
                            IconName::AgentFast
                        },
                        15.,
                        resting,
                    )
                    .group_hover(fast_group, |style| style.text_color(hovered)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pick_option(option_id.clone(), next.clone(), cx);
                }))
        });

        let header = div()
            .h(px(if has_ladder {
                HEADER_HEIGHT
            } else {
                HEADER_HEIGHT_SINGLE
            }))
            .flex_none()
            .flex()
            .gap(px(CARD_INSET))
            .child(provider)
            .child(title)
            .children(fast_button);

        let option_count = options.len();
        let permission_row = self.render_permission_row(
            kind,
            settings.permission.for_harness(kind.harness_id()),
            COMPACT_SETTING_HEIGHT,
            cx,
        );
        v_flex()
            .p(px(CARD_INSET))
            .child(header)
            .when(has_ladder, |this| {
                this.child(
                    div()
                        .px(px(8.))
                        .pt(px(SLIDER_TOP))
                        .pb(px(SLIDER_BOTTOM))
                        .child(self.render_slider(
                            &ladder,
                            selected_index,
                            selection.fast_on(),
                            cx,
                        )),
                )
            })
            .when(option_count > 0, |this| {
                this.child(
                    v_flex()
                        .id("picker-options")
                        .mt(px(OPTIONS_GAP))
                        .pt(px(CARD_INSET))
                        .gap(px(MENU_GAP))
                        .max_h(px(compact_options_height(option_count)))
                        .overflow_y_scroll()
                        .children(options.into_iter().map(|row| {
                            self.render_setting_row(row, COMPACT_SETTING_HEIGHT, cx)
                        })),
                )
            })
            .child(
                div()
                    .mt(px(OPTIONS_GAP))
                    .pt(px(CARD_INSET))
                    .child(permission_row),
            )
    }

    fn render_slider(
        &self,
        ladder: &[ReasoningLevel],
        selected_index: usize,
        fast: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let dark = is_dark(cx);
        let count = ladder.len();
        let fraction = if count > 1 {
            selected_index as f32 / (count - 1) as f32
        } else {
            0.
        };
        let intensity = if fast { 1. } else { 0. };
        let breath = 0.5;
        let base = accent(cx);
        let fill = Plate::accent(base, dark, intensity * (0.2 + 0.25 * breath));
        let filled_stop = white(0.4);
        let open_stop = cx.theme().colors().text_muted.opacity(0.28);
        let halo = base.opacity(0.35 * intensity * (0.55 + 0.45 * breath));
        let place = |element: Div| {
            element
                .absolute()
                .left(relative(fraction))
                .ml(px(-THUMB_WIDTH / 2.))
                .w(px(THUMB_WIDTH))
                .h(px(THUMB_HEIGHT))
                .rounded_full()
        };
        let span = (count.max(2) - 1) as f32;
        let bounds_store = self.slider_bounds.clone();
        let dragging = self.dragging_slider;
        let picker = cx.entity().downgrade();
        let drag_ladder = ladder.to_vec();
        let press_ladder = ladder.to_vec();
        div()
            .relative()
            .h(px(SLIDER_HEIGHT))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.dragging_slider = true;
                    this.set_effort_at(event.position.x, &press_ladder, cx);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.stop_dragging(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.stop_dragging(cx)),
            )
            .child(Plate::light(dark).apply(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(RAIL_TOP))
                    .h(px(RAIL_HEIGHT))
                    .rounded_full(),
            ))
            .child(
                canvas(
                    move |bounds, _, _| bounds_store.set(Some(bounds)),
                    move |bounds, _, window, _| {
                        // Drags continue past the slider's edges, so follow the pointer window-wide.
                        if dragging {
                            let moving = picker.clone();
                            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                                if phase != DispatchPhase::Bubble {
                                    return;
                                }
                                moving
                                    .update(cx, |this, cx| {
                                        if event.pressed_button == Some(MouseButton::Left) {
                                            this.set_effort_at(event.position.x, &drag_ladder, cx);
                                        } else {
                                            this.stop_dragging(cx);
                                        }
                                    })
                                    .ok();
                            });
                        }
                        let width = f32::from(bounds.size.width);
                        let run = (width - 2. * THUMB_INSET).max(0.);
                        let center = THUMB_INSET + fraction * run;
                        let rect = |x: f32, y: f32, w: f32, h: f32| {
                            Bounds::new(bounds.origin + point(px(x), px(y)), size(px(w), px(h)))
                        };
                        fill.paint(
                            window,
                            rect(0., RAIL_TOP, (center + FILL_PAST).min(width), RAIL_HEIGHT),
                            RAIL_HEIGHT / 2.,
                        );
                        for index in 0..count {
                            let stop = if count > 1 {
                                index as f32 / (count - 1) as f32
                            } else {
                                0.
                            };
                            let x = THUMB_INSET + stop * run;
                            window.paint_quad(gpui::quad(
                                rect(x - 2., SLIDER_HEIGHT / 2. - 2., 4., 4.),
                                px(2.),
                                if stop <= fraction {
                                    filled_stop
                                } else {
                                    open_stop
                                },
                                px(0.),
                                gpui::transparent_black(),
                                gpui::BorderStyle::default(),
                            ));
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .absolute()
                    .left(px(THUMB_INSET))
                    .right(px(THUMB_INSET))
                    .top(px((SLIDER_HEIGHT - THUMB_HEIGHT) / 2.))
                    .h(px(THUMB_HEIGHT))
                    .when(fast, |this| {
                        this.child(place(div()).shadow(vec![shadow(halo, 1., 10., 0., false)]))
                    })
                    .child(Plate::thumb(dark).apply(place(div()))),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(THUMB_INSET))
                    .right(px(THUMB_INSET))
                    .children(ladder.iter().enumerate().map(|(index, level)| {
                        let level = *level;
                        div()
                            .id(("effort-stop", index))
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .map(|this| {
                                if count > 1 {
                                    this.left(relative((index as f32 - 0.5) / span))
                                        .w(relative(1. / span))
                                } else {
                                    this.left_0().right_0()
                                }
                            })
                            .tooltip(Tooltip::text(view::reasoning_label(level)))
                    })),
            )
    }

    fn toggle_favorite(&self, kind: AgentKind, model_id: &str, cx: &mut Context<Self>) {
        let key = ChatListPrefs::favorite_key(kind, model_id);
        self.store.update(cx, |store, cx| {
            store.update_list_prefs(
                |prefs| {
                    if let Some(position) = prefs.favorite_models.iter().position(|id| *id == key) {
                        prefs.favorite_models.remove(position);
                    } else {
                        prefs.favorite_models.push(key);
                    }
                },
                cx,
            )
        });
    }

    fn visible_models(
        &self,
        kind: AgentKind,
        models: &[Model],
        favorites_only: bool,
        cx: &Context<Self>,
    ) -> Vec<Model> {
        let prefs = self.store.read(cx).list_prefs().clone();
        let query = self.search.read(cx).text(cx).trim().to_lowercase();
        models
            .iter()
            .filter(|model| !favorites_only || prefs.is_favorite(kind, &model.id))
            .filter(|model| {
                query.is_empty()
                    || model.label.to_lowercase().contains(&query)
                    || model.id.to_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }

    fn empty_rows(&self, models: &[Model], favorites_only: bool, cx: &Context<Self>) -> AnyElement {
        let searching = !self.search.read(cx).text(cx).trim().is_empty();
        if searching {
            empty_list_note("No models found", cx)
        } else if favorites_only {
            empty_list_note("No starred models yet – hit a row's star", cx)
        } else if models.is_empty() {
            skeleton_rows(cx)
        } else {
            empty_list_note("No models found", cx)
        }
    }

    fn render_star(
        &self,
        kind: AgentKind,
        model_id: &str,
        is_favorite: bool,
        side: f32,
        radius: f32,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let star_id = model_id.to_string();
        div()
            .id(SharedString::from(format!("picker-star-{model_id}")))
            .flex_none()
            .size(px(side))
            .rounded(px(radius))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(ink(0.08, cx)))
            .tooltip(Tooltip::text(if is_favorite {
                "Remove from favorites"
            } else {
                "Add to favorites"
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_favorite(kind, &star_id, cx)
            }))
    }

    fn render_compact_row(
        &self,
        kind: AgentKind,
        model: Model,
        is_selected: bool,
        is_favorite: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let group: SharedString = format!("picker-row-{}", model.id).into();
        let star_color = if is_favorite {
            accent(cx)
        } else {
            colors.text_muted
        };
        let star = self
            .render_star(kind, &model.id, is_favorite, 24., 6., cx)
            .when(!is_favorite, |this| this.visible_on_hover(group.clone()))
            .child(glyph(
                if is_favorite {
                    IconName::AgentStarFilled
                } else {
                    IconName::AgentStar
                },
                13.,
                star_color,
            ));
        let tooltip = model.description.clone();
        let row = h_flex()
            .id(group.clone())
            .group(group)
            .h(px(COMPACT_ROW_HEIGHT))
            .pl(px(8.))
            .pr(px(4.))
            .rounded(px(MENU_ITEM_RADIUS))
            .gap(px(8.))
            .cursor_pointer()
            .text_color(colors.text)
            .border_1()
            .border_color(gpui::transparent_black())
            .map(|this| {
                if is_selected {
                    Plate::light(is_dark(cx)).apply(this)
                } else {
                    this.hover(|style| style.bg(ink(0.05, cx)))
                }
            })
            .when_some(tooltip, |this, tooltip| this.tooltip(Tooltip::text(tooltip)))
            .child(brand_glyph(kind, 14., colors.text_muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(model.label.clone()),
            )
            .child(star)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.pick_model(model.clone(), window, cx)
            }));
        div().pb(px(MENU_GAP)).child(row).into_any_element()
    }

    fn render_full_row(
        &self,
        kind: AgentKind,
        model: Model,
        is_selected: bool,
        is_favorite: bool,
        two_line: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let warning = cx.theme().status().warning;
        let star = self
            .render_star(
                kind,
                &model.id,
                is_favorite,
                22.,
                MENU_ITEM_RADIUS,
                cx,
            )
            .child(glyph(
                if is_favorite {
                    IconName::AgentStarFilled
                } else {
                    IconName::AgentStar
                },
                13.,
                if is_favorite {
                    warning
                } else {
                    colors.text_muted
                },
            ));
        let label = div()
            .truncate()
            .text_size(ui(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(colors.text)
            .child(model.label.clone());
        let body = if two_line {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(label.w_full())
                .child(
                    h_flex()
                        .gap(px(6.))
                        .child(brand_glyph(kind, 11., colors.text_muted))
                        .child(
                            div()
                                .flex_none()
                                .text_size(ui(11.))
                                .text_color(colors.text_muted)
                                .child(kind.label()),
                        ),
                )
        } else {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(6.))
                .child(label.flex_none().max_w_full())
        };
        let tooltip = model.description.clone();
        let row = h_flex()
            .id(SharedString::from(format!("picker-row-{}", model.id)))
            .px(px(8.))
            .py(px(if two_line { 6. } else { 5. }))
            .rounded(px(MENU_ITEM_RADIUS))
            .gap(px(10.))
            .cursor_pointer()
            .map(|this| {
                if is_selected {
                    this.bg(selected_row(cx)).shadow(selected_ring(cx))
                } else {
                    this.hover(|style| style.bg(ink(0.05, cx)))
                }
            })
            .when_some(tooltip, |this, tooltip| this.tooltip(Tooltip::text(tooltip)))
            .child(body)
            .child(star)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.pick_model(model.clone(), window, cx)
            }));
        div().pb(px(MENU_GAP)).child(row).into_any_element()
    }

    fn render_list_host(
        &self,
        id: &'static str,
        height: f32,
        rows: Vec<AnyElement>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .relative()
            .flex_none()
            .h(px(height))
            .py(px(CARD_INSET))
            .bg(ink(0.02, cx))
            .child(
                v_flex()
                    .id(id)
                    .size_full()
                    .px(px(CARD_INSET))
                    .overflow_y_scroll()
                    .children(rows),
            )
    }

    fn render_models(
        &self,
        kind: AgentKind,
        models: &[Model],
        selection: &Selection,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().colors().text_muted;
        let prefs = self.store.read(cx).list_prefs().clone();
        let selected_id = selection.model.as_ref().map(|model| model.id.clone());
        let visible = self.visible_models(kind, models, false, cx);
        let list_height = compact_list_height(visible.len());
        let rows: Vec<AnyElement> = if visible.is_empty() {
            vec![self.empty_rows(models, false, cx)]
        } else {
            visible
                .into_iter()
                .map(|model| {
                    let is_selected = selected_id.as_deref() == Some(model.id.as_str());
                    let is_favorite = prefs.is_favorite(kind, &model.id);
                    self.render_compact_row(kind, model, is_selected, is_favorite, cx)
                })
                .collect()
        };
        v_flex()
            .child(
                h_flex()
                    .h(px(LIST_HEADER_HEIGHT))
                    .flex_none()
                    .px(px(CARD_INSET))
                    .border_b_1()
                    .border_color(hairline(0.08, cx))
                    .gap(px(4.))
                    .child(
                        menu_row("picker-back", cx)
                            .flex_none()
                            .tooltip(Tooltip::text("Back"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.show_settings(window, cx)),
                            )
                            .child(back_glyph(14., muted)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(ui(13.))
                            .child(self.search.clone()),
                    ),
            )
            .child(self.render_list_host("picker-model-rows", list_height, rows, cx))
    }

    fn render_tab(
        &self,
        id: &'static str,
        tab: ModelTab,
        icon: Svg,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let is_viewed = self.tab == tab;
        div()
            .id(id)
            .relative()
            .size(px(32.))
            .rounded(px(MENU_ITEM_RADIUS))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(!is_viewed, |this| this.hover(|style| style.bg(ink(0.06, cx))))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tab = tab;
                cx.notify();
            }))
            .child(icon)
            .when(is_viewed, |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom(px(-4.))
                        .left(px(6.))
                        .right(px(6.))
                        .h(px(2.))
                        .rounded(px(1.))
                        .bg(accent(cx)),
                )
            })
    }

    fn render_full(
        &self,
        kind: AgentKind,
        models: &[Model],
        selection: &Selection,
        settings_rows: Vec<SettingRow>,
        permission: PermissionMode,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let prefs = self.store.read(cx).list_prefs().clone();
        let selected_id = selection.model.as_ref().map(|model| model.id.clone());
        let favorites_only = self.tab == ModelTab::Favorites;
        let viewed_color = |viewed: bool| if viewed { colors.text } else { colors.text_muted };
        let tabs = h_flex()
            .flex_none()
            .h(px(LIST_HEADER_HEIGHT))
            .px(px(CARD_INSET))
            .border_b_1()
            .border_color(hairline(0.08, cx))
            .gap(px(2.))
            .child(self.render_tab(
                "picker-tab-favorites",
                ModelTab::Favorites,
                glyph(IconName::AgentStarFilled, 15., viewed_color(favorites_only)),
                cx,
            ))
            .child(self.render_tab(
                "picker-tab-agent",
                ModelTab::Agent,
                brand_glyph(kind, 16., viewed_color(!favorites_only)),
                cx,
            ));
        let search_row = h_flex()
            .flex_none()
            .h(px(LIST_HEADER_HEIGHT))
            .px(px(10.))
            .border_b_1()
            .border_color(hairline(0.08, cx))
            .gap(px(8.))
            .child(glyph(IconName::AgentMagnifer, 14., colors.text_muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui(13.))
                    .child(self.search.clone()),
            );
        let visible = self.visible_models(kind, models, favorites_only, cx);
        let rows: Vec<AnyElement> = if visible.is_empty() {
            vec![self.empty_rows(models, favorites_only, cx)]
        } else {
            visible
                .into_iter()
                .map(|model| {
                    let is_selected = selected_id.as_deref() == Some(model.id.as_str());
                    let is_favorite = prefs.is_favorite(kind, &model.id);
                    self.render_full_row(kind, model, is_selected, is_favorite, favorites_only, cx)
                })
                .collect()
        };
        let ladder = selection.ladder();
        let selected_index = selection
            .reasoning
            .and_then(|level| ladder.iter().position(|candidate| *candidate == level))
            .unwrap_or(0);
        let slider = (!ladder.is_empty()).then(|| {
            let effort: SharedString = ladder
                .get(selected_index)
                .map(|level| view::reasoning_label(*level).into())
                .unwrap_or_default();
            v_flex()
                .px(px(8.))
                .child(
                    h_flex()
                        .h(px(FULL_SLIDER_LABEL_HEIGHT))
                        .text_size(ui(13.))
                        .text_color(colors.text.opacity(0.9))
                        .child(div().flex_1().child("Reasoning"))
                        .child(div().text_color(colors.text_muted).child(effort)),
                )
                .child(self.render_slider(&ladder, selected_index, selection.fast_on(), cx))
        });
        let tray_height = full_tray_height(settings_rows.len() + 1, slider.is_some());
        let permission_row = self.render_permission_row(kind, permission, FULL_SETTING_HEIGHT, cx);
        v_flex()
            .child(tabs)
            .child(search_row)
            .child(self.render_list_host("picker-model-rows", FULL_LIST_HEIGHT, rows, cx))
            .when(tray_height > 0., |this| {
                this.child(
                    v_flex()
                        .id("picker-traits-tray")
                        .flex_none()
                        .border_t_1()
                        .border_color(hairline(0.08, cx))
                        .h(px(tray_height))
                        .overflow_y_scroll()
                        .px(px(CARD_INSET))
                        .child(
                            v_flex()
                                .gap(px(MENU_GAP))
                                .py(px(CARD_INSET))
                                .children(slider)
                                .children(settings_rows.into_iter().map(|row| {
                                    self.render_setting_row(row, FULL_SETTING_HEIGHT, cx)
                                }))
                                .child(permission_row),
                        ),
                )
            })
    }
}

impl EventEmitter<DismissEvent> for ModelPicker {}

impl Focusable for ModelPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ModelPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let kind = session.kind();
        let settings = session.settings().clone();
        let models = self.store.read(cx).models(kind).to_vec();
        let selection = Selection::resolve(&models, &settings);
        let full = AgentChatSettings::get_global(cx).model_picker == AgentChatModelPickerLayout::Full;
        let card = popover_card(cx)
            .key_context("menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .p(px(0.));
        let permission = settings.permission.for_harness(kind.harness_id());
        if self.page == Page::Permissions {
            return card
                .w(px(if full { FULL_WIDTH } else { COMPACT_WIDTH }))
                .child(self.render_permissions(kind, permission, cx))
                .into_any_element();
        }
        if full {
            let settings_rows: Vec<SettingRow> = selection
                .option_rows(|option| EFFORT_OPTION_IDS.contains(&option.id.as_str()));
            let has_slider = !selection.ladder().is_empty();
            let height = FULL_CHROME_HEIGHT
                + FULL_LIST_HEIGHT
                + full_tray_height(settings_rows.len() + 1, has_slider);
            return card
                .w(px(FULL_WIDTH))
                .h(px(height))
                .child(self.render_full(kind, &models, &selection, settings_rows, permission, cx))
                .into_any_element();
        }
        card.w(px(COMPACT_WIDTH))
            .map(|this| match self.page {
                Page::Settings => {
                    this.child(self.render_settings(kind, &selection, &settings, cx))
                }
                Page::Models | Page::Permissions => {
                    this.child(self.render_models(kind, &models, &selection, cx))
                }
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn slider_position_picks_the_nearest_effort(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = crate::session::tests::new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (picker, cx) = cx.add_window_view(|window, cx| {
            ModelPicker::new(session.clone(), store.clone(), window, cx)
        });
        let ladder = [
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::Max,
        ];
        let reasoning_at = |x: f32, cx: &mut gpui::VisualTestContext| {
            picker.update(cx, |picker, cx| {
                picker.slider_bounds.set(Some(Bounds::new(
                    point(px(100.), px(0.)),
                    size(px(2. * THUMB_INSET + 300.), px(SLIDER_HEIGHT)),
                )));
                picker.set_effort_at(px(x), &ladder, cx);
            });
            session.read_with(cx, |session, _| session.settings().reasoning)
        };
        assert_eq!(reasoning_at(0., cx), Some(ReasoningLevel::Low));
        assert_eq!(reasoning_at(100. + THUMB_INSET + 95., cx), Some(ReasoningLevel::Medium));
        assert_eq!(reasoning_at(100. + THUMB_INSET + 210., cx), Some(ReasoningLevel::High));
        assert_eq!(reasoning_at(2000., cx), Some(ReasoningLevel::Max));
    }

    #[gpui::test]
    async fn a_quick_click_on_the_slider_does_not_leave_it_dragging(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = crate::session::tests::new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (picker, cx) = cx.add_window_view(|window, cx| {
            ModelPicker::new(session.clone(), store.clone(), window, cx)
        });
        crate::session::tests::run_until(cx, |cx| {
            picker.read_with(cx, |picker, _| picker.slider_bounds.get().is_some())
        });
        let bounds = picker
            .read_with(cx, |picker, _| picker.slider_bounds.get())
            .expect("slider was drawn");
        let position = bounds.center();
        cx.simulate_mouse_down(position, MouseButton::Left, gpui::Modifiers::none());
        assert!(picker.read_with(cx, |picker, _| picker.dragging_slider));
        cx.simulate_mouse_up(position, MouseButton::Left, gpui::Modifiers::none());
        assert!(!picker.read_with(cx, |picker, _| picker.dragging_slider));
    }

    #[gpui::test]
    async fn picking_a_permission_mode_saves_it_and_returns(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            workspace::AppState::test(cx);
            editor::init(cx);
        });
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = crate::session::tests::new_store(directory.path(), cx);
        let session = store.update(cx, |store, cx| {
            store.create_session(AgentKind::Claude, directory.path().to_path_buf(), cx)
        });
        let (picker, cx) = cx.add_window_view(|window, cx| {
            ModelPicker::new(session.clone(), store.clone(), window, cx)
        });
        picker.update(cx, |picker, cx| picker.show_permissions(cx));
        cx.run_until_parked();
        picker.update_in(cx, |picker, window, cx| {
            picker.pick_permission(PermissionMode::FullAccess, window, cx)
        });
        assert_eq!(
            session.read_with(cx, |session, _| session.settings().permission),
            PermissionMode::FullAccess
        );
        assert!(picker.read_with(cx, |picker, _| picker.page == Page::Settings));
    }

    #[test]
    fn compact_list_fits_rows_up_to_seven() {
        assert_eq!(compact_list_height(0), 4. * 34. + 8.);
        assert_eq!(compact_list_height(3), 3. * 34. + 8.);
        assert_eq!(compact_list_height(20), 7. * 34. + 8.);
    }

    #[test]
    fn full_tray_grows_with_settings_then_caps() {
        assert_eq!(full_tray_height(0, false), 0.);
        assert_eq!(full_tray_height(2, false), 71.);
        assert_eq!(full_tray_height(1, true), 32. + 24. + 36. + 2. + 7.);
        assert_eq!(full_tray_height(12, true), FULL_TRAY_MAX_HEIGHT);
    }
}
