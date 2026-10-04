use gpui::{AnyElement, IntoElement, Stateful};
use smallvec::SmallVec;

use crate::prelude::*;

const TAB_HEIGHT: Pixels = px(24.);
const TAB_MIN_WIDTH: Pixels = px(112.);
const TAB_SLOT_SIZE: Pixels = px(18.);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum TabCloseSide {
    Start,
    End,
}

#[derive(IntoElement, RegisterComponent)]
pub struct Tab {
    div: Stateful<Div>,
    selected: bool,
    close_side: TabCloseSide,
    start_slot: Option<AnyElement>,
    end_slot: Option<AnyElement>,
    children: SmallVec<[AnyElement; 2]>,
}

impl Tab {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            div: div()
                .id(id.clone())
                .debug_selector(|| format!("TAB-{}", id)),
            selected: false,
            close_side: TabCloseSide::End,
            start_slot: None,
            end_slot: None,
            children: SmallVec::new(),
        }
    }

    pub fn close_side(mut self, close_side: TabCloseSide) -> Self {
        self.close_side = close_side;
        self
    }

    pub fn start_slot<E: IntoElement>(mut self, element: impl Into<Option<E>>) -> Self {
        self.start_slot = element.into().map(IntoElement::into_any_element);
        self
    }

    pub fn end_slot<E: IntoElement>(mut self, element: impl Into<Option<E>>) -> Self {
        self.end_slot = element.into().map(IntoElement::into_any_element);
        self
    }

    pub fn content_height(cx: &App) -> Pixels {
        DynamicSpacing::Base32.px(cx) - px(1.)
    }

    pub fn container_height(cx: &App) -> Pixels {
        DynamicSpacing::Base32.px(cx)
    }
}

impl InteractiveElement for Tab {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for Tab {}

impl Toggleable for Tab {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl ParentElement for Tab {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

impl RenderOnce for Tab {
    #[allow(refining_impl_trait)]
    fn render(self, _: &mut Window, cx: &mut App) -> Stateful<Div> {
        let colors = cx.theme().colors();
        let text_color = if self.selected {
            colors.text
        } else {
            colors.text_muted
        };
        let selected_background = colors.tab_active_background;
        let hover_background = selected_background.opacity(0.6);

        let has_end_slot = self.end_slot.is_some();
        let slot_layer = || {
            h_flex()
                .absolute()
                .inset_0()
                .justify_center()
        };
        let slot = h_flex()
            .relative()
            .flex_none()
            .size(TAB_SLOT_SIZE)
            .when_some(self.start_slot, |slot, indicator| {
                slot.child(
                    slot_layer()
                        .when(has_end_slot, |layer| {
                            layer.group_hover("", |style| style.invisible())
                        })
                        .child(indicator),
                )
            })
            .when_some(self.end_slot, |slot, end_slot| {
                slot.child(slot_layer().child(end_slot))
            });

        let content = h_flex()
            .flex_1()
            .min_w_0()
            .gap(px(3.))
            .children(self.children);

        self.div
            .group("")
            .flex()
            .flex_none()
            .items_center()
            .h(TAB_HEIGHT)
            .min_w(TAB_MIN_WIDTH)
            .px(px(4.))
            .gap(px(3.))
            .rounded(px(6.))
            .text_color(text_color)
            .cursor_pointer()
            .map(|this| {
                if self.selected {
                    this.bg(selected_background)
                } else {
                    this.hover(|style| style.bg(hover_background))
                }
            })
            .map(|this| match self.close_side {
                TabCloseSide::End => this.child(content).child(slot),
                TabCloseSide::Start => this.child(slot).child(content),
            })
    }
}

impl Component for Tab {
    fn scope() -> ComponentScope {
        ComponentScope::Navigation
    }

    fn description() -> &'static str {
        "A rounded tab chip for tabbed interfaces, with selected and unselected states."
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .children(vec![example_group_with_title(
                "Variations",
                vec![
                    single_example(
                        "Default",
                        Tab::new("default").child("Default Tab").into_any_element(),
                    ),
                    single_example(
                        "Selected",
                        Tab::new("selected")
                            .toggle_state(true)
                            .child("Selected Tab")
                            .into_any_element(),
                    ),
                ],
            )])
            .into_any_element()
    }
}
