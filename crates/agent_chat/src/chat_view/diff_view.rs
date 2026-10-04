use super::{ChatView, code_font};
use crate::chat_style::text_faint;
use agent_harness::ToolDiff;
use gpui::{
    AnyElement, App, Context, HighlightStyle, Hsla, IntoElement, ParentElement, SharedString,
    Styled, StyledText, div, hsla, px,
};
use language::{HighlightId, Rope};
use std::{ops::Range, path::Path, sync::Arc};
use theme::ActiveTheme as _;
use ui::prelude::*;
use util::ResultExt as _;

const CONTEXT_LINES: u32 = 3;
const MAX_DIFF_LINES: usize = 600;
const HUNK_HEADER_HEIGHT: f32 = 28.;
const DIFF_LINE_HEIGHT: f32 = 21.;
const DIFF_TEXT_SIZE: f32 = 12.;
const NOTICE_HEIGHT: f32 = 24.;
const BODY_BOTTOM_PAD: f32 = 8.;
const GUTTER_WIDTH: f32 = 36.;
const MARKER_WIDTH: f32 = 28.;
const ACCENT_BAR_WIDTH: f32 = 3.;
const CODE_PADDING_LEFT: f32 = 12.;

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
    new_file: bool,
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
    let mut diff_line_indices = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.kind != LineKind::Gap)
        .map(|(index, _)| index);
    let hidden = match diff_line_indices.nth(MAX_DIFF_LINES) {
        Some(cut) => {
            let hidden = 1 + diff_line_indices.count();
            lines.truncate(cut);
            while lines.last().is_some_and(|line| line.kind == LineKind::Gap) {
                lines.pop();
            }
            hidden
        }
        None => 0,
    };
    FileDiffView {
        lines,
        additions,
        deletions,
        hidden,
        new_file: diff.old_text.is_none(),
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
        let code_font = code_font(cx);
        let colors = cx.theme().colors();
        let status = cx.theme().status();
        let syntax = cx.theme().syntax().clone();
        let faint = text_faint(cx);
        let hunk_background = if cx.theme().appearance.is_light() {
            hsla(0.6, 0.35, 0.35, 0.07)
        } else {
            hsla(0.6, 0.35, 0.6, 0.05)
        };
        let max_line = view
            .lines
            .iter()
            .filter_map(|line| line.old_number.max(line.new_number))
            .max()
            .unwrap_or(1)
            .max(1);
        let gutter_width = ((max_line.ilog10() + 1) as f32 * 6.6 + 8. + 6.).max(GUTTER_WIDTH);
        let mut notices: Vec<String> = Vec::new();
        if view.new_file {
            notices.push("New file".to_string());
        }
        if view.hidden > 0 {
            notices.push(format!(
                "Diff truncated – showing first {MAX_DIFF_LINES} of {} lines",
                MAX_DIFF_LINES + view.hidden
            ));
        }
        let mut rows: Vec<AnyElement> = notices
            .into_iter()
            .map(|notice| {
                div()
                    .h(px(NOTICE_HEIGHT))
                    .w_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(16.))
                    .text_size(px(11.))
                    .text_color(faint)
                    .child(notice)
                    .into_any_element()
            })
            .collect();
        for hunk in hunks(&view.lines) {
            rows.push(
                div()
                    .h(px(HUNK_HEADER_HEIGHT))
                    .w_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(16.))
                    .bg(hunk_background)
                    .font(code_font.clone())
                    .text_size(px(11.))
                    .text_color(faint)
                    .child(hunk_header(hunk))
                    .into_any_element(),
            );
            for line in hunk {
                let (marker, marker_color, row_background, accent, number_color) = match line.kind
                {
                    LineKind::Added => (
                        "+",
                        status.created,
                        Some(status.created.opacity(0.055)),
                        Some(status.created.opacity(0.55)),
                        status.created.opacity(0.9),
                    ),
                    LineKind::Removed => (
                        "−",
                        status.deleted,
                        Some(status.deleted.opacity(0.055)),
                        Some(status.deleted.opacity(0.55)),
                        status.deleted.opacity(0.9),
                    ),
                    _ => ("·", faint.opacity(0.5), None, None, faint.opacity(0.8)),
                };
                let gutter = |number: Option<u32>, color: Hsla| {
                    div()
                        .w(px(gutter_width))
                        .flex_none()
                        .font(code_font.clone())
                        .text_size(px(11.))
                        .line_height(px(DIFF_LINE_HEIGHT))
                        .text_color(color)
                        .flex()
                        .justify_end()
                        .pr(px(8.))
                        .child(number.map(|number| number.to_string()).unwrap_or_default())
                };
                let highlights: Vec<(Range<usize>, HighlightStyle)> = line
                    .highlights
                    .iter()
                    .filter_map(|(range, id)| {
                        let style = syntax.get(*id).cloned()?;
                        (range.end <= line.text.len()).then(|| (range.clone(), style))
                    })
                    .collect();
                rows.push(
                    div()
                        .h(px(DIFF_LINE_HEIGHT))
                        .w_full()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_start()
                        .when_some(row_background, |this, background| this.bg(background))
                        .child(
                            div()
                                .w(px(ACCENT_BAR_WIDTH))
                                .self_stretch()
                                .flex_none()
                                .when_some(accent, |this, accent| this.bg(accent)),
                        )
                        .child(gutter(
                            line.old_number,
                            if line.kind == LineKind::Removed {
                                number_color
                            } else {
                                faint.opacity(0.8)
                            },
                        ))
                        .child(gutter(
                            line.new_number,
                            if line.kind == LineKind::Added {
                                number_color
                            } else {
                                faint.opacity(0.8)
                            },
                        ))
                        .child(
                            div()
                                .w(px(MARKER_WIDTH))
                                .flex_none()
                                .flex()
                                .justify_center()
                                .font(code_font.clone())
                                .text_size(px(DIFF_TEXT_SIZE))
                                .line_height(px(DIFF_LINE_HEIGHT))
                                .text_color(marker_color)
                                .child(marker),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .min_h(px(DIFF_LINE_HEIGHT))
                                .overflow_hidden()
                                .child(
                                    div()
                                        .pl(px(CODE_PADDING_LEFT))
                                        .font(code_font.clone())
                                        .text_size(px(DIFF_TEXT_SIZE))
                                        .line_height(px(DIFF_LINE_HEIGHT))
                                        .whitespace_nowrap()
                                        .text_color(colors.text.opacity(0.92))
                                        .child(
                                            StyledText::new(line.text.clone())
                                                .with_highlights(highlights),
                                        ),
                                ),
                        )
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .pb(px(BODY_BOTTOM_PAD))
            .children(rows)
            .into_any_element()
    }
}

fn hunks(lines: &[DiffLine]) -> impl Iterator<Item = &[DiffLine]> {
    lines
        .split(|line| line.kind == LineKind::Gap)
        .filter(|hunk| !hunk.is_empty())
}

fn hunk_header(hunk: &[DiffLine]) -> String {
    let old_numbers = hunk.iter().filter_map(|line| line.old_number);
    let new_numbers = hunk.iter().filter_map(|line| line.new_number);
    let old_start = old_numbers.clone().min().unwrap_or(0);
    let new_start = new_numbers.clone().min().unwrap_or(0);
    format!(
        "@@ -{old_start},{} +{new_start},{} @@",
        old_numbers.count(),
        new_numbers.count()
    )
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
        assert_eq!(hunk_header(&view.lines), "@@ -0,0 +1,2 @@");
    }

    #[test]
    fn emptied_files_start_the_new_side_at_zero() {
        let view = build(&ToolDiff {
            path: "gone.rs".into(),
            old_text: Some("a\n".into()),
            new_text: String::new(),
        });
        assert_eq!(hunk_header(&view.lines), "@@ -1,1 +0,0 @@");
    }

    #[test]
    fn the_line_cap_counts_only_diff_lines() {
        let old: String = (0..1000).map(|line| format!("line {line}\n")).collect();
        let new: String = (0..1000)
            .map(|line| {
                if line % 10 == 5 {
                    format!("changed {line}\n")
                } else {
                    format!("line {line}\n")
                }
            })
            .collect();
        let view = build(&ToolDiff {
            path: "a.rs".into(),
            old_text: Some(old),
            new_text: new,
        });
        let shown = view
            .lines
            .iter()
            .filter(|line| line.kind != LineKind::Gap)
            .count();
        assert_eq!(shown, MAX_DIFF_LINES);
        assert_eq!(view.hidden, 800 - MAX_DIFF_LINES);
        assert!(view.lines.last().is_some_and(|line| line.kind != LineKind::Gap));
    }
}
