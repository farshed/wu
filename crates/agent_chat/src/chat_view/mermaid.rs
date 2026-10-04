use super::ChatView;
use crate::chat_style::{accent, ink, page, text_faint};
use gpui::{App, Context, Hsla, Image, ImageFormat, Rgba};
use mermaid_render::{DiagramTheme, RenderError};
use std::sync::Arc;
use theme::ActiveTheme as _;
use util::ResultExt as _;

const MAX_DIAGRAMS: usize = 64;

#[derive(Clone)]
pub(super) enum Diagram {
    Pending,
    Ready {
        image: Arc<Image>,
        width: f32,
        height: f32,
    },
    Failed(String),
}

fn rgba(color: Hsla) -> mermaid_render::Rgba {
    let Rgba { r, g, b, a } = color.to_rgb();
    mermaid_render::Rgba { r, g, b, a }
}

pub(super) fn diagram_theme(cx: &App) -> DiagramTheme {
    let colors = cx.theme().colors();
    let is_light = cx.theme().appearance.is_light();
    let background = page(cx);
    let canvas = background.blend(ink(0.035, cx));
    let node = if is_light {
        background
    } else {
        canvas.blend(ink(0.06, cx))
    };
    let border_strong = ink(if is_light { 0.17 } else { 0.14 }, cx);
    DiagramTheme {
        font_family: "Geist".into(),
        background: rgba(canvas),
        text: rgba(colors.text),
        line: rgba(text_faint(cx)),
        node_fill: rgba(node),
        node_border: rgba(border_strong),
        accent: rgba(accent(cx)),
    }
}

pub(super) fn mermaid_sources(markdown: &str) -> Vec<String> {
    let mut sources = Vec::new();
    let mut lines = markdown.lines();
    while let Some(line) = lines.next() {
        let fence = line.trim_start();
        let Some(info) = fence.strip_prefix("```") else {
            continue;
        };
        if !info.trim().eq_ignore_ascii_case("mermaid") {
            continue;
        }
        let mut source = String::new();
        let mut closed = false;
        for body in lines.by_ref() {
            if body.trim_start().starts_with("```") {
                closed = true;
                break;
            }
            source.push_str(body);
            source.push('\n');
        }
        if closed {
            sources.push(source);
        }
    }
    sources
}

impl ChatView {
    pub(super) fn request_visible_diagrams(&mut self, cx: &mut Context<Self>) {
        let sources: Vec<String> = self
            .entries(cx)
            .iter()
            .filter_map(|entry| match entry {
                crate::session::Entry::Assistant { markdown, .. } => {
                    let source = markdown.read(cx).source();
                    source.contains("```mermaid").then(|| mermaid_sources(source))
                }
                _ => None,
            })
            .flatten()
            .collect();
        for source in sources {
            self.request_diagram(source, cx);
        }
    }

    pub(super) fn diagram(&self, source: &str, cx: &App) -> Option<Diagram> {
        let dark = !cx.theme().appearance.is_light();
        self.diagrams
            .borrow()
            .get(&(source.trim_end().to_string(), dark))
            .cloned()
    }

    pub(super) fn request_diagram(&mut self, source: String, cx: &mut Context<Self>) {
        let dark = !cx.theme().appearance.is_light();
        let key = (source.trim_end().to_string(), dark);
        if self.diagrams.borrow().contains_key(&key) {
            return;
        }
        if self.diagrams.borrow().len() >= MAX_DIAGRAMS {
            self.diagrams.borrow_mut().clear();
        }
        self.diagrams.borrow_mut().insert(key.clone(), Diagram::Pending);
        let theme = diagram_theme(cx);
        let key_for_task = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { mermaid_render::render(&source, &theme) })
                .await;
            let diagram = match result {
                Ok(rendered) => Diagram::Ready {
                    image: Arc::new(Image::from_bytes(ImageFormat::Svg, rendered.svg.to_vec())),
                    width: rendered.width,
                    height: rendered.height,
                },
                Err(RenderError::TooComplex) => {
                    Diagram::Failed("This diagram is too large to draw.".into())
                }
                Err(error) => Diagram::Failed(error.to_string()),
            };
            this.update(cx, |this, cx| {
                this.diagram_tasks.remove(&key);
                this.diagrams.borrow_mut().insert(key, diagram);
                cx.notify();
            })
            .log_err();
        });
        self.diagram_tasks.insert(key_for_task, task);
    }
}

#[cfg(test)]
mod tests {
    use super::mermaid_sources;

    #[test]
    fn finds_finished_mermaid_fences_only() {
        let markdown = "Text\n```mermaid\ngraph TD\n  A-->B\n```\n```rust\nfn x() {}\n```\n";
        assert_eq!(mermaid_sources(markdown), ["graph TD\n  A-->B\n"]);
        assert!(mermaid_sources("```mermaid\ngraph TD\n  A-->").is_empty());
    }
}
