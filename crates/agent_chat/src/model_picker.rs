use crate::{
    AgentChatSettings, AgentKind,
    agent_chat_settings::AgentChatModelPickerLayout,
    session::{AgentSession, AgentStore, ChatListPrefs, RunSettings},
};
use settings::Settings as _;
use agent_harness::{Model, ModelOption, ReasoningLevel, view};
use editor::Editor;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window, div, hsla, px,
    relative, rgb,
};
use theme::ActiveTheme as _;
use ui::{Icon, IconButton, IconName, IconSize, Label, LabelSize, Tooltip, prelude::*};

const CARD_WIDTH: f32 = 256.;
const FULL_CARD_WIDTH: f32 = 320.;
const CARD_RADIUS: f32 = 12.;
const CARD_INSET: f32 = 4.;
const ROW_RADIUS: f32 = CARD_RADIUS - 1. - CARD_INSET;
const ROW_HEIGHT: f32 = 32.;
const LIST_ROWS: f32 = 7.;
const SLIDER_HEIGHT: f32 = 36.;
const RAIL_HEIGHT: f32 = 28.;
const THUMB_WIDTH: f32 = 44.;
const THUMB_HEIGHT: f32 = 32.;
const CLAUDE_BRAND: u32 = 0xD97757;
const EFFORT_OPTION_IDS: [&str; 3] = ["effort", "reasoning", "reasoning_effort"];

pub(crate) fn agent_icon(kind: AgentKind) -> Icon {
    match kind {
        AgentKind::Claude => {
            Icon::new(IconName::AgentClaude).color(Color::Custom(rgb(CLAUDE_BRAND).into()))
        }
        AgentKind::Codex => Icon::new(IconName::AgentCodex),
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Settings,
    Models,
}

pub struct ModelPicker {
    session: Entity<AgentSession>,
    store: Entity<AgentStore>,
    page: Page,
    search: Entity<Editor>,
    focus_handle: FocusHandle,
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
            editor
        });
        let kind = session.read(cx).kind();
        store.update(cx, |store, cx| store.ensure_models(kind, cx));
        let subscriptions = vec![
            cx.observe(&session, |_, _, cx| cx.notify()),
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&search, |_, _, cx| cx.notify()),
        ];
        Self {
            session,
            store,
            page: Page::Settings,
            search,
            focus_handle: cx.focus_handle(),
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

    fn show_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Models;
        window.focus(&self.search.focus_handle(cx), cx);
        cx.notify();
    }

    fn render_settings(
        &self,
        kind: AgentKind,
        selection: &Selection,
        settings: &RunSettings,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let model_label = selection.model_label(settings, kind);
        let ladder = selection
            .model
            .as_ref()
            .map(|model| model.reasoning_levels.clone())
            .unwrap_or_default();
        let title: SharedString = selection
            .reasoning
            .map(|level| view::reasoning_label(level).into())
            .unwrap_or_else(|| model_label.clone());
        let fast = selection.fast();
        let fast_id = fast.as_ref().map(|(id, ..)| id.clone());
        let options: Vec<ModelOption> = selection
            .model
            .as_ref()
            .map(|model| {
                model
                    .options
                    .iter()
                    .filter(|option| {
                        Some(&option.id) != fast_id.as_ref()
                            && !EFFORT_OPTION_IDS.contains(&option.id.as_str())
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let header = h_flex()
            .h(px(if ladder.is_empty() { 36. } else { 48. }))
            .gap_1()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .justify_center()
                    .child(Label::new(title).weight(FontWeight::SEMIBOLD))
                    .when(!ladder.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .id("picker-model-name")
                                .gap_0p5()
                                .cursor_pointer()
                                .child(
                                    Label::new(model_label.clone())
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .truncate(),
                                )
                                .child(
                                    Icon::new(IconName::ChevronRight)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.show_models(window, cx)),
                                ),
                        )
                    }),
            )
            .when(ladder.is_empty(), |this| {
                this.child(
                    IconButton::new("picker-models", IconName::ChevronRight)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text("Models"))
                        .on_click(cx.listener(|this, _, window, cx| this.show_models(window, cx))),
                )
            })
            .when_some(fast, |this, (option_id, on, off, is_on)| {
                this.child(
                    div()
                        .id("picker-fast")
                        .w(px(32.))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(ROW_RADIUS))
                        .cursor_pointer()
                        .when(is_on, |this| this.bg(colors.element_selected))
                        .hover(|style| style.bg(colors.element_hover))
                        .tooltip(Tooltip::text(if is_on {
                            "Fast mode on"
                        } else {
                            "Fast mode off"
                        }))
                        .child(
                            Icon::new(if is_on {
                                IconName::AgentFastFilled
                            } else {
                                IconName::AgentFast
                            })
                            .size(IconSize::Small)
                            .color(if is_on {
                                Color::Accent
                            } else {
                                Color::Muted
                            }),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let choice = if is_on { off.clone() } else { on.clone() };
                            this.pick_option(option_id.clone(), choice, cx);
                        })),
                )
            });

        v_flex()
            .gap(px(2.))
            .child(header)
            .when(!ladder.is_empty(), |this| {
                this.child(self.render_slider(&ladder, selection.reasoning, cx))
            })
            .when(!options.is_empty(), |this| {
                this.child(
                    v_flex()
                        .pt_1()
                        .gap(px(2.))
                        .children(options.into_iter().map(|option| {
                            let current = selection.choice(&option).to_string();
                            let current_label = option
                                .choices
                                .iter()
                                .find(|choice| choice.id == current)
                                .map(|choice| choice.label.clone())
                                .unwrap_or_else(|| current.clone());
                            let next = option
                                .choices
                                .iter()
                                .position(|choice| choice.id == current)
                                .and_then(|index| {
                                    option.choices.get((index + 1) % option.choices.len())
                                })
                                .or_else(|| option.choices.first())
                                .map(|choice| choice.id.clone());
                            let option_id = option.id.clone();
                            h_flex()
                                .id(SharedString::from(format!("picker-option-{}", option.id)))
                                .h(px(26.))
                                .px_2()
                                .gap_1()
                                .rounded(px(ROW_RADIUS))
                                .cursor_pointer()
                                .hover(|style| style.bg(colors.element_hover))
                                .child(Label::new(option.label).size(LabelSize::Small))
                                .child(div().flex_1())
                                .child(
                                    Label::new(current_label)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(
                                    Icon::new(IconName::ChevronRight)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(next) = next.clone() {
                                        this.pick_option(option_id.clone(), next, cx);
                                    }
                                }))
                        })),
                )
            })
    }

    fn render_slider(
        &self,
        ladder: &[ReasoningLevel],
        selected: Option<ReasoningLevel>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let count = ladder.len();
        let selected_index = selected
            .and_then(|level| ladder.iter().position(|candidate| *candidate == level))
            .unwrap_or(0);
        let fill_fraction = (selected_index as f32 + 0.5) / count as f32;
        let rail_top = (SLIDER_HEIGHT - RAIL_HEIGHT) / 2.;
        div()
            .relative()
            .h(px(SLIDER_HEIGHT))
            .mx_2()
            .child(
                div()
                    .absolute()
                    .top(px(rail_top))
                    .left_0()
                    .right_0()
                    .h(px(RAIL_HEIGHT))
                    .rounded_full()
                    .bg(colors.element_background)
                    .border_1()
                    .border_color(colors.border_variant)
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(relative(fill_fraction))
                            .rounded_full()
                            .bg(colors.text_accent),
                    ),
            )
            .child(
                h_flex()
                    .absolute()
                    .inset_0()
                    .children(ladder.iter().enumerate().map(|(index, level)| {
                        let level = *level;
                        let is_selected = index == selected_index;
                        div()
                            .id(("effort-stop", index))
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .tooltip(Tooltip::text(view::reasoning_label(level)))
                            .map(|this| {
                                if is_selected {
                                    this.child(
                                        div()
                                            .flex_none()
                                            .w(px(THUMB_WIDTH))
                                            .h(px(THUMB_HEIGHT))
                                            .rounded_full()
                                            .bg(hsla(0., 0., 1., 1.))
                                            .border_1()
                                            .border_color(colors.border)
                                            .shadow_sm(),
                                    )
                                } else {
                                    this.child(div().size(px(4.)).rounded_full().bg(
                                        if index < selected_index {
                                            hsla(0., 0., 1., 0.6)
                                        } else {
                                            colors.text_muted.opacity(0.5)
                                        },
                                    ))
                                }
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.update_settings(cx, |settings| {
                                    settings.reasoning = Some(level);
                                });
                            }))
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

    fn render_full(
        &self,
        kind: AgentKind,
        models: &[Model],
        selection: &Selection,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let prefs = self.store.read(cx).list_prefs().clone();
        let favorites: Vec<Model> = models
            .iter()
            .filter(|model| prefs.is_favorite(kind, &model.id))
            .cloned()
            .collect();
        let selected_id = selection.model.as_ref().map(|model| model.id.clone());
        let ladder = selection
            .model
            .as_ref()
            .map(|model| model.reasoning_levels.clone())
            .unwrap_or_default();
        v_flex()
            .gap(px(4.))
            .child(
                h_flex()
                    .px_2()
                    .h(px(32.))
                    .gap_2()
                    .child(
                        Icon::new(IconName::MagnifyingGlass)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(div().flex_1().child(self.search.clone())),
            )
            .when(!favorites.is_empty(), |this| {
                this.child(
                    h_flex()
                        .px_1()
                        .gap_1()
                        .flex_wrap()
                        .children(favorites.into_iter().map(|model| {
                            let is_selected = selected_id.as_deref() == Some(model.id.as_str());
                            h_flex()
                                .id(SharedString::from(format!("picker-favorite-{}", model.id)))
                                .h(px(24.))
                                .px_2()
                                .gap_1()
                                .rounded(px(ROW_RADIUS))
                                .border_1()
                                .border_color(colors.border_variant)
                                .cursor_pointer()
                                .when(is_selected, |this| this.bg(colors.element_selected))
                                .hover(|style| style.bg(colors.element_hover))
                                .child(
                                    Icon::new(IconName::AgentStarFilled)
                                        .size(IconSize::XSmall)
                                        .color(Color::Warning),
                                )
                                .child(Label::new(model.label.clone()).size(LabelSize::XSmall))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.pick_model(model.clone(), window, cx)
                                }))
                        })),
                )
            })
            .child(self.render_model_rows(kind, models, selection, true, cx))
            .when(!ladder.is_empty(), |this| {
                this.child(
                    v_flex()
                        .pt_1()
                        .border_t_1()
                        .border_color(colors.border_variant)
                        .child(
                            div()
                                .px_2()
                                .pt_1()
                                .child(
                                    Label::new("Reasoning")
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                ),
                        )
                        .child(
                            h_flex()
                                .px_1()
                                .py_1()
                                .gap_1()
                                .flex_wrap()
                                .children(ladder.into_iter().map(|level| {
                                    let is_selected = selection.reasoning == Some(level);
                                    div()
                                        .id(SharedString::from(format!("picker-level-{level:?}")))
                                        .h(px(24.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded(px(ROW_RADIUS))
                                        .cursor_pointer()
                                        .when(is_selected, |this| this.bg(colors.element_selected))
                                        .hover(|style| style.bg(colors.element_hover))
                                        .child(
                                            Label::new(view::reasoning_label(level))
                                                .size(LabelSize::Small)
                                                .color(if is_selected {
                                                    Color::Default
                                                } else {
                                                    Color::Muted
                                                }),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.update_settings(cx, |settings| {
                                                settings.reasoning = Some(level)
                                            })
                                        }))
                                })),
                        ),
                )
            })
            .child(self.render_option_rows(selection, cx))
    }

    fn render_option_rows(&self, selection: &Selection, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let options: Vec<ModelOption> = selection
            .model
            .as_ref()
            .map(|model| {
                model
                    .options
                    .iter()
                    .filter(|option| !EFFORT_OPTION_IDS.contains(&option.id.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        v_flex().gap(px(2.)).children(options.into_iter().map(|option| {
            let current = selection.choice(&option).to_string();
            let current_label = option
                .choices
                .iter()
                .find(|choice| choice.id == current)
                .map(|choice| choice.label.clone())
                .unwrap_or_else(|| current.clone());
            let next = option
                .choices
                .iter()
                .position(|choice| choice.id == current)
                .and_then(|index| option.choices.get((index + 1) % option.choices.len()))
                .or_else(|| option.choices.first())
                .map(|choice| choice.id.clone());
            let option_id = option.id.clone();
            h_flex()
                .id(SharedString::from(format!("picker-full-option-{}", option.id)))
                .h(px(26.))
                .px_2()
                .gap_1()
                .rounded(px(ROW_RADIUS))
                .cursor_pointer()
                .hover(|style| style.bg(colors.element_hover))
                .child(Label::new(option.label).size(LabelSize::Small))
                .child(div().flex_1())
                .child(
                    Label::new(current_label)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(next) = next.clone() {
                        this.pick_option(option_id.clone(), next, cx);
                    }
                }))
        }))
    }

    fn render_model_rows(
        &self,
        kind: AgentKind,
        models: &[Model],
        selection: &Selection,
        stars: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let prefs = self.store.read(cx).list_prefs().clone();
        let query = self.search.read(cx).text(cx).trim().to_lowercase();
        let selected_id = selection.model.as_ref().map(|model| model.id.clone());
        let rows: Vec<Model> = models
            .iter()
            .filter(|model| {
                query.is_empty()
                    || model.label.to_lowercase().contains(&query)
                    || model.id.to_lowercase().contains(&query)
            })
            .cloned()
            .collect();
        let list_height = (rows.len().max(1) as f32).min(LIST_ROWS) * (ROW_HEIGHT + 2.);
        v_flex()
            .id("picker-model-rows")
            .pt_1()
            .gap(px(2.))
            .h(px(list_height))
            .overflow_y_scroll()
            .when(rows.is_empty(), |this| {
                this.child(
                    div().px_2().py_1().child(
                        Label::new("No matching models")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
            .children(rows.into_iter().map(|model| {
                let is_selected = selected_id.as_deref() == Some(model.id.as_str());
                let is_favorite = prefs.is_favorite(kind, &model.id);
                let tooltip = model.description.clone();
                let group: SharedString = format!("picker-row-{}", model.id).into();
                let star_id = model.id.clone();
                h_flex()
                    .id(group.clone())
                    .group(group.clone())
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .gap_2()
                    .rounded(px(ROW_RADIUS))
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(colors.element_selected))
                    .hover(|style| style.bg(colors.element_hover))
                    .when_some(tooltip, |this, tooltip| this.tooltip(Tooltip::text(tooltip)))
                    .child(agent_icon(kind).size(IconSize::Small))
                    .child(
                        div().flex_1().min_w_0().child(
                            Label::new(model.label.clone())
                                .size(LabelSize::Small)
                                .truncate(),
                        ),
                    )
                    .when(stars, |this| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("picker-star-{}", model.id)))
                                .size(px(20.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(5.))
                                .when(!is_favorite, |this| this.visible_on_hover(group.clone()))
                                .hover(|style| style.bg(colors.element_hover))
                                .tooltip(Tooltip::text(if is_favorite {
                                    "Remove from favorites"
                                } else {
                                    "Add to favorites"
                                }))
                                .child(
                                    Icon::new(if is_favorite {
                                        IconName::AgentStarFilled
                                    } else {
                                        IconName::AgentStar
                                    })
                                    .size(IconSize::XSmall)
                                    .color(if is_favorite {
                                        Color::Warning
                                    } else {
                                        Color::Muted
                                    }),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_favorite(kind, &star_id, cx)
                                })),
                        )
                    })
                    .when(is_selected, |this| {
                        this.child(
                            Icon::new(IconName::Check)
                                .size(IconSize::Small)
                                .color(Color::Accent),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_model(model.clone(), window, cx)
                    }))
            }))
    }

    fn render_models(
        &self,
        kind: AgentKind,
        models: &[Model],
        selection: &Selection,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        v_flex()
            .child(
                h_flex()
                    .h(px(36.))
                    .gap_1()
                    .child(
                        IconButton::new("picker-back", IconName::ChevronLeft)
                            .icon_size(IconSize::Small)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.page = Page::Settings;
                                window.focus(&this.focus_handle, cx);
                                cx.notify();
                            })),
                    )
                    .child(Label::new("Models").weight(FontWeight::MEDIUM)),
            )
            .child(
                h_flex()
                    .px_2()
                    .pb_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .child(
                        Icon::new(IconName::MagnifyingGlass)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(div().flex_1().child(self.search.clone())),
            )
            .child(self.render_model_rows(kind, models, selection, true, cx))
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
        let colors = cx.theme().colors();
        let full = AgentChatSettings::get_global(cx).model_picker == AgentChatModelPickerLayout::Full;
        v_flex()
            .key_context("menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .w(px(if full { FULL_CARD_WIDTH } else { CARD_WIDTH }))
            .p(px(CARD_INSET))
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(colors.border)
            .bg(colors.elevated_surface_background)
            .shadow_lg()
            .overflow_hidden()
            .map(|this| match (full, self.page) {
                (true, _) => this.child(self.render_full(kind, &models, &selection, cx)),
                (false, Page::Settings) => {
                    this.child(self.render_settings(kind, &selection, &settings, cx))
                }
                (false, Page::Models) => {
                    this.child(self.render_models(kind, &models, &selection, cx))
                }
            })
    }
}
