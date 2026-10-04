use super::{CODE_FONT, ChatView};
use agent_harness::ToolDiff;
use gpui::{
    AnyElement, App, Context, HighlightStyle, IntoElement, ParentElement, SharedString, Styled,
    StyledText, div, px,
};
use language::{HighlightId, Rope};
use std::{ops::Range, path::Path, sync::Arc};
use theme::ActiveTheme as _;
use ui::prelude::*;
use util::ResultExt as _;

const CONTEXT_LINES: u32 = 3;
const MAX_DIFF_LINES: usize = 600;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Context,
    Added,
    Removed,
    Gap,
}

#[derive(Clone)]
struct DiffLine {
    kind: LineKind,
    old_number: Option<u32>,
    new_number: Option<u32>,
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightId)>,
}

pub(super) struct FileDiffView {
    lines: Vec<DiffLine>,
    pub additions: u32,
    pub deletions: u32,
    hidden: usize,
    highlighted: bool,
}

fn split_lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

fn build(diff: &ToolDiff) -> FileDiffView {
    let old = diff.old_text.as_deref().unwrap_or("");
    let old_lines = split_lines(old);
    let new_lines = split_lines(&diff.new_text);
    let hunks = language::line_diff(old, &diff.new_text);
    let mut lines = Vec::new();
    let (mut additions, mut deletions) = (0u32, 0u32);
    let mut old_position = 0u32;
    let line = |kind, old_number: Option<u32>, new_number: Option<u32>, text: &str| DiffLine {
        kind,
        old_number: old_number.map(|number| number + 1),
        new_number: new_number.map(|number| number + 1),
        text: text.to_string().into(),
        highlights: Vec::new(),
    };
    for (index, (before, after)) in hunks.iter().enumerate() {
        let delta = after.start as i64 - before.start as i64;
        let leading_start = before.start.saturating_sub(CONTEXT_LINES).max(old_position);
        if leading_start > old_position && !lines.is_empty() {
            lines.push(line(LineKind::Gap, None, None, ""));
        }
        for old_index in leading_start..before.start {
            let new_index = (old_index as i64 + delta) as u32;
            let text = old_lines.get(old_index as usize).copied().unwrap_or_default();
            lines.push(line(LineKind::Context, Some(old_index), Some(new_index), text));
        }
        for old_index in before.clone() {
            deletions += 1;
            let text = old_lines.get(old_index as usize).copied().unwrap_or_default();
            lines.push(line(LineKind::Removed, Some(old_index), None, text));
        }
        for new_index in after.clone() {
            additions += 1;
            let text = new_lines.get(new_index as usize).copied().unwrap_or_default();
            lines.push(line(LineKind::Added, None, Some(new_index), text));
        }
        let trailing_delta = after.end as i64 - before.end as i64;
        let next_start = hunks
            .get(index + 1)
            .map_or(old_lines.len() as u32, |(next, _)| next.start);
        let trailing_end = (before.end + CONTEXT_LINES).min(next_start).min(old_lines.len() as u32);
        for old_index in before.end..trailing_end {
            let new_index = (old_index as i64 + trailing_delta) as u32;
            let text = old_lines.get(old_index as usize).copied().unwrap_or_default();
            lines.push(line(LineKind::Context, Some(old_index), Some(new_index), text));
        }
        old_position = trailing_end.max(before.end);
    }
    let hidden = lines.len().saturating_sub(MAX_DIFF_LINES);
    lines.truncate(MAX_DIFF_LINES);
    FileDiffView {
        lines,
        additions,
        deletions,
        hidden,
        highlighted: false,
    }
}

fn highlight_lines(language: &Arc<language::Language>, text: &str) -> Vec<Vec<(Range<usize>, HighlightId)>> {
    let rope = Rope::from(text);
    let highlights = language.highlight_text(&rope, 0..text.len());
    let mut line_starts = vec![0];
    line_starts.extend(text.match_indices('\n').map(|(index, _)| index + 1));
    let mut per_line = vec![Vec::new(); line_starts.len()];
    for (range, id) in highlights {
        let first = line_starts.partition_point(|start| *start <= range.start).saturating_sub(1);
        let mut line_index = first;
        while let Some(start) = line_starts.get(line_index).copied() {
            if start >= range.end {
                break;
            }
            let end = line_starts
                .get(line_index + 1)
                .map_or(text.len(), |next| next.saturating_sub(1));
            let clipped = range.start.max(start)..range.end.min(end);
            if clipped.start < clipped.end
                && let Some(line) = per_line.get_mut(line_index)
            {
                line.push((clipped.start - start..clipped.end - start, id));
            }
            line_index += 1;
        }
    }
    per_line
}

impl ChatView {
    pub(super) fn diff_view(&self, tool_id: &str, diff: &ToolDiff, cx: &Context<Self>) -> Arc<FileDiffView> {
        if let Some(view) = self.diff_cache.borrow().get(tool_id) {
            return view.clone();
        }
        let view = Arc::new(build(diff));
        self.diff_cache
            .borrow_mut()
            .insert(tool_id.to_string(), view.clone());
        self.request_highlights(tool_id.to_string(), diff.clone(), cx);
        view
    }

    fn request_highlights(&self, tool_id: String, diff: ToolDiff, cx: &Context<Self>) {
        let languages = self.store.read(cx).languages().clone();
        let path = Path::new(&diff.path).to_path_buf();
        let task = cx.spawn(async move |this, cx| {
            let Some(language) = languages.load_language_for_file_path(&path).await.ok() else {
                return;
            };
            let highlighted = cx
                .background_spawn(async move {
                    let mut view = build(&diff);
                    let old = highlight_lines(&language, diff.old_text.as_deref().unwrap_or(""));
                    let new = highlight_lines(&language, &diff.new_text);
                    for line in &mut view.lines {
                        let source = match (line.kind, line.old_number, line.new_number) {
                            (LineKind::Added | LineKind::Context, _, Some(number)) => {
                                new.get(number as usize - 1)
                            }
                            (LineKind::Removed, Some(number), _) => old.get(number as usize - 1),
                            _ => None,
                        };
                        if let Some(highlights) = source {
                            line.highlights = highlights.clone();
                        }
                    }
                    view.highlighted = true;
                    view
                })
                .await;
            this.update(cx, |this, cx| {
                this.diff_cache
                    .borrow_mut()
                    .insert(tool_id, Arc::new(highlighted));
                cx.notify();
            })
            .log_err();
        });
        self.diff_tasks.borrow_mut().push(task);
    }

    pub(super) fn render_diff(&self, view: &FileDiffView, cx: &App) -> AnyElement {
        let colors = cx.theme().colors();
        let status = cx.theme().status();
        let syntax = cx.theme().syntax().clone();
        let number_width = view
            .lines
            .iter()
            .filter_map(|line| line.old_number.max(line.new_number))
            .max()
            .unwrap_or(1)
            .to_string()
            .len()
            .max(2) as f32
            * 7.5
            + 8.;
        let faint = colors.text_muted.opacity(0.6);
        v_flex()
            .my(px(6.))
            .rounded(px(8.))
            .border_1()
            .border_color(colors.border_variant)
            .overflow_hidden()
            .font_family(CODE_FONT)
            .text_size(px(12.))
            .line_height(px(18.))
            .children(view.lines.iter().map(|line| {
                if line.kind == LineKind::Gap {
                    return div()
                        .h(px(18.))
                        .px(px(8.))
                        .text_color(faint)
                        .bg(colors.editor_background.opacity(0.5))
                        .child("⋯")
                        .into_any_element();
                }
                let (background, sign, sign_color) = match line.kind {
                    LineKind::Added => (Some(status.created.opacity(0.12)), "+", status.created),
                    LineKind::Removed => (Some(status.deleted.opacity(0.12)), "-", status.deleted),
                    _ => (None, " ", faint),
                };
                let highlights: Vec<(Range<usize>, HighlightStyle)> = line
                    .highlights
                    .iter()
                    .filter_map(|(range, id)| {
                        let style = syntax.get(*id).cloned()?;
                        (range.end <= line.text.len()).then(|| (range.clone(), style))
                    })
                    .collect();
                h_flex()
                    .w_full()
                    .when_some(background, |this, background| this.bg(background))
                    .child(
                        div()
                            .flex_none()
                            .w(px(number_width))
                            .pr(px(6.))
                            .text_right()
                            .text_color(faint)
                            .child(line.old_number.map(|number| number.to_string()).unwrap_or_default()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(number_width))
                            .pr(px(6.))
                            .text_right()
                            .text_color(faint)
                            .child(line.new_number.map(|number| number.to_string()).unwrap_or_default()),
                    )
                    .child(div().flex_none().w(px(14.)).text_color(sign_color).child(sign))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_color(colors.text)
                            .child(StyledText::new(line.text.clone()).with_highlights(highlights)),
                    )
                    .into_any_element()
            }))
            .when(view.hidden > 0, |this| {
                this.child(
                    div()
                        .px(px(8.))
                        .text_color(faint)
                        .child(format!("… {} more lines", view.hidden)),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(view: &FileDiffView) -> String {
        view.lines
            .iter()
            .map(|line| match line.kind {
                LineKind::Context => ' ',
                LineKind::Added => '+',
                LineKind::Removed => '-',
                LineKind::Gap => '~',
            })
            .collect()
    }

    #[test]
    fn hunks_keep_three_lines_of_context_and_split_with_gaps() {
        let old: String = (1..=20).map(|line| format!("line {line}\n")).collect();
        let new = old.replace("line 2\n", "line two\n").replace("line 18\n", "line eighteen\n");
        let view = build(&ToolDiff {
            path: "a.rs".into(),
            old_text: Some(old),
            new_text: new,
        });
        assert_eq!((view.additions, view.deletions), (2, 2));
        assert_eq!(kinds(&view), " -+   ~   -+  ");
        assert_eq!(view.lines[1].old_number, Some(2));
        assert_eq!(view.lines[2].new_number, Some(2));
    }

    #[test]
    fn new_files_are_all_additions() {
        let view = build(&ToolDiff {
            path: "new.rs".into(),
            old_text: None,
            new_text: "a\nb\n".into(),
        });
        assert_eq!(kinds(&view), "++");
        assert_eq!((view.additions, view.deletions), (2, 0));
    }
}
