pub(super) mod blame;

use super::*;
use ::git::{
    Oid, Restore, blame::BlameEntry, commit::ParsedCommitMessage, repository::RepoPath,
    status::FileStatus,
};
use buffer_diff::{BufferDiff, DiffHunkStatus, DiffHunkStatusKind};
use project::git_store::Repository;

#[derive(Clone)]
pub struct ResolvedDiffHunk {
    pub buffer_range: Range<text::Anchor>,
    pub diff_base_byte_range: Range<usize>,
    pub status: DiffHunkStatus,
}

#[derive(Clone)]
pub struct ResolvedDiffHunks {
    pub diff: Entity<BufferDiff>,
    pub buffer_id: BufferId,
    pub buffer: Option<Entity<Buffer>>,
    pub hunks: Vec<ResolvedDiffHunk>,
}

pub trait DiffHunkDelegate {
    fn toggle(
        &self,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        window: &mut Window,
        cx: &mut Context<Editor>,
    );

    fn stage_or_unstage(
        &self,
        stage: bool,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        window: &mut Window,
        cx: &mut Context<Editor>,
    );

    fn restore(
        &self,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        if hunks.is_empty() || editor.read_only(cx) {
            return;
        }
        self.stage_or_unstage(false, hunks.clone(), editor, window, cx);
        editor.transact(window, cx, |editor, window, cx| {
            editor.restore_diff_hunks(hunks, cx);
            let selections = editor
                .selections
                .all::<MultiBufferOffset>(&editor.display_snapshot(cx));
            editor.change_selections(
                SelectionEffects::no_scroll(),
                window,
                cx,
                |selections_state| {
                    selections_state.select(selections);
                },
            );
        });
    }

    fn render_hunk_controls(
        &self,
        row: u32,
        status: &DiffHunkStatus,
        hunk_range: Range<Anchor>,
        is_created_file: bool,
        line_height: Pixels,
        editor: &Entity<Editor>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement;

    fn render_hunk_as_staged(&self, status: &DiffHunkStatus, _cx: &App) -> bool {
        !status.has_secondary_hunk()
    }
}

pub struct UncommittedDiffHunkDelegate;

impl DiffHunkDelegate for UncommittedDiffHunkDelegate {
    fn toggle(
        &self,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let stage = hunks
            .iter()
            .flat_map(|hunks| hunks.hunks.iter())
            .any(|hunk| hunk.status.has_secondary_hunk());
        self.stage_or_unstage(stage, hunks, editor, window, cx);
    }

    fn stage_or_unstage(
        &self,
        stage: bool,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        _window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let Some(project) = editor.project() else {
            return;
        };
        for hunks in hunks {
            let Some(buffer) = hunks.buffer else {
                continue;
            };

            let ranges = hunks
                .hunks
                .into_iter()
                .map(|hunk| hunk.buffer_range)
                .collect::<Vec<_>>();
            if ranges.is_empty() {
                continue;
            }
            let secondary_diff = hunks.diff.read(cx).secondary_diff();
            project
                .update(cx, |project, cx| {
                    if stage {
                        let Some(secondary_diff) = secondary_diff else {
                            return Err(anyhow::anyhow!("diff has no unstaged secondary"));
                        };
                        project.stage_hunks(buffer, secondary_diff, ranges, cx)
                    } else {
                        project.unstage_uncommitted_hunks(buffer, hunks.diff, ranges, cx)
                    }
                })
                .log_err();
        }
    }

    fn render_hunk_controls(
        &self,
        row: u32,
        status: &DiffHunkStatus,
        hunk_range: Range<Anchor>,
        is_created_file: bool,
        line_height: Pixels,
        editor: &Entity<Editor>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        render_diff_hunk_controls(
            row,
            status,
            hunk_range,
            is_created_file,
            line_height,
            editor,
            window,
            cx,
        )
    }
}

pub struct RestoreOnlyDiffHunkDelegate;

impl DiffHunkDelegate for RestoreOnlyDiffHunkDelegate {
    fn toggle(
        &self,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn stage_or_unstage(
        &self,
        _stage: bool,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn restore(
        &self,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn render_hunk_controls(
        &self,
        _row: u32,
        _status: &DiffHunkStatus,
        _hunk_range: Range<Anchor>,
        _is_created_file: bool,
        _line_height: Pixels,
        _editor: &Entity<Editor>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> AnyElement {
        gpui::Empty.into_any_element()
    }
}

pub struct RestoreOnlyUnstagedDiffHunkDelegate;

impl DiffHunkDelegate for RestoreOnlyUnstagedDiffHunkDelegate {
    fn toggle(
        &self,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn stage_or_unstage(
        &self,
        _stage: bool,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn render_hunk_controls(
        &self,
        _row: u32,
        _status: &DiffHunkStatus,
        _hunk_range: Range<Anchor>,
        _is_created_file: bool,
        _line_height: Pixels,
        _editor: &Entity<Editor>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> AnyElement {
        gpui::Empty.into_any_element()
    }

    fn render_hunk_as_staged(&self, _status: &DiffHunkStatus, _cx: &App) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DisplayDiffHunk {
    Folded {
        display_row: DisplayRow,
    },
    Unfolded {
        is_created_file: bool,
        diff_base_byte_range: Range<usize>,
        display_row_range: Range<DisplayRow>,
        multi_buffer_range: Range<Anchor>,
        status: DiffHunkStatus,
        word_diffs: Vec<Range<MultiBufferOffset>>,
    },
}

#[derive(Clone)]
pub(super) struct InlineBlamePopoverState {
    pub(super) scroll_handle: ScrollHandle,
    pub(super) commit_message: Option<ParsedCommitMessage>,
    pub(super) markdown: Entity<Markdown>,
}

pub(super) struct InlineBlamePopover {
    pub(super) position: gpui::Point<Pixels>,
    pub(super) hide_task: Option<Task<()>>,
    pub(super) popover_bounds: Option<Bounds<Pixels>>,
    pub(super) popover_state: InlineBlamePopoverState,
    pub(super) keyboard_grace: bool,
}

impl Editor {
    pub fn diff_hunks_in_ranges<'a>(
        &'a self,
        ranges: &'a [Range<Anchor>],
        buffer: &'a MultiBufferSnapshot,
    ) -> impl 'a + Iterator<Item = MultiBufferDiffHunk> {
        ranges.iter().flat_map(move |range| {
            let end_excerpt = buffer.excerpt_containing(range.end..range.end);
            let range = range.to_point(buffer);
            let mut peek_end = range.end;
            if range.end.row < buffer.max_row().0 {
                peek_end = Point::new(range.end.row + 1, 0);
            }
            buffer
                .diff_hunks_in_range(range.start..peek_end)
                .filter(move |hunk| {
                    if let Some((_, excerpt_range)) = &end_excerpt
                        && let Some(end_anchor) =
                            buffer.anchor_in_excerpt(excerpt_range.context.end)
                        && let Some(hunk_end_anchor) =
                            buffer.anchor_in_excerpt(hunk.excerpt_range.context.end)
                        && hunk_end_anchor.cmp(&end_anchor, buffer).is_gt()
                    {
                        false
                    } else {
                        true
                    }
                })
        })
    }

    fn resolve_diff_hunks(
        &self,
        hunks: Vec<MultiBufferDiffHunk>,
        cx: &App,
    ) -> Vec<ResolvedDiffHunks> {
        let multibuffer = self.buffer().read(cx);
        let chunk_by = hunks.into_iter().chunk_by(|hunk| hunk.buffer_id);
        let mut resolved = Vec::new();

        for (source_buffer_id, hunks) in &chunk_by {
            let Some(diff) = multibuffer.diff_for(source_buffer_id) else {
                continue;
            };
            let diff_snapshot = diff.read(cx).snapshot(cx);
            let main_buffer_id = diff_snapshot.buffer_id();
            let buffer = multibuffer.buffer(main_buffer_id).or_else(|| {
                self.project
                    .as_ref()
                    .and_then(|project| project.read(cx).buffer_for_id(main_buffer_id, cx))
            });
            let mut resolved_hunks = Vec::new();

            for hunk in hunks {
                if hunk.buffer_id == main_buffer_id {
                    resolved_hunks.push(ResolvedDiffHunk {
                        buffer_range: hunk.buffer_range,
                        diff_base_byte_range: hunk.diff_base_byte_range.start.0
                            ..hunk.diff_base_byte_range.end.0,
                        status: hunk.status,
                    });
                } else {
                    let diff_base_byte_range =
                        hunk.diff_base_byte_range.start.0..hunk.diff_base_byte_range.end.0;
                    let Some(hunk) = diff_snapshot
                        .hunks_intersecting_base_text_range(
                            diff_base_byte_range.clone(),
                            diff_snapshot.buffer_snapshot(),
                        )
                        .find(|hunk| hunk.diff_base_byte_range == diff_base_byte_range)
                    else {
                        continue;
                    };
                    let kind = if hunk.buffer_range.start == hunk.buffer_range.end {
                        DiffHunkStatusKind::Deleted
                    } else if hunk.diff_base_byte_range.is_empty() {
                        DiffHunkStatusKind::Added
                    } else {
                        DiffHunkStatusKind::Modified
                    };
                    resolved_hunks.push(ResolvedDiffHunk {
                        buffer_range: hunk.buffer_range,
                        diff_base_byte_range: hunk.diff_base_byte_range,
                        status: DiffHunkStatus {
                            kind,
                            secondary: hunk.secondary_status,
                        },
                    });
                }
            }

            if !resolved_hunks.is_empty() {
                resolved.push(ResolvedDiffHunks {
                    diff,
                    buffer_id: main_buffer_id,
                    buffer,
                    hunks: resolved_hunks,
                });
            }
        }

        resolved
    }

    pub fn diff_hunk_delegate(&self) -> Arc<dyn DiffHunkDelegate> {
        self.diff_hunk_delegate
            .clone()
            .unwrap_or_else(|| Arc::new(UncommittedDiffHunkDelegate))
    }

    pub fn set_diff_hunk_delegate(
        &mut self,
        delegate: Option<Arc<dyn DiffHunkDelegate>>,
        cx: &mut Context<Self>,
    ) {
        let had_delegate = self.diff_hunk_delegate.is_some();
        let has_delegate = delegate.is_some();
        self.diff_hunk_delegate = delegate;

        if !had_delegate && has_delegate {
            self.load_diff_task.take();
        } else if had_delegate && !has_delegate {
            self.buffer.update(cx, |buffer, cx| {
                buffer.set_all_diff_hunks_collapsed(cx);
            });

            if let Some(project) = self.project.clone() {
                self.load_diff_task = Some(
                    self.update_uncommitted_diff_for_buffer(
                        &project,
                        self.buffer.read(cx).all_buffers(),
                        cx,
                    )
                    .shared(),
                );
            }
        }

        cx.notify();
    }

    pub fn git_blame_inline_enabled(&self) -> bool {
        self.git_blame_inline_enabled
    }

    pub fn blame(&self) -> Option<&Entity<GitBlame>> {
        self.blame.as_ref()
    }

    pub fn active_git_blame_entry(&self, cx: &mut App) -> Option<BlameEntry> {
        if !self.show_git_blame_inline
            || self.newest_selection_head_on_empty_line(cx)
            || !self.has_blame_entries(cx)
        {
            return None;
        }

        let blame = self.blame.as_ref()?;
        let snapshot = self.display_snapshot(cx);
        let cursor = self.selections.newest::<Point>(&snapshot).head();
        let (buffer, point) = snapshot.buffer_snapshot().point_to_buffer_point(cursor)?;

        blame
            .update(cx, |blame, cx| {
                blame
                    .blame_for_rows(
                        &[RowInfo {
                            buffer_id: Some(buffer.remote_id()),
                            buffer_row: Some(point.row),
                            ..Default::default()
                        }],
                        cx,
                    )
                    .next()
            })
            .flatten()
            .map(|(_, entry)| entry)
    }

    pub fn show_git_blame_gutter(&self) -> bool {
        self.show_git_blame_gutter
    }

    pub fn expand_selected_diff_hunks(&mut self, cx: &mut Context<Self>) {
        let ranges: Vec<_> = self
            .selections
            .disjoint_anchors()
            .iter()
            .map(|s| s.range())
            .collect();
        self.buffer
            .update(cx, |buffer, cx| buffer.expand_diff_hunks(ranges, cx))
    }

    pub fn toggle_git_blame(
        &mut self,
        _: &::git::Blame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_git_blame_gutter = !self.show_git_blame_gutter;

        if self.show_git_blame_gutter && !self.has_blame_entries(cx) {
            self.start_git_blame(true, window, cx);
        }

        cx.notify();
    }

    pub fn toggle_git_blame_inline(
        &mut self,
        _: &ToggleGitBlameInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_git_blame_inline_internal(true, window, cx);
        cx.notify();
    }

    /// Hides the inline blame popover element, in case it's already visible, or
    /// interrupts the task meant to show it, in case the task is running.
    ///
    /// When `ignore_timeout` is set to `true`, the popover is hidden
    /// immediately, otherwise it'll be hidden after a short delay.
    ///
    /// Returns `true` if the popover was visible and was hidden, `false`
    /// otherwise.
    pub fn hide_blame_popover(&mut self, ignore_timeout: bool, cx: &mut Context<Self>) -> bool {
        self.inline_blame_popover_show_task.take();

        if let Some(state) = &mut self.inline_blame_popover {
            if ignore_timeout {
                self.inline_blame_popover.take();
                cx.notify();
            } else {
                state.hide_task = Some(cx.spawn(async move |editor, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(100))
                        .await;

                    editor
                        .update(cx, |editor, cx| {
                            editor.inline_blame_popover.take();
                            cx.notify();
                        })
                        .ok();
                }));
            }

            true
        } else {
            false
        }
    }

    pub fn git_restore(&mut self, _: &Restore, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        let selections = self
            .selections
            .all(&self.display_snapshot(cx))
            .into_iter()
            .map(|s| s.range())
            .collect();
        self.restore_hunks_in_ranges(selections, window, cx);
    }

    pub fn status_for_buffer_id(&self, buffer_id: BufferId, cx: &App) -> Option<FileStatus> {
        if let Some(status) = self
            .addons
            .iter()
            .find_map(|(_, addon)| addon.override_status_for_buffer_id(buffer_id, cx))
        {
            return Some(status);
        }
        self.project
            .as_ref()?
            .read(cx)
            .status_for_buffer_id(buffer_id, cx)
    }

    pub fn go_to_hunk_before_or_after_position(
        &mut self,
        snapshot: &EditorSnapshot,
        position: Point,
        direction: Direction,
        wrap_around: bool,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let row = if direction == Direction::Next {
            self.hunk_after_position(snapshot, position, wrap_around)
                .map(|hunk| hunk.row_range.start)
        } else {
            self.hunk_before_position(snapshot, position, wrap_around)
        };

        if let Some(row) = row {
            let destination = Point::new(row.0, 0);
            let autoscroll = Autoscroll::center();

            self.unfold_ranges(&[destination..destination], false, false, cx);
            self.change_selections(SelectionEffects::scroll(autoscroll), window, cx, |s| {
                s.select_ranges([destination..destination]);
            });
        }
    }

    pub fn set_expand_all_diff_hunks(&mut self, cx: &mut App) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.set_all_diff_hunks_expanded(cx);
        });
    }

    pub fn expand_all_diff_hunks(
        &mut self,
        _: &ExpandAllDiffHunks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.expand_diff_hunks(vec![Anchor::Min..Anchor::Max], cx)
        });
    }

    pub(super) fn blame_hover(
        &mut self,
        _: &BlameHover,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let just_started = self.blame.is_none();
        if just_started {
            self.start_git_blame(true, window, cx);
        }
        let Some(blame) = self.blame.as_ref() else {
            return;
        };

        if just_started && !blame.read(cx).has_generated_entries() {
            let subscription = cx.observe_in(blame, window, |editor, blame, window, cx| {
                if blame.read(cx).has_generated_entries() {
                    editor.pending_blame_hover_observation.take();
                    editor.show_blame_hover_popover(window, cx);
                }
            });
            self.pending_blame_hover_observation = Some(subscription);
            return;
        }

        self.show_blame_hover_popover(window, cx);
    }

    fn show_blame_hover_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(window, cx);
        let cursor = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();
        let Some((buffer, point)) = snapshot.buffer_snapshot().point_to_buffer_point(cursor) else {
            return;
        };

        let Some(blame) = self.blame.as_ref() else {
            return;
        };

        let row_info = RowInfo {
            buffer_id: Some(buffer.remote_id()),
            buffer_row: Some(point.row),
            ..Default::default()
        };
        let Some((buffer, blame_entry)) = blame
            .update(cx, |blame, cx| blame.blame_for_rows(&[row_info], cx).next())
            .flatten()
        else {
            return;
        };

        let anchor = self.selections.newest_anchor().head();
        let position = self.to_pixel_point(anchor, &snapshot, window, cx);
        if let (Some(position), Some(last_bounds)) = (position, self.last_bounds) {
            self.show_blame_popover(
                buffer,
                &blame_entry,
                position + last_bounds.origin,
                true,
                cx,
            );
        };
    }

    pub(super) fn restore_file(
        &mut self,
        _: &::git::RestoreFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        let mut buffer_ids = HashSet::default();
        let snapshot = self.buffer().read(cx).snapshot(cx);
        for selection in self
            .selections
            .all::<MultiBufferOffset>(&self.display_snapshot(cx))
        {
            buffer_ids.extend(snapshot.buffer_ids_for_range(selection.range()))
        }

        let ranges = buffer_ids
            .into_iter()
            .flat_map(|buffer_id| snapshot.range_for_buffer(buffer_id))
            .collect::<Vec<_>>();

        self.restore_hunks_in_ranges(ranges, window, cx);
    }

    /// Restores the diff hunks in the editor's selections and moves the cursor
    /// to the next diff hunk. Wraps around to the beginning of the buffer if
    /// not all diff hunks are expanded.
    pub(super) fn restore_and_next(
        &mut self,
        _: &::git::RestoreAndNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        let selections = self
            .selections
            .all(&self.display_snapshot(cx))
            .into_iter()
            .map(|selection| selection.range())
            .collect();

        self.restore_hunks_in_ranges(selections, window, cx);

        let all_diff_hunks_expanded = self.buffer().read(cx).all_diff_hunks_expanded();
        let wrap_around = !all_diff_hunks_expanded;
        let snapshot = self.snapshot(window, cx);
        let position = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();

        self.go_to_hunk_before_or_after_position(
            &snapshot,
            position,
            Direction::Next,
            wrap_around,
            window,
            cx,
        );
    }

    pub fn restore_diff_hunks(&mut self, hunks: Vec<ResolvedDiffHunks>, cx: &mut Context<Self>) {
        let mut revert_changes = Vec::new();
        for hunks in hunks {
            let Some(buffer) = hunks.buffer else {
                continue;
            };
            let diff_snapshot = hunks.diff.read(cx).snapshot(cx);
            let changes = hunks
                .hunks
                .into_iter()
                .filter_map(|hunk| {
                    if hunk.diff_base_byte_range == (0..0)
                        && hunk.buffer_range.start.is_min()
                        && hunk.buffer_range.end.is_max()
                    {
                        return None;
                    }
                    let original_text = diff_snapshot
                        .base_text()
                        .as_rope()
                        .slice(hunk.diff_base_byte_range.start..hunk.diff_base_byte_range.end);
                    Some((hunk.buffer_range, original_text))
                })
                .collect::<Vec<_>>();
            if !changes.is_empty() {
                revert_changes.push((buffer, changes));
            }
        }

        for (buffer, changes) in revert_changes {
            buffer.update(cx, |buffer, cx| {
                buffer.edit(
                    changes
                        .into_iter()
                        .map(|(range, text)| (range, text.to_string())),
                    None,
                    cx,
                );
            });
        }
    }

    pub(super) fn go_to_next_hunk(
        &mut self,
        _: &GoToHunk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(window, cx);
        let selection = self.selections.newest::<Point>(&self.display_snapshot(cx));
        self.go_to_hunk_before_or_after_position(
            &snapshot,
            selection.head(),
            Direction::Next,
            true,
            window,
            cx,
        );
    }

    pub(super) fn collapse_all_diff_hunks(
        &mut self,
        _: &CollapseAllDiffHunks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.collapse_diff_hunks(vec![Anchor::Min..Anchor::Max], cx)
        });
    }

    pub fn toggle_all_diff_hunks(
        &mut self,
        _: &ToggleAllDiffHunks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_any_expanded_diff_hunks(cx) {
            self.collapse_all_diff_hunks(&CollapseAllDiffHunks, window, cx);
        } else {
            self.expand_all_diff_hunks(&ExpandAllDiffHunks, window, cx);
        }
    }

    pub(super) fn toggle_selected_diff_hunks(
        &mut self,
        _: &ToggleSelectedDiffHunks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ranges: Vec<_> = self
            .selections
            .disjoint_anchors()
            .iter()
            .map(|s| s.range())
            .collect();
        self.toggle_diff_hunks_in_ranges(ranges, cx);
    }

    pub(super) fn copy_permalink_to_line(
        &mut self,
        _: &CopyPermalinkToLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let permalink_task = self.get_permalink_to_line(cx);
        let workspace = self.workspace();

        cx.spawn_in(window, async move |_, cx| match permalink_task.await {
            Ok(permalink) => {
                cx.update(|_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(permalink.to_string()));
                })
                .ok();
            }
            Err(err) => {
                let message = format!("Failed to copy permalink to line: {err}");

                anyhow::Result::<()>::Err(err).log_err();

                if let Some(workspace) = workspace {
                    workspace
                        .update_in(cx, |workspace, _, cx| {
                            struct CopyPermalinkToLine;

                            workspace.show_toast(
                                Toast::new(
                                    NotificationId::unique::<CopyPermalinkToLine>(),
                                    message,
                                ),
                                cx,
                            )
                        })
                        .ok();
                }
            }
        })
        .detach();
    }

    pub(super) fn open_permalink_to_line(
        &mut self,
        _: &OpenPermalinkToLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let permalink_task = self.get_permalink_to_line(cx);
        let workspace = self.workspace();

        cx.spawn_in(window, async move |_, cx| match permalink_task.await {
            Ok(permalink) => {
                cx.update(|_, cx| {
                    cx.open_url(permalink.as_ref());
                })
                .ok();
            }
            Err(err) => {
                let message = format!("Failed to open permalink to line: {err}");

                anyhow::Result::<()>::Err(err).log_err();

                if let Some(workspace) = workspace {
                    workspace.update(cx, |workspace, cx| {
                        struct OpenPermalinkToLine;

                        workspace.show_toast(
                            Toast::new(NotificationId::unique::<OpenPermalinkToLine>(), message),
                            cx,
                        )
                    });
                }
            }
        })
        .detach();
    }

    pub(super) fn toggle_staged_selected_diff_hunks(
        &mut self,
        _: &::git::ToggleStaged,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ranges: Vec<_> = self
            .selections
            .disjoint_anchors()
            .iter()
            .map(|s| s.range())
            .collect();
        let task = self.save_buffers_for_ranges_if_needed(&ranges, cx);
        cx.spawn_in(window, async move |this, cx| {
            task.await?;
            this.update_in(cx, |this, window, cx| {
                let snapshot = this.buffer.read(cx).snapshot(cx);
                let hunks = this.diff_hunks_in_ranges(&ranges, &snapshot).collect();
                this.apply_toggle(hunks, window, cx);
            })
        })
        .detach_and_log_err(cx);
    }

    pub(super) fn stage_and_next(
        &mut self,
        _: &::git::StageAndNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.do_stage_or_unstage_and_next(true, window, cx);
    }

    pub(super) fn unstage_and_next(
        &mut self,
        _: &::git::UnstageAndNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.do_stage_or_unstage_and_next(false, window, cx);
    }

    pub fn apply_toggle(
        &mut self,
        hunks: Vec<MultiBufferDiffHunk>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut hunks = self.resolve_diff_hunks(hunks, cx);
        if self.diff_hunk_delegate.is_none() {
            hunks.retain(|hunks| hunks.diff.read(cx).is_stageable());
        }
        if hunks.is_empty() {
            return;
        }
        let delegate = self.diff_hunk_delegate();
        delegate.toggle(hunks, self, window, cx);
    }

    pub fn apply_stage_or_unstage(
        &mut self,
        stage: bool,
        hunks: Vec<MultiBufferDiffHunk>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut hunks = self.resolve_diff_hunks(hunks, cx);
        if self.diff_hunk_delegate.is_none() {
            hunks.retain(|hunks| hunks.diff.read(cx).is_stageable());
        }
        if hunks.is_empty() {
            return;
        }
        let delegate = self.diff_hunk_delegate();
        delegate.stage_or_unstage(stage, hunks, self, window, cx);
    }

    pub fn apply_restore(
        &mut self,
        hunks: Vec<MultiBufferDiffHunk>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut hunks = self.resolve_diff_hunks(hunks, cx);
        if self.diff_hunk_delegate.is_none() {
            hunks.retain(|hunks| hunks.diff.read(cx).is_stageable());
        }
        if hunks.is_empty() {
            return;
        }
        let delegate = self.diff_hunk_delegate();
        delegate.restore(hunks, self, window, cx);
    }

    pub(super) fn clear_expanded_diff_hunks(&mut self, cx: &mut Context<Self>) -> bool {
        self.buffer.update(cx, |buffer, cx| {
            let ranges = vec![Anchor::Min..Anchor::Max];
            if !buffer.all_diff_hunks_expanded()
                && buffer.has_expanded_diff_hunks_in_ranges(&ranges, cx)
            {
                buffer.collapse_diff_hunks(ranges, cx);
                true
            } else {
                false
            }
        })
    }

    pub(super) fn has_any_expanded_diff_hunks(&self, cx: &App) -> bool {
        if self.buffer.read(cx).all_diff_hunks_expanded() {
            return true;
        }
        let ranges = vec![Anchor::Min..Anchor::Max];
        self.buffer
            .read(cx)
            .has_expanded_diff_hunks_in_ranges(&ranges, cx)
    }

    pub(super) fn toggle_single_diff_hunk(&mut self, range: Range<Anchor>, cx: &mut Context<Self>) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.toggle_single_diff_hunk(range, cx);
        })
    }

    pub(super) fn apply_all_diff_hunks(
        &mut self,
        _: &ApplyAllDiffHunks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        let buffers = self.buffer.read(cx).all_buffers();
        for branch_buffer in buffers {
            branch_buffer.update(cx, |branch_buffer, cx| {
                branch_buffer.merge_into_base(Vec::new(), cx);
            });
        }

        if let Some(project) = self.project.clone() {
            self.save(
                SaveOptions {
                    format: true,
                    force_format: false,
                    autosave: false,
                },
                project,
                window,
                cx,
            )
            .detach_and_log_err(cx);
        }
    }

    pub(super) fn apply_selected_diff_hunks(
        &mut self,
        _: &ApplyDiffHunk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        let snapshot = self.snapshot(window, cx);
        let hunks = snapshot.hunks_for_ranges(
            self.selections
                .all(&snapshot.display_snapshot)
                .into_iter()
                .map(|selection| selection.range()),
        );
        let mut ranges_by_buffer = HashMap::default();
        self.transact(window, cx, |editor, _window, cx| {
            for hunk in hunks {
                if let Some(buffer) = editor.buffer.read(cx).buffer(hunk.buffer_id) {
                    ranges_by_buffer
                        .entry(buffer.clone())
                        .or_insert_with(Vec::new)
                        .push(hunk.buffer_range.to_offset(buffer.read(cx)));
                }
            }

            for (buffer, ranges) in ranges_by_buffer {
                buffer.update(cx, |buffer, cx| {
                    buffer.merge_into_base(ranges, cx);
                });
            }
        });

        if let Some(project) = self.project.clone() {
            self.save(
                SaveOptions {
                    format: true,
                    force_format: false,
                    autosave: false,
                },
                project,
                window,
                cx,
            )
            .detach_and_log_err(cx);
        }
    }

    pub(super) fn open_git_blame_commit(
        &mut self,
        _: &OpenGitBlameCommit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_git_blame_commit_internal(window, cx);
    }

    pub(super) fn toggle_git_blame_inline_internal(
        &mut self,
        user_triggered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.git_blame_inline_enabled {
            self.git_blame_inline_enabled = false;
            self.show_git_blame_inline = false;
            self.show_git_blame_inline_delay_task.take();
        } else {
            self.git_blame_inline_enabled = true;
            self.start_git_blame_inline(user_triggered, window, cx);
        }

        cx.notify();
    }

    pub(super) fn start_git_blame_inline(
        &mut self,
        user_triggered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_git_blame(user_triggered, window, cx);

        if ProjectSettings::get_global(cx)
            .git
            .inline_blame_delay()
            .is_some()
        {
            self.start_inline_blame_timer(window, cx);
        } else {
            self.show_git_blame_inline = true
        }
    }

    pub(super) fn render_git_blame_gutter(&self, cx: &App) -> bool {
        !self.mode().is_minimap() && self.show_git_blame_gutter && self.has_blame_entries(cx)
    }

    pub(super) fn render_git_blame_inline(&self, window: &Window, cx: &App) -> bool {
        ProjectSettings::get_global(cx).git.inline_blame.location
            == project::project_settings::InlineBlameLocation::Inline
            && self.show_git_blame_inline
            && (self.focus_handle.is_focused(window) || self.inline_blame_popover.is_some())
            && !self.newest_selection_head_on_empty_line(cx)
            && self.has_blame_entries(cx)
    }

    pub(super) fn start_inline_blame_timer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(delay) = ProjectSettings::get_global(cx).git.inline_blame_delay() {
            self.show_git_blame_inline = false;

            self.show_git_blame_inline_delay_task =
                Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(delay).await;

                    this.update(cx, |this, cx| {
                        this.show_git_blame_inline = true;
                        cx.notify();
                    })
                    .log_err();
                }));
        }
    }

    pub(super) fn show_blame_popover(
        &mut self,
        buffer: BufferId,
        blame_entry: &BlameEntry,
        position: gpui::Point<Pixels>,
        ignore_timeout: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &mut self.inline_blame_popover {
            state.hide_task.take();
        } else {
            let blame_popover_delay = EditorSettings::get_global(cx).hover_popover_delay.0;
            let blame_entry = blame_entry.clone();
            let show_task = cx.spawn(async move |editor, cx| {
                if !ignore_timeout {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(blame_popover_delay))
                        .await;
                }
                editor
                    .update(cx, |editor, cx| {
                        editor.inline_blame_popover_show_task.take();
                        let Some(blame) = editor.blame.as_ref() else {
                            return;
                        };
                        let blame = blame.read(cx);
                        let details = blame.details_for_entry(buffer, &blame_entry);
                        let markdown = cx.new(|cx| {
                            Markdown::new(
                                details
                                    .as_ref()
                                    .map(|message| message.message.clone())
                                    .unwrap_or_default(),
                                None,
                                None,
                                cx,
                            )
                        });
                        editor.inline_blame_popover = Some(InlineBlamePopover {
                            position,
                            hide_task: None,
                            popover_bounds: None,
                            popover_state: InlineBlamePopoverState {
                                scroll_handle: ScrollHandle::new(),
                                commit_message: details,
                                markdown,
                            },
                            keyboard_grace: ignore_timeout,
                        });
                        cx.notify();
                    })
                    .ok();
            });
            self.inline_blame_popover_show_task = Some(show_task);
        }
    }

    pub(super) fn go_to_prev_hunk(
        &mut self,
        _: &GoToPreviousHunk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(window, cx);
        let selection = self.selections.newest::<Point>(&snapshot.display_snapshot);
        self.go_to_hunk_before_or_after_position(
            &snapshot,
            selection.head(),
            Direction::Prev,
            true,
            window,
            cx,
        );
    }

    pub fn stage_or_unstage_diff_hunks(
        &mut self,
        stage: bool,
        ranges: Vec<Range<Anchor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.save_buffers_for_ranges_if_needed(&ranges, cx);
        cx.spawn_in(window, async move |this, cx| {
            task.await?;
            this.update_in(cx, |this, window, cx| {
                let snapshot = this.buffer.read(cx).snapshot(cx);
                let hunks = this.diff_hunks_in_ranges(&ranges, &snapshot).collect();
                this.apply_stage_or_unstage(stage, hunks, window, cx);
            })
        })
        .detach_and_log_err(cx);
    }

    pub fn restore_diff_hunks_in_ranges(
        &mut self,
        ranges: Vec<Range<Anchor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let hunks = self.diff_hunks_in_ranges(&ranges, &snapshot).collect();
        self.apply_restore(hunks, window, cx);
    }

    fn toggle_diff_hunks_in_ranges(
        &mut self,
        ranges: Vec<Range<Anchor>>,
        cx: &mut Context<Editor>,
    ) {
        self.buffer.update(cx, |buffer, cx| {
            let expand = !buffer.has_expanded_diff_hunks_in_ranges(&ranges, cx);
            buffer.expand_or_collapse_diff_hunks(ranges, expand, cx);
        })
    }

    pub fn set_blame(
        &mut self,
        blame: Entity<GitBlame>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.blame_subscription = Some(cx.observe_in(&blame, window, |_, _, _, cx| cx.notify()));
        self.blame = Some(blame);
        self.show_git_blame_gutter = true;
        cx.notify();
    }

    fn start_git_blame(
        &mut self,
        user_triggered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .blame
            .as_ref()
            .is_some_and(|blame| blame.read(cx).is_static())
        {
            return;
        }
        if let Some(project) = self.project() {
            if let Some(buffer) = self.buffer().read(cx).as_singleton()
                && buffer.read(cx).file().is_none_or(|file| {
                    matches!(file.disk_state(), language::DiskState::Historic { .. })
                })
            {
                return;
            }

            let focused = self.focus_handle(cx).contains_focused(window, cx);

            let project = project.clone();
            let blame = cx
                .new(|cx| GitBlame::new(self.buffer.clone(), project, user_triggered, focused, cx));
            self.blame_subscription =
                Some(cx.observe_in(&blame, window, |_, _, _, cx| cx.notify()));
            self.blame = Some(blame);
        }
    }

    fn restore_hunks_in_ranges(
        &mut self,
        ranges: Vec<Range<Point>>,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let hunks = self.snapshot(window, cx).hunks_for_ranges(ranges);
        self.apply_restore(hunks, window, cx);
    }

    fn save_buffers_for_ranges_if_needed(
        &mut self,
        ranges: &[Range<Anchor>],
        cx: &mut Context<Editor>,
    ) -> Task<Result<()>> {
        let multibuffer = self.buffer.read(cx);
        let snapshot = multibuffer.read(cx);
        let buffer_ids: HashSet<_> = ranges
            .iter()
            .flat_map(|range| snapshot.buffer_ids_for_range(range.clone()))
            .collect();
        drop(snapshot);

        let mut buffers = HashSet::default();
        for buffer_id in buffer_ids {
            if let Some(buffer_entity) = multibuffer.buffer(buffer_id) {
                let buffer = buffer_entity.read(cx);
                if buffer.file().is_some_and(|file| file.disk_state().exists()) && buffer.is_dirty()
                {
                    buffers.insert(buffer_entity);
                }
            }
        }

        if let Some(project) = &self.project {
            project.update(cx, |project, cx| project.save_buffers(buffers, cx))
        } else {
            Task::ready(Ok(()))
        }
    }

    fn do_stage_or_unstage_and_next(
        &mut self,
        stage: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ranges = self.selections.disjoint_anchor_ranges().collect::<Vec<_>>();

        if ranges.iter().any(|range| range.start != range.end) {
            self.stage_or_unstage_diff_hunks(stage, ranges, window, cx);
            return;
        }

        self.stage_or_unstage_diff_hunks(stage, ranges, window, cx);

        let all_diff_hunks_expanded = self.buffer().read(cx).all_diff_hunks_expanded();
        let wrap_around = !all_diff_hunks_expanded;
        let snapshot = self.snapshot(window, cx);
        let position = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();

        self.go_to_hunk_before_or_after_position(
            &snapshot,
            position,
            Direction::Next,
            wrap_around,
            window,
            cx,
        );
    }

    fn open_git_blame_commit_internal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let (blame_entry, repo) = self.blame_entry_at_cursor(window, cx)?;
        let renderer = cx.global::<GlobalBlameRenderer>().0.clone();
        let workspace = self.workspace()?.downgrade();
        renderer.open_blame_commit(blame_entry, repo, workspace, window, cx);
        None
    }

    fn blame_entry_at_cursor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(BlameEntry, Entity<Repository>)> {
        let blame = self.blame.clone()?;
        let snapshot = self.snapshot(window, cx);
        let cursor = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();
        let (buffer, point) = snapshot.buffer_snapshot().point_to_buffer_point(cursor)?;
        let (_, blame_entry) = blame
            .update(cx, |blame, cx| {
                blame
                    .blame_for_rows(
                        &[RowInfo {
                            buffer_id: Some(buffer.remote_id()),
                            buffer_row: Some(point.row),
                            ..Default::default()
                        }],
                        cx,
                    )
                    .next()
            })
            .flatten()?;
        let repository = blame.read(cx).repository(cx, buffer.remote_id())?;
        Some((blame_entry, repository))
    }

    pub(crate) fn blame_revision_target(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(RepoPath, Oid, Entity<Repository>)> {
        let (blame_entry, repository) = self.blame_entry_at_cursor(window, cx)?;
        let highlighted_sha = self
            .blame
            .as_ref()
            .and_then(|blame| blame.read(cx).highlighted_sha());
        let (revision, path) = blame_entry.revision_target(highlighted_sha)?;
        Some((path, revision, repository))
    }

    pub(crate) fn blame_previous_revision_target(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(RepoPath, Oid, Entity<Repository>)> {
        let (blame_entry, repository) = self.blame_entry_at_cursor(window, cx)?;
        let (revision, path) = blame_entry.previous_revision_target()?;
        Some((path, revision, repository))
    }

    pub(super) fn blame_revision(
        &mut self,
        _: &BlameRevision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((path, revision, repository)) = self.blame_revision_target(window, cx) else {
            return;
        };
        self.open_blame_revision(path, revision, repository, window, cx);
    }

    pub(super) fn blame_previous_revision(
        &mut self,
        _: &BlamePreviousRevision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((path, revision, repository)) = self.blame_previous_revision_target(window, cx)
        else {
            return;
        };
        self.open_blame_revision(path, revision, repository, window, cx);
    }

    fn open_blame_revision(
        &mut self,
        path: RepoPath,
        revision: Oid,
        repository: Entity<Repository>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace() else {
            return;
        };
        let renderer = cx.global::<GlobalBlameRenderer>().0.clone();
        renderer.open_blame_revision(
            path,
            revision,
            repository,
            workspace.downgrade(),
            window,
            cx,
        );
    }

    fn has_blame_entries(&self, cx: &App) -> bool {
        self.blame()
            .is_some_and(|blame| blame.read(cx).has_generated_entries())
    }

    fn newest_selection_head_on_empty_line(&self, cx: &App) -> bool {
        let cursor_anchor = self.selections.newest_anchor().head();

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let buffer_row = MultiBufferRow(cursor_anchor.to_point(&snapshot).row);

        snapshot.line_len(buffer_row) == 0
    }
    fn hunk_after_position(
        &mut self,
        snapshot: &EditorSnapshot,
        position: Point,
        wrap_around: bool,
    ) -> Option<MultiBufferDiffHunk> {
        let result = snapshot
            .buffer_snapshot()
            .diff_hunks_in_range(position..snapshot.buffer_snapshot().max_point())
            .find(|hunk| hunk.row_range.start.0 > position.row);

        if wrap_around {
            result.or_else(|| {
                snapshot
                    .buffer_snapshot()
                    .diff_hunks_in_range(Point::zero()..position)
                    .find(|hunk| hunk.row_range.end.0 < position.row)
            })
        } else {
            result
        }
    }

    fn hunk_before_position(
        &mut self,
        snapshot: &EditorSnapshot,
        position: Point,
        wrap_around: bool,
    ) -> Option<MultiBufferRow> {
        let result = snapshot.buffer_snapshot().diff_hunk_before(position);

        if wrap_around {
            result.or_else(|| snapshot.buffer_snapshot().diff_hunk_before(Point::MAX))
        } else {
            result
        }
    }

    fn get_permalink_to_line(&self, cx: &mut Context<Self>) -> Task<Result<url::Url>> {
        let buffer_and_selection = maybe!({
            let selection = self.selections.newest::<Point>(&self.display_snapshot(cx));
            let selection_range = selection.range();

            let multi_buffer = self.buffer().read(cx);
            let multi_buffer_snapshot = multi_buffer.snapshot(cx);
            let buffer_ranges = multi_buffer_snapshot
                .range_to_buffer_ranges(selection_range.start..selection_range.end);

            let (buffer_snapshot, range, _) = if selection.reversed {
                buffer_ranges.first()
            } else {
                buffer_ranges.last()
            }?;

            let buffer_range = range.to_point(buffer_snapshot);
            let buffer = multi_buffer.buffer(buffer_snapshot.remote_id())?;

            let Some(buffer_diff) = multi_buffer.diff_for(buffer_snapshot.remote_id()) else {
                return Some((buffer, buffer_range.start.row..buffer_range.end.row));
            };

            let buffer_diff_snapshot = buffer_diff.read(cx).snapshot(cx);
            let start = buffer_diff_snapshot
                .buffer_point_to_base_text_point(buffer_range.start, &buffer_snapshot);
            let end = buffer_diff_snapshot
                .buffer_point_to_base_text_point(buffer_range.end, &buffer_snapshot);

            Some((buffer, start.row..end.row))
        });

        let Some((buffer, selection)) = buffer_and_selection else {
            return Task::ready(Err(anyhow!("failed to determine buffer and selection")));
        };

        let Some(project) = self.project() else {
            return Task::ready(Err(anyhow!("editor does not have project")));
        };

        project.update(cx, |project, cx| {
            project.get_permalink_to_line(&buffer, selection, cx)
        })
    }
}

impl EditorSnapshot {
    pub(super) fn display_diff_hunks_for_rows<'a>(
        &'a self,
        display_rows: Range<DisplayRow>,
        folded_buffers: &'a HashSet<BufferId>,
    ) -> impl 'a + Iterator<Item = DisplayDiffHunk> {
        let buffer_start = DisplayPoint::new(display_rows.start, 0).to_point(self);
        let buffer_end = DisplayPoint::new(display_rows.end, 0).to_point(self);

        self.buffer_snapshot()
            .diff_hunks_in_range(buffer_start..buffer_end)
            .filter_map(|hunk| {
                if folded_buffers.contains(&hunk.buffer_id)
                    || (hunk.row_range.is_empty() && self.buffer.all_diff_hunks_expanded())
                {
                    return None;
                }

                let hunk_start_point = Point::new(hunk.row_range.start.0, 0);
                let hunk_end_point = if hunk.row_range.end > hunk.row_range.start {
                    let last_row = MultiBufferRow(hunk.row_range.end.0 - 1);
                    let line_len = self.buffer_snapshot().line_len(last_row);
                    Point::new(last_row.0, line_len)
                } else {
                    Point::new(hunk.row_range.end.0, 0)
                };

                let hunk_display_start = self.point_to_display_point(hunk_start_point, Bias::Left);
                let hunk_display_end = self.point_to_display_point(hunk_end_point, Bias::Right);

                let display_hunk = if hunk_display_start.column() != 0 {
                    DisplayDiffHunk::Folded {
                        display_row: hunk_display_start.row(),
                    }
                } else {
                    let mut end_row = hunk_display_end.row();
                    if hunk.row_range.end > hunk.row_range.start || hunk_display_end.column() > 0 {
                        end_row.0 += 1;
                    }
                    let is_created_file = hunk.is_created_file();
                    let multi_buffer_range = hunk.multi_buffer_range.clone();

                    DisplayDiffHunk::Unfolded {
                        status: hunk.status(),
                        diff_base_byte_range: hunk.diff_base_byte_range.start.0
                            ..hunk.diff_base_byte_range.end.0,
                        word_diffs: hunk.word_diffs,
                        display_row_range: hunk_display_start.row()..end_row,
                        multi_buffer_range,
                        is_created_file,
                    }
                };

                Some(display_hunk)
            })
    }

    fn hunks_for_ranges(
        &self,
        ranges: impl IntoIterator<Item = Range<Point>>,
    ) -> Vec<MultiBufferDiffHunk> {
        let mut hunks = Vec::new();
        let mut processed_buffer_rows: HashMap<BufferId, HashSet<Range<text::Anchor>>> =
            HashMap::default();
        for query_range in ranges {
            let query_rows =
                MultiBufferRow(query_range.start.row)..MultiBufferRow(query_range.end.row + 1);
            for hunk in self.buffer_snapshot().diff_hunks_in_range(
                Point::new(query_rows.start.0, 0)..Point::new(query_rows.end.0, 0),
            ) {
                // Include deleted hunks that are adjacent to the query range, because
                // otherwise they would be missed.
                let mut intersects_range = hunk.row_range.overlaps(&query_rows);
                if hunk.status().is_deleted() {
                    intersects_range |= hunk.row_range.start == query_rows.end;
                    intersects_range |= hunk.row_range.end == query_rows.start;
                }
                if intersects_range {
                    if !processed_buffer_rows
                        .entry(hunk.buffer_id)
                        .or_default()
                        .insert(hunk.buffer_range.start..hunk.buffer_range.end)
                    {
                        continue;
                    }
                    hunks.push(hunk);
                }
            }
        }

        hunks
    }
}

pub fn set_blame_renderer(renderer: impl BlameRenderer + 'static, cx: &mut App) {
    cx.set_global(GlobalBlameRenderer(Arc::new(renderer)));
}

pub fn render_diff_hunk_controls(
    row: u32,
    status: &DiffHunkStatus,
    hunk_range: Range<Anchor>,
    is_created_file: bool,
    line_height: Pixels,
    editor: &Entity<Editor>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let stageable = hunk_range
        .start
        .buffer_id()
        .and_then(|buffer_id| editor.read(cx).buffer().read(cx).diff_for(buffer_id))
        .is_some_and(|diff| diff.read(cx).is_stageable());
    let show_stage_restore = stageable
        && ProjectSettings::get_global(cx)
            .git
            .show_stage_restore_buttons;

    h_flex()
        .h(line_height)
        .mr_1()
        .gap_1()
        .px_0p5()
        .pb_1()
        .border_x_1()
        .border_b_1()
        .border_color(cx.theme().colors().border_variant)
        .rounded_b_lg()
        .bg(cx.theme().colors().editor_background)
        .gap_1()
        .block_mouse_except_scroll()
        .shadow_md()
        .when(show_stage_restore, |el| {
            el.child(if status.has_secondary_hunk() {
                Button::new(("stage", row as u64), "Stage")
                    .alpha(if status.is_pending() { 0.66 } else { 1.0 })
                    .tooltip({
                        let focus_handle = editor.focus_handle(cx);
                        move |_window, cx| {
                            Tooltip::for_action_in(
                                "Stage Hunk",
                                &::git::ToggleStaged,
                                &focus_handle,
                                cx,
                            )
                        }
                    })
                    .on_click({
                        let editor = editor.clone();
                        move |_event, window, cx| {
                            editor.update(cx, |editor, cx| {
                                editor.stage_or_unstage_diff_hunks(
                                    true,
                                    vec![hunk_range.start..hunk_range.start],
                                    window,
                                    cx,
                                );
                            });
                        }
                    })
            } else {
                Button::new(("unstage", row as u64), "Unstage")
                    .alpha(if status.is_pending() { 0.66 } else { 1.0 })
                    .tooltip({
                        let focus_handle = editor.focus_handle(cx);
                        move |_window, cx| {
                            Tooltip::for_action_in(
                                "Unstage Hunk",
                                &::git::ToggleStaged,
                                &focus_handle,
                                cx,
                            )
                        }
                    })
                    .on_click({
                        let editor = editor.clone();
                        move |_event, window, cx| {
                            editor.update(cx, |editor, cx| {
                                editor.stage_or_unstage_diff_hunks(
                                    false,
                                    vec![hunk_range.start..hunk_range.start],
                                    window,
                                    cx,
                                );
                            });
                        }
                    })
            })
        })
        .when(show_stage_restore, |el| {
            el.child(
                Button::new(("restore", row as u64), "Restore")
                    .tooltip({
                        let focus_handle = editor.focus_handle(cx);
                        move |_window, cx| {
                            Tooltip::for_action_in(
                                "Restore Hunk",
                                &::git::Restore,
                                &focus_handle,
                                cx,
                            )
                        }
                    })
                    .on_click({
                        let editor = editor.clone();
                        move |_event, window, cx| {
                            editor.update(cx, |editor, cx| {
                                let snapshot = editor.snapshot(window, cx);
                                let point = hunk_range.start.to_point(&snapshot.buffer_snapshot());
                                editor.restore_hunks_in_ranges(vec![point..point], window, cx);
                            });
                        }
                    })
                    .disabled(is_created_file),
            )
        })
        .when(
            !editor.read(cx).buffer().read(cx).all_diff_hunks_expanded(),
            |el| {
                el.child(
                    IconButton::new(("next-hunk", row as u64), IconName::ArrowDown)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        // .disabled(!has_multiple_hunks)
                        .tooltip({
                            let focus_handle = editor.focus_handle(cx);
                            move |_window, cx| {
                                Tooltip::for_action_in("Next Hunk", &GoToHunk, &focus_handle, cx)
                            }
                        })
                        .on_click({
                            let editor = editor.clone();
                            move |_event, window, cx| {
                                editor.update(cx, |editor, cx| {
                                    let snapshot = editor.snapshot(window, cx);
                                    let position =
                                        hunk_range.end.to_point(&snapshot.buffer_snapshot());
                                    editor.go_to_hunk_before_or_after_position(
                                        &snapshot,
                                        position,
                                        Direction::Next,
                                        true,
                                        window,
                                        cx,
                                    );
                                    editor.expand_selected_diff_hunks(cx);
                                });
                            }
                        }),
                )
                .child(
                    IconButton::new(("prev-hunk", row as u64), IconName::ArrowUp)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        // .disabled(!has_multiple_hunks)
                        .tooltip({
                            let focus_handle = editor.focus_handle(cx);
                            move |_window, cx| {
                                Tooltip::for_action_in(
                                    "Previous Hunk",
                                    &GoToPreviousHunk,
                                    &focus_handle,
                                    cx,
                                )
                            }
                        })
                        .on_click({
                            let editor = editor.clone();
                            move |_event, window, cx| {
                                editor.update(cx, |editor, cx| {
                                    let snapshot = editor.snapshot(window, cx);
                                    let point =
                                        hunk_range.start.to_point(&snapshot.buffer_snapshot());
                                    editor.go_to_hunk_before_or_after_position(
                                        &snapshot,
                                        point,
                                        Direction::Prev,
                                        true,
                                        window,
                                        cx,
                                    );
                                    editor.expand_selected_diff_hunks(cx);
                                });
                            }
                        }),
                )
            },
        )
        .into_any_element()
}

impl Editor {
    pub(super) fn update_uncommitted_diff_for_buffer(
        &mut self,
        project: &Entity<Project>,
        buffers: impl IntoIterator<Item = Entity<Buffer>>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let mut tasks = Vec::new();
        project.update(cx, |project, cx| {
            let git_store = project.git_store().clone();
            git_store.update(cx, |git_store, cx| {
                for buffer in buffers {
                    if project::File::from_dyn(buffer.read(cx).file()).is_some() {
                        tasks.push(git_store.open_display_diff(buffer, cx));
                    }
                }
            });
        });

        let editor = cx.entity();
        let buffer = self.buffer.clone();
        cx.spawn(async move |_, cx| {
            let diffs = future::join_all(tasks).await;
            if editor.read_with(cx, |editor, _cx| editor.diff_hunk_delegate.is_some()) {
                return;
            }

            buffer.update(cx, |buffer, cx| {
                for diff in diffs.into_iter().flatten() {
                    buffer.add_diff(diff, cx);
                }
            });
        })
    }
}
