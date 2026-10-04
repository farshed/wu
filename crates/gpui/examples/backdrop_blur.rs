use gpui::{
    App, Bounds, Context, Corners, IntoElement, Pixels, Render, Window, WindowBounds,
    WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use gpui_platform::application;

struct BackdropBlurExample {
    enabled: bool,
}

impl Render for BackdropBlurExample {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.enabled;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x182030))
            .text_color(rgb(0xffffff))
            .child(
                div()
                    .id("toggle-blur")
                    .p_4()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.enabled = !this.enabled;
                        cx.notify();
                    }))
                    .child(format!(
                        "Nested backdrop blur: {} (click to toggle)",
                        if enabled { "on" } else { "off" }
                    )),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        let rect = |x: f32, y: f32, w: f32, h: f32| Bounds {
                            origin: bounds.origin + point(px(x), px(y)),
                            size: size(px(w), px(h)),
                        };
                        for x in 0..80 {
                            let color = if x % 2 == 0 { 0x3498db } else { 0x172038 };
                            window
                                .paint_quad(fill(rect(x as f32 * 10., 0., 10., 420.), rgb(color)));
                        }
                        let outer = rect(50., 40., 600., 330.);
                        window.paint_layer(outer, |window| {
                            panel(window, outer, px(14.), enabled);
                            for y in 0..12 {
                                window.paint_quad(fill(
                                    rect(90., 75. + y as f32 * 22., 500., 3.),
                                    rgb(0xffb454),
                                ));
                            }
                            let inner = rect(250., 120., 300., 180.);
                            window.paint_layer(inner, |window| {
                                panel(window, inner, px(10.), enabled);
                                window.paint_quad(fill(rect(300., 205., 200., 4.), rgb(0xffffff)));
                            });
                        });
                    },
                )
                .size_full(),
            )
    }
}

fn panel(window: &mut Window, bounds: Bounds<Pixels>, radius: Pixels, enabled: bool) {
    if enabled {
        window.paint_backdrop_blur(bounds, Corners::all(px(20.)), radius);
    }
    window.paint_quad(gpui::quad(
        bounds,
        px(20.),
        rgb(0xffffff).alpha(0.12),
        px(1.),
        rgb(0xffffff).alpha(0.4),
        Default::default(),
    ));
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(720.), px(480.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| BackdropBlurExample { enabled: true }),
        )
        .unwrap();
        cx.activate(true);
    });
}
