use super::{ChatView, ui};
use gpui::{
    AnyElement, Context, Image, ImageSource, IntoElement, MouseButton, ObjectFit, ParentElement, ScrollWheelEvent,
    Styled, Window, deferred, div, hsla, img, px, relative,
};
use std::{path::PathBuf, sync::Arc};
use ui::{Icon, IconName, IconSize, prelude::*};

const MIN_ZOOM: f32 = 1.;
const MAX_ZOOM: f32 = 6.;

pub(super) struct Lightbox {
    pub source: ImageSource,
    pub zoom: f32,
}

impl ChatView {
    pub(super) fn open_image(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.lightbox = Some(Lightbox {
            source: path.into(),
            zoom: 1.,
        });
        window.focus(&self.lightbox_focus, cx);
        cx.notify();
    }

    pub(super) fn open_image_data(
        &mut self,
        image: Arc<Image>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.lightbox = Some(Lightbox {
            source: image.into(),
            zoom: 1.,
        });
        window.focus(&self.lightbox_focus, cx);
        cx.notify();
    }

    pub(super) fn close_image(&mut self, cx: &mut Context<Self>) {
        if self.lightbox.take().is_some() {
            cx.notify();
        }
    }

    fn zoom_image(&mut self, factor: f32, cx: &mut Context<Self>) {
        if let Some(lightbox) = &mut self.lightbox {
            lightbox.zoom = (lightbox.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
            cx.notify();
        }
    }

    pub(super) fn render_lightbox(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let lightbox = self.lightbox.as_ref()?;
        let zoom = lightbox.zoom;
        let button = |id: &'static str, icon: IconName| {
            div()
                .id(id)
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(hsla(0., 0., 0., 0.55))
                .cursor_pointer()
                .hover(|style| style.bg(hsla(0., 0., 0., 0.75)))
                .child(Icon::new(icon).size(IconSize::Small).color(Color::Custom(gpui::white())))
        };
        Some(
            deferred(
                div()
                    .id("agent-lightbox")
                    .track_focus(&self.lightbox_focus)
                    .key_context("AgentLightbox")
                    .on_action(cx.listener(|this, _: &menu::Cancel, _, cx| this.close_image(cx)))
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(hsla(0., 0., 0., 0.82))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.close_image(cx)),
                    )
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                        if !event.modifiers.platform && !event.modifiers.control {
                            return;
                        }
                        let delta = event.delta.pixel_delta(window.line_height()).y;
                        this.zoom_image(if f32::from(delta) > 0. { 1.1 } else { 1. / 1.1 }, cx);
                    }))
                    .child(
                        div()
                            .id("agent-lightbox-scroll")
                            .size_full()
                            .overflow_scroll()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .w(relative(0.9 * zoom))
                                    .h(relative(0.9 * zoom))
                                    .flex_none()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(
                                        img(lightbox.source.clone())
                                            .size_full()
                                            .object_fit(ObjectFit::Contain),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .absolute()
                            .top(px(14.))
                            .right(px(14.))
                            .gap(px(8.))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                div()
                                    .px(px(10.))
                                    .py(px(5.))
                                    .rounded_full()
                                    .bg(hsla(0., 0., 0., 0.55))
                                    .text_size(ui(12.))
                                    .text_color(gpui::white())
                                    .child(format!("{:.0}%", zoom * 100.)),
                            )
                            .child(
                                button("agent-lightbox-zoom-out", IconName::Dash).on_click(
                                    cx.listener(|this, _, _, cx| this.zoom_image(1. / 1.25, cx)),
                                ),
                            )
                            .child(
                                button("agent-lightbox-zoom-in", IconName::Plus).on_click(
                                    cx.listener(|this, _, _, cx| this.zoom_image(1.25, cx)),
                                ),
                            )
                            .child(
                                button("agent-lightbox-close", IconName::AgentClose)
                                    .on_click(cx.listener(|this, _, _, cx| this.close_image(cx))),
                            ),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}
