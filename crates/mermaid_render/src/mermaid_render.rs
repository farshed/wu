use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const MAX_SOURCE_BYTES: usize = 16 * 1024;
pub const MAX_SOURCE_LINES: usize = 256;
pub const MAX_SOURCE_TOKENS: usize = 2048;
pub const MAX_SVG_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CACHE_ENTRIES: usize = 64;

// The renderer cannot be cancelled, so renders run one at a time.
static RENDER_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    fn over(self, base: Rgba) -> Self {
        let mix = |top: f32, bottom: f32| top * self.a + bottom * (1.0 - self.a);
        Self {
            r: mix(self.r, base.r),
            g: mix(self.g, base.g),
            b: mix(self.b, base.b),
            a: 1.0,
        }
    }

    fn opaque(self) -> Self {
        self.with_alpha(1.0)
    }

    fn luminance(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    fn hex(self) -> String {
        let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!(
            "#{:02x}{:02x}{:02x}",
            channel(self.r),
            channel(self.g),
            channel(self.b)
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiagramTheme {
    pub font_family: String,
    /// Never painted: the diagram is transparent and this only fills label backdrops and masks.
    pub background: Rgba,
    pub text: Rgba,
    pub line: Rgba,
    pub node_fill: Rgba,
    pub node_border: Rgba,
    pub accent: Rgba,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderedDiagram {
    pub svg: Arc<[u8]>,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("Diagram exceeds preview complexity limit")]
    TooComplex,
    #[error("Diagram output exceeds preview size limit")]
    OutputTooLarge,
    #[error("{0}")]
    Invalid(String),
    #[error("Diagram could not be rendered")]
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Palette {
    dark: bool,
    font: String,
    canvas: String,
    node: String,
    group: String,
    text: String,
    line: String,
    border: String,
    grid: String,
    accent_line: String,
    accent_wash: String,
}

impl Palette {
    fn new(theme: &DiagramTheme) -> Self {
        let canvas = theme.background.opaque();
        let node = theme.node_fill.over(canvas);
        Self {
            dark: canvas.luminance() < 0.5,
            font: theme.font_family.clone(),
            canvas: canvas.hex(),
            node: node.hex(),
            group: theme.text.with_alpha(0.03).over(canvas).hex(),
            text: theme.text.over(node).hex(),
            line: theme.line.over(canvas).hex(),
            border: theme.node_border.over(canvas).hex(),
            grid: theme
                .node_border
                .with_alpha(theme.node_border.a * 0.5)
                .over(canvas)
                .hex(),
            accent_line: theme.accent.with_alpha(0.6).over(canvas).hex(),
            accent_wash: theme.accent.with_alpha(0.12).over(node).hex(),
        }
    }
}

/// Blocks on CPU work; call it off the UI thread.
pub fn render(source: &str, theme: &DiagramTheme) -> Result<RenderedDiagram, RenderError> {
    if exceeds_complexity_limit(source) {
        return Err(RenderError::TooComplex);
    }
    let palette = Palette::new(theme);
    let svg = {
        let _guard = RENDER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::panic::catch_unwind(|| render_svg(source, &palette))
            .unwrap_or(Err(RenderError::Failed))?
    };
    if svg.len() > MAX_SVG_BYTES {
        return Err(RenderError::OutputTooLarge);
    }
    let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default())
        .map_err(|_| RenderError::Failed)?;
    let size = tree.size();
    Ok(RenderedDiagram {
        svg: Arc::from(svg.into_bytes()),
        width: size.width(),
        height: size.height(),
    })
}

fn exceeds_complexity_limit(source: &str) -> bool {
    source.len() > MAX_SOURCE_BYTES
        || source.lines().count() > MAX_SOURCE_LINES
        || source
            .split(|c: char| c.is_whitespace() || matches!(c, ';' | '>' | '{' | '}'))
            .count()
            > MAX_SOURCE_TOKENS
}

fn render_svg(source: &str, palette: &Palette) -> Result<String, RenderError> {
    let mut options = mermaid_rs_renderer::RenderOptions::default();
    options.theme = if palette.dark {
        mermaid_rs_renderer::Theme::dark()
    } else {
        mermaid_rs_renderer::Theme::modern()
    };
    options.layout.node_spacing = 36.0;
    options.layout.rank_spacing = 40.0;
    options.layout.node_padding_x = 18.0;
    options.layout.node_padding_y = 10.0;
    let theme = &mut options.theme;
    theme.font_family = palette.font.clone();
    theme.font_size = 14.0;
    theme.background = palette.canvas.clone();
    theme.primary_color = palette.node.clone();
    theme.primary_text_color = palette.text.clone();
    theme.primary_border_color = palette.border.clone();
    theme.text_color = palette.text.clone();
    theme.line_color = palette.line.clone();
    theme.secondary_color = palette.node.clone();
    theme.tertiary_color = palette.group.clone();
    theme.edge_label_background = palette.canvas.clone();
    theme.cluster_background = palette.group.clone();
    theme.cluster_border = palette.border.clone();
    theme.sequence_actor_fill = palette.node.clone();
    theme.sequence_actor_border = palette.border.clone();
    theme.sequence_actor_line = palette.border.clone();
    theme.sequence_note_fill = palette.accent_wash.clone();
    theme.sequence_note_border = palette.accent_line.clone();
    theme.sequence_activation_fill = palette.accent_wash.clone();
    theme.sequence_activation_border = palette.accent_line.clone();
    if diagram_keyword(source) == Some("gantt") {
        // Gantt derives its bar hues from this color; a neutral gray turns every section red.
        theme.primary_border_color = palette.accent_line.clone();
    }
    let svg = mermaid_rs_renderer::render_with_options(source, options)
        .map_err(|error| RenderError::Invalid(error.to_string()))?;
    Ok(restyle(svg, palette))
}

fn diagram_keyword(source: &str) -> Option<&str> {
    let mut lines = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .peekable();
    if lines.next_if_eq(&"---").is_some() {
        lines.by_ref().find(|line| *line == "---");
    }
    lines
        .find(|line| !line.starts_with("%%"))
        .and_then(|line| line.split_whitespace().next())
}

fn restyle(svg: String, palette: &Palette) -> String {
    let canvas_fill = format!(" fill=\"{}\"", palette.canvas);
    let node_fill = format!(" fill=\"{}\"", palette.node);
    let border_stroke = format!(" stroke=\"{}\"", palette.border);
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg.as_str();
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..=end];
        rest = &rest[end + 1..];
        if is_canvas(tag, palette) {
            continue;
        }
        if tag.starts_with("<polygon ") && is_default_diamond(tag, palette) {
            out.push_str(
                &tag.replacen(&node_fill, &format!(" fill=\"{}\"", palette.accent_wash), 1)
                    .replacen(
                        &border_stroke,
                        &format!(" stroke=\"{}\"", palette.accent_line),
                        1,
                    ),
            );
        } else if tag.starts_with("<rect ") {
            let mut tag = tag.replacen(" rx=\"3\" ry=\"3\" ", " rx=\"8\" ry=\"8\" ", 1);
            if tag.contains(&border_stroke) {
                tag = tag.replacen(&canvas_fill, &node_fill, 1);
            }
            out.push_str(&tag);
        } else if tag.starts_with("<line ") {
            out.push_str(&tag.replacen(
                " stroke=\"#E2E8F0\"",
                &format!(" stroke=\"{}\"", palette.grid),
                1,
            ));
        } else {
            out.push_str(tag);
        }
    }
    out.push_str(rest);
    out
}

fn attributes(tag: &str) -> Vec<(&str, &str)> {
    let parts: Vec<_> = tag.split('"').collect();
    parts
        .as_chunks::<2>()
        .0
        .iter()
        .filter_map(|[name, value]| {
            let name = name.trim_end().strip_suffix('=')?;
            Some((name.rsplit(' ').next()?, *value))
        })
        .collect()
}

fn is_canvas(tag: &str, palette: &Palette) -> bool {
    if !tag.starts_with("<rect ") {
        return false;
    }
    let mut fill = None;
    for (name, value) in attributes(tag) {
        match name {
            "x" | "y" | "width" | "height" => {}
            "fill" => fill = Some(value),
            _ => return false,
        }
    }
    fill == Some(palette.canvas.as_str())
}

fn is_default_diamond(tag: &str, palette: &Palette) -> bool {
    let attributes = attributes(tag);
    let get = |key: &str| {
        attributes
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
    };
    if get("fill") != Some(palette.node.as_str()) || get("stroke") != Some(palette.border.as_str())
    {
        return false;
    }
    let Some(points) = get("points") else {
        return false;
    };
    let points: Vec<(f32, f32)> = points
        .split_whitespace()
        .filter_map(|point| {
            let (x, y) = point.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        })
        .collect();
    let [top, right, bottom, left] = points[..] else {
        return false;
    };
    (top.0 - bottom.0).abs() < 0.05
        && (left.1 - right.1).abs() < 0.05
        && left.0 < top.0
        && top.0 < right.0
        && top.1 < left.1
        && left.1 < bottom.1
}

pub trait RetainedBytes {
    fn retained_bytes(&self) -> usize;
}

impl RetainedBytes for RenderedDiagram {
    fn retained_bytes(&self) -> usize {
        self.svg.len()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lookup<T> {
    Pending,
    Ready(T),
    Failed(RenderError),
}

#[derive(Debug)]
pub struct Finished<T> {
    pub owners: Vec<String>,
    pub evicted: Vec<T>,
}

struct Entry<T> {
    state: Lookup<T>,
    last_requested_frame: u64,
    owners: Vec<String>,
}

/// Request every painted diagram each frame: sources not requested in the last two frames are dropped or evicted.
pub struct DiagramCache<T = RenderedDiagram> {
    entries: HashMap<String, Entry<T>>,
    frame: u64,
    theme: Option<DiagramTheme>,
    new_requests: bool,
}

impl<T> Default for DiagramCache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::default(),
            frame: 0,
            theme: None,
            new_requests: false,
        }
    }
}

impl<T: Clone + RetainedBytes> DiagramCache<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns every retained diagram when the theme changed, since the old colors are baked in.
    pub fn begin_frame(&mut self, theme: &DiagramTheme) -> Vec<T> {
        self.frame += 1;
        if self.theme.as_ref() == Some(theme) {
            return Vec::new();
        }
        self.theme = Some(theme.clone());
        self.drain()
    }

    pub fn request(&mut self, source: &str, owner: &str) -> Lookup<T> {
        let frame = self.frame;
        let entry = match self.entries.get_mut(source) {
            Some(entry) => entry,
            None => {
                self.new_requests = true;
                self.entries.entry(source.to_owned()).or_insert(Entry {
                    state: Lookup::Pending,
                    last_requested_frame: frame,
                    owners: Vec::new(),
                })
            }
        };
        entry.last_requested_frame = frame;
        if !entry.owners.iter().any(|known| known == owner) {
            entry.owners.push(owner.to_owned());
        }
        entry.state.clone()
    }

    pub fn take_new_requests(&mut self) -> bool {
        std::mem::take(&mut self.new_requests)
    }

    // Two frames, because a request can arrive while the current frame has laid out only some of its views.
    pub fn next_job(&mut self) -> Option<String> {
        let frame = self.frame;
        self.entries.retain(|_, entry| {
            !matches!(entry.state, Lookup::Pending)
                || frame.saturating_sub(entry.last_requested_frame) <= 1
        });
        self.entries
            .iter()
            .filter(|(_, entry)| matches!(entry.state, Lookup::Pending))
            .max_by_key(|(_, entry)| entry.last_requested_frame)
            .map(|(source, _)| source.clone())
    }

    /// Returns `None` when the theme changed while the diagram rendered.
    pub fn finish(
        &mut self,
        source: String,
        theme: &DiagramTheme,
        result: Result<T, RenderError>,
    ) -> Option<Finished<T>> {
        if self.theme.as_ref() != Some(theme) {
            return None;
        }
        let state = match result {
            Ok(diagram) => Lookup::Ready(diagram),
            Err(error) => Lookup::Failed(error),
        };
        let frame = self.frame;
        let entry = self.entries.entry(source).or_insert(Entry {
            state: Lookup::Pending,
            last_requested_frame: frame,
            owners: Vec::new(),
        });
        entry.state = state;
        let owners = entry.owners.clone();
        Some(Finished {
            owners,
            evicted: self.evict(),
        })
    }

    fn evict(&mut self) -> Vec<T> {
        let mut evicted = Vec::new();
        let frame = self.frame;
        while self.retained_bytes() > MAX_RETAINED_BYTES || self.entries.len() > MAX_CACHE_ENTRIES {
            let Some(source) = self
                .entries
                .iter()
                .filter(|(_, entry)| {
                    !matches!(entry.state, Lookup::Pending)
                        && frame.saturating_sub(entry.last_requested_frame) > 1
                })
                .min_by_key(|(_, entry)| entry.last_requested_frame)
                .map(|(source, _)| source.clone())
            else {
                break;
            };
            if let Some(Entry {
                state: Lookup::Ready(diagram),
                ..
            }) = self.entries.remove(&source)
            {
                evicted.push(diagram);
            }
        }
        evicted
    }

    pub fn retained_bytes(&self) -> usize {
        self.entries
            .values()
            .filter_map(|entry| match &entry.state {
                Lookup::Ready(diagram) => Some(diagram.retained_bytes()),
                _ => None,
            })
            .sum()
    }

    pub fn drain(&mut self) -> Vec<T> {
        self.entries
            .drain()
            .filter_map(|(_, entry)| match entry.state {
                Lookup::Ready(diagram) => Some(diagram),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(hex: u32) -> Rgba {
        Rgba {
            r: ((hex >> 16) & 0xff) as f32 / 255.0,
            g: ((hex >> 8) & 0xff) as f32 / 255.0,
            b: (hex & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    fn light_theme() -> DiagramTheme {
        DiagramTheme {
            font_family: "Helvetica".into(),
            background: rgb(0xf6f6f6),
            text: rgb(0x1f2328),
            line: rgb(0x8c959f),
            node_fill: rgb(0xffffff),
            node_border: rgb(0xd0d7de),
            accent: rgb(0x0969da),
        }
    }

    fn dark_theme() -> DiagramTheme {
        DiagramTheme {
            font_family: "Helvetica".into(),
            background: rgb(0x1e1e1e),
            text: rgb(0xe6edf3),
            line: rgb(0x7d8590),
            node_fill: rgb(0x2a2a2a),
            node_border: rgb(0x484f58),
            accent: rgb(0x4493f8),
        }
    }

    const FLOWCHART: &str = "flowchart TD\n    A[Start] --> B{Ready?}\n    B -->|Yes| C[Ship]\n    B -->|No| D[Fix]\n    D --> B";
    const SEQUENCE: &str = "sequenceDiagram\n    participant User\n    participant Server\n    User->>Server: Request\n    activate Server\n    Note right of Server: Thinking\n    Server-->>User: Response\n    deactivate Server";

    fn assert_renders(source: &str, theme: &DiagramTheme) -> RenderedDiagram {
        let diagram = render(source, theme).expect("diagram should render");
        let svg = std::str::from_utf8(&diagram.svg).expect("svg should be utf-8");
        assert!(svg.starts_with("<svg") || svg.contains("<svg "));
        assert!(!svg.contains("<foreignObject"));
        assert!(diagram.width > 0.0 && diagram.height > 0.0);
        assert_eq!(render(source, theme), Ok(diagram.clone()));
        diagram
    }

    #[test]
    fn flowchart_renders_in_both_themes() {
        for theme in [light_theme(), dark_theme()] {
            let diagram = assert_renders(FLOWCHART, &theme);
            let svg = std::str::from_utf8(&diagram.svg).expect("svg should be utf-8");
            assert!(svg.contains("Start") && svg.contains("Ship"));
        }
    }

    #[test]
    fn sequence_diagram_renders_in_both_themes() {
        for theme in [light_theme(), dark_theme()] {
            let diagram = assert_renders(SEQUENCE, &theme);
            let svg = std::str::from_utf8(&diagram.svg).expect("svg should be utf-8");
            assert!(svg.contains("Request") && svg.contains("Response"));
            assert!(svg.contains(&Palette::new(&theme).accent_wash));
        }
    }

    #[test]
    fn malformed_and_oversized_diagrams_are_rejected() {
        let theme = dark_theme();
        assert!(matches!(
            render("this is not a diagram", &theme),
            Err(RenderError::Invalid(_))
        ));
        assert_eq!(
            render(&"x".repeat(MAX_SOURCE_BYTES + 1), &theme),
            Err(RenderError::TooComplex)
        );
        assert_eq!(
            render(&"A\n".repeat(MAX_SOURCE_LINES + 1), &theme),
            Err(RenderError::TooComplex)
        );
        assert_eq!(
            render(
                &format!("flowchart LR\n{}", "A;".repeat(MAX_SOURCE_TOKENS)),
                &theme
            ),
            Err(RenderError::TooComplex)
        );
        assert!(render("flowchart TD\nA[Hola<br/>mundo] --> B[Fin]", &theme).is_ok());
    }

    #[test]
    fn diagrams_take_theme_style_and_keep_explicit_colors() {
        for theme in [light_theme(), dark_theme()] {
            let palette = Palette::new(&theme);
            let diagram = render(
                "flowchart TD\nA[Start] --> B{Ready?}\nB --> C[End]\nB --> D{Other}\nstyle D fill:#dbeafe,stroke:#2563eb",
                &theme,
            )
            .expect("diagram should render");
            let svg = std::str::from_utf8(&diagram.svg).expect("svg should be utf-8");
            assert!(!svg.contains(&format!("fill=\"{}\"/>", palette.canvas)));
            assert!(svg.contains(" rx=\"8\" ry=\"8\" "));
            assert!(!svg.contains(" rx=\"3\" ry=\"3\" "));
            let diamonds = svg.matches("<polygon ").count();
            let accented = svg
                .matches(&format!(
                    "fill=\"{}\" stroke=\"{}\"",
                    palette.accent_wash, palette.accent_line
                ))
                .count();
            assert!(diamonds >= 2);
            assert_eq!(accented, 1, "only the default-colored decision is tinted");
            assert!(svg.contains("fill=\"#dbeafe\" stroke=\"#2563eb\""));

            let gantt = render(
                "%% plan\ngantt\ndateFormat YYYY-MM-DD\nsection A\nTask :2026-09-08, 2d",
                &theme,
            )
            .expect("gantt should render");
            let gantt = std::str::from_utf8(&gantt.svg).expect("svg should be utf-8");
            assert!(!gantt.contains("#E2E8F0"));
        }
    }

    #[test]
    fn diagram_keyword_skips_front_matter_and_comments() {
        assert_eq!(diagram_keyword("gantt\n  title X"), Some("gantt"));
        assert_eq!(
            diagram_keyword("---\ntitle: Plan\n---\n%% note\n\n gantt"),
            Some("gantt")
        );
        assert_eq!(diagram_keyword("flowchart LR; A-->B"), Some("flowchart"));
        assert_eq!(diagram_keyword("  \n%% only"), None);
    }

    #[derive(Clone, Debug, PartialEq)]
    struct Weighted(usize);

    impl RetainedBytes for Weighted {
        fn retained_bytes(&self) -> usize {
            self.0
        }
    }

    #[test]
    fn requests_queue_once_and_track_their_owners() {
        let theme = light_theme();
        let mut cache = DiagramCache::<Weighted>::new();
        assert!(cache.begin_frame(&theme).is_empty());
        assert_eq!(cache.request("graph TD; A-->B", "row-0"), Lookup::Pending);
        assert_eq!(cache.request("graph TD; A-->B", "row-2"), Lookup::Pending);
        assert!(cache.take_new_requests());
        assert!(!cache.take_new_requests());
        let source = cache.next_job().expect("a job should be queued");
        let finished = cache
            .finish(source, &theme, Ok(Weighted(80)))
            .expect("theme is unchanged");
        assert_eq!(finished.owners, ["row-0", "row-2"]);
        assert!(finished.evicted.is_empty());
        assert_eq!(
            cache.request("graph TD; A-->B", "row-0"),
            Lookup::Ready(Weighted(80))
        );
        assert!(!cache.take_new_requests());
        assert!(cache.next_job().is_none());
    }

    #[test]
    fn requests_that_scrolled_away_are_dropped_before_rendering() {
        let theme = light_theme();
        let mut cache = DiagramCache::<Weighted>::new();
        cache.begin_frame(&theme);
        cache.request("a", "row");
        cache.begin_frame(&theme);
        cache.request("b", "row2");
        assert!(cache.next_job().is_some());
        cache.begin_frame(&theme);
        cache.request("b", "row2");
        assert_eq!(cache.next_job().as_deref(), Some("b"));
        cache.finish("b".into(), &theme, Err(RenderError::Failed));
        assert!(cache.next_job().is_none());
        assert_eq!(
            cache.request("b", "row2"),
            Lookup::Failed(RenderError::Failed)
        );
    }

    #[test]
    fn theme_changes_discard_diagrams_and_obsolete_results() {
        let light = light_theme();
        let dark = dark_theme();
        let mut cache = DiagramCache::<Weighted>::new();
        cache.begin_frame(&light);
        cache.request("a", "row");
        cache.finish("a".into(), &light, Ok(Weighted(80)));
        assert_eq!(cache.begin_frame(&dark).len(), 1);
        assert_eq!(cache.request("a", "row"), Lookup::Pending);
        assert!(cache.finish("a".into(), &light, Ok(Weighted(80))).is_none());
        assert_eq!(cache.request("a", "row"), Lookup::Pending);
    }

    #[test]
    fn eviction_spares_diagrams_painted_in_the_latest_frames() {
        let theme = light_theme();
        let mut cache = DiagramCache::<Weighted>::new();
        for index in 0..MAX_CACHE_ENTRIES {
            cache.begin_frame(&theme);
            let source = format!("graph {index}");
            cache.request(&source, &format!("row{index}"));
            cache.finish(source, &theme, Err(RenderError::Failed));
        }
        cache.begin_frame(&theme);
        cache.begin_frame(&theme);
        cache.request("graph 0", "row0");
        cache.request("fresh", "fresh");
        cache.finish("fresh".into(), &theme, Ok(Weighted(80)));
        assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
        assert!(cache.entries.contains_key("graph 0"));
        assert!(cache.entries.contains_key("fresh"));
        assert!(!cache.entries.contains_key("graph 1"));
    }

    #[test]
    fn eviction_keeps_retained_bytes_within_budget() {
        let theme = light_theme();
        let mut cache = DiagramCache::<Weighted>::new();
        cache.begin_frame(&theme);
        cache.request("old", "row");
        cache.finish("old".into(), &theme, Ok(Weighted(MAX_RETAINED_BYTES)));
        cache.begin_frame(&theme);
        cache.begin_frame(&theme);
        cache.request("new", "row");
        let finished = cache
            .finish("new".into(), &theme, Ok(Weighted(1)))
            .expect("theme is unchanged");
        assert_eq!(finished.evicted, [Weighted(MAX_RETAINED_BYTES)]);
        assert_eq!(cache.retained_bytes(), 1);
    }
}
