use crate::{
    EXCLUDE_PLACEHOLDER, INCLUDE_PLACEHOLDER, REPLACE_PLACEHOLDER, ReplaceAll, ReplaceNext,
    SearchOption, SearchOptions, SearchSource, ToggleCaseSensitive, ToggleIncludeIgnored,
    ToggleRegex, ToggleReplace, ToggleWholeWord,
    project_search::{
        ProjectSearch, ProjectSearchSettings, ProjectSearchView, QuerySeed, SearchMode,
        ToggleFilters, contains_uppercase, query_seed, split_glob_patterns,
    },
    search_bar::render_text_input,
};
use anyhow::Result;
use collections::{HashMap, HashSet};
use editor::{
    Anchor, Editor, EditorEvent, EditorSettings, HighlightKey, SelectionEffects,
    actions::{Backtab, SelectAll, Tab},
    scroll::Autoscroll,
};
use fs::Fs;
use gpui::{
    Action, AnyElement, App, AsyncWindowContext, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, HighlightStyle, IntoElement, KeyContext, ParentElement, Pixels, Render,
    ScrollStrategy, SharedString, StrikethroughStyle, Styled, StyledText, Subscription, Task,
    UniformListScrollHandle, WeakEntity, Window, actions, div, px, uniform_list,
};
use language::{Buffer, BufferId, BufferSnapshot, Point};
use menu::{Cancel, Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use project::{Project, ProjectPath, search::SearchQuery};
use settings::{DockSide, IntoGpui, RegisterSetting, Settings, SettingsStore};
use std::{ops::Range, sync::Arc, time::Duration};
use text::ToPoint as _;
use ui::{Icon, IconButton, IconButtonShape, IconName, Label, ListItem, Tooltip, prelude::*};
use util::paths::{PathMatcher, PathStyle};
use workspace::{
    DeploySearch, HideStatusItem, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    searchable::{SearchToken, SearchableItem as _},
};

actions!(
    search_panel,
    [
        /// Toggles focus on the search panel.
        ToggleFocus,
        /// Opens the current search results in an editor tab.
        OpenInEditor,
        /// Clears the search query and results.
        ClearResults,
        /// Runs the search again.
        RefreshResults,
        /// Collapses every file in the results.
        CollapseAllEntries,
        /// Expands every file in the results.
        ExpandAllEntries,
        /// Collapses the selected file.
        CollapseSelectedEntry,
        /// Expands the selected file.
        ExpandSelectedEntry,
    ]
);

const SEARCH_PANEL_KEY: &str = "SearchPanel";
const SEARCH_ON_TYPE_DEBOUNCE: Duration = Duration::from_millis(250);
const LEFT_CONTEXT_CHARS: usize = 24;
const RIGHT_CONTEXT_CHARS: usize = 160;
const INDENT_STEP: Pixels = px(22.);

#[derive(Debug, Clone, Copy, PartialEq, RegisterSetting)]
pub struct SearchPanelSettings {
    pub button: bool,
    pub default_width: Pixels,
    pub dock: DockSide,
}

impl Settings for SearchPanelSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let panel = content.search_panel.as_ref().unwrap();
        Self {
            button: panel.button.unwrap(),
            default_width: panel.default_width.unwrap().into_gpui(),
            dock: panel.dock.unwrap(),
        }
    }
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<SearchPanel>(window, cx);
        });
    })
    .detach();
}

pub struct SearchPanel {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    fs: Arc<dyn Fs>,
    focus_handle: FocusHandle,
    search: Entity<ProjectSearch>,
    highlighted_editors: Vec<WeakEntity<Editor>>,
    highlight_generation: usize,
    query_editor: Entity<Editor>,
    replacement_editor: Entity<Editor>,
    included_files_editor: Entity<Editor>,
    excluded_files_editor: Entity<Editor>,
    search_options: SearchOptions,
    replace_enabled: bool,
    filters_enabled: bool,
    error: Option<SharedString>,
    files: Vec<FileResult>,
    entries: Vec<Entry>,
    collapsed_files: HashSet<BufferId>,
    selected_entry: Option<Entry>,
    scroll_handle: UniformListScrollHandle,
    debounced_search: Option<Task<()>>,
    pending_replace_all: bool,
    _subscriptions: Vec<Subscription>,
}

struct FileResult {
    buffer_id: BufferId,
    project_path: ProjectPath,
    file_name: SharedString,
    directory: SharedString,
    matches: Vec<Range<Anchor>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    File(usize),
    Match { file: usize, index: usize },
}

struct MatchLine {
    text: String,
    match_range: Range<usize>,
}

impl SearchPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            Self::new(workspace, window, cx)
        })
    }

    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let weak_workspace = cx.entity().downgrade();
        cx.new(|cx| {
            let search =
                cx.new(|cx| ProjectSearch::new(project.clone(), weak_workspace.clone(), cx));
            let query_editor = single_line_editor("Search", window, cx);
            query_editor.update(cx, |editor, _| {
                editor.set_use_autoclose(false);
                editor.set_use_selection_highlight(false);
            });
            let replacement_editor = single_line_editor(REPLACE_PLACEHOLDER, window, cx);
            let included_files_editor = single_line_editor(INCLUDE_PLACEHOLDER, window, cx);
            let excluded_files_editor = single_line_editor(EXCLUDE_PLACEHOLDER, window, cx);

            let subscriptions = vec![
                cx.observe_in(&search, window, |this: &mut Self, _, window, cx| {
                    this.search_changed(window, cx)
                }),
                cx.subscribe(&query_editor, |this, _, event: &EditorEvent, cx| {
                    this.query_edited(event, cx)
                }),
                cx.subscribe(&replacement_editor, |_, _, event: &EditorEvent, cx| {
                    if matches!(event, EditorEvent::Edited { .. }) {
                        cx.notify();
                    }
                }),
                cx.subscribe(
                    &included_files_editor,
                    |this, _, event: &EditorEvent, cx| this.filter_edited(event, cx),
                ),
                cx.subscribe(
                    &excluded_files_editor,
                    |this, _, event: &EditorEvent, cx| this.filter_edited(event, cx),
                ),
                cx.observe_global::<SettingsStore>(|_, cx| cx.notify()),
            ];

            Self {
                workspace: weak_workspace,
                fs: project.read(cx).fs().clone(),
                project,
                focus_handle: cx.focus_handle(),
                search,
                highlighted_editors: Vec::new(),
                highlight_generation: 0,
                query_editor,
                replacement_editor,
                included_files_editor,
                excluded_files_editor,
                search_options: SearchOptions::from_settings(
                    &EditorSettings::get_global(cx).search,
                ),
                replace_enabled: false,
                filters_enabled: false,
                error: None,
                files: Vec::new(),
                entries: Vec::new(),
                collapsed_files: HashSet::default(),
                selected_entry: None,
                scroll_handle: UniformListScrollHandle::new(),
                debounced_search: None,
                pending_replace_all: false,
                _subscriptions: subscriptions,
            }
        })
    }

    pub fn deploy(
        workspace: &mut Workspace,
        action: &DeploySearch,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let query_seed = query_seed(workspace, window, cx);
        let Some(panel) = workspace.focus_panel::<SearchPanel>(window, cx) else {
            return;
        };
        panel.update(cx, |panel, cx| {
            panel.apply_deploy(action, query_seed, window, cx);
        });
    }

    fn apply_deploy(
        &mut self,
        action: &DeploySearch,
        query_seed: Option<QuerySeed>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_enabled |= action.replace_enabled;
        for (option, enabled) in [
            (SearchOptions::REGEX, action.regex),
            (SearchOptions::CASE_SENSITIVE, action.case_sensitive),
            (SearchOptions::WHOLE_WORD, action.whole_word),
            (SearchOptions::INCLUDE_IGNORED, action.include_ignored),
        ] {
            if let Some(enabled) = enabled {
                self.search_options.set(option, enabled);
            }
        }
        if let Some(included_files) = action.included_files.as_deref() {
            self.included_files_editor
                .update(cx, |editor, cx| editor.set_text(included_files, window, cx));
            self.filters_enabled = true;
        }
        if let Some(excluded_files) = action.excluded_files.as_deref() {
            self.excluded_files_editor
                .update(cx, |editor, cx| editor.set_text(excluded_files, window, cx));
            self.filters_enabled = true;
        }
        let query = action
            .query
            .clone()
            .filter(|query| !query.is_empty())
            .or_else(|| {
                query_seed
                    .map(|seed| seed.into_query(self.search_options.contains(SearchOptions::REGEX)))
            });
        if let Some(query) = query {
            self.query_editor
                .update(cx, |editor, cx| editor.set_text(query, window, cx));
            self.search(SearchMode::OnType, cx);
        }
        self.focus_query_editor(window, cx);
        cx.notify();
    }

    fn focus_query_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query_editor.update(cx, |editor, cx| {
            editor.select_all(&SelectAll, window, cx);
        });
        window.focus(&self.query_editor.focus_handle(cx), cx);
    }

    fn query_text(&self, cx: &App) -> String {
        self.query_editor.read(cx).text(cx)
    }

    fn query_edited(&mut self, event: &EditorEvent, cx: &mut Context<Self>) {
        if !matches!(event, EditorEvent::Edited { .. }) {
            return;
        }
        let settings = EditorSettings::get_global(cx);
        if settings.use_smartcase_search {
            let query = self.query_text(cx);
            if !query.is_empty()
                && self.search_options.contains(SearchOptions::CASE_SENSITIVE)
                    != contains_uppercase(&query)
            {
                self.search_options.toggle(SearchOptions::CASE_SENSITIVE);
            }
        }
        if settings.search.search_on_type {
            self.schedule_search(cx);
        }
        cx.notify();
    }

    fn filter_edited(&mut self, event: &EditorEvent, cx: &mut Context<Self>) {
        if matches!(event, EditorEvent::Edited { .. })
            && self.filters_enabled
            && EditorSettings::get_global(cx).search.search_on_type
            && !self.query_text(cx).is_empty()
        {
            self.schedule_search(cx);
        }
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.debounced_search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(SEARCH_ON_TYPE_DEBOUNCE)
                .await;
            this.update(cx, |this, cx| this.search(SearchMode::OnType, cx))
                .ok();
        }));
    }

    fn search(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        self.debounced_search = None;
        self.clear_match_highlights(cx);
        if self.query_text(cx).is_empty() {
            self.error = None;
            self.pending_replace_all = false;
            self.search.update(cx, |search, cx| search.clear(cx));
            cx.notify();
            return;
        }
        match self.build_query(cx) {
            Ok(query) => {
                self.error = None;
                self.search
                    .update(cx, |search, cx| search.search(query, mode, false, cx));
            }
            Err(error) => {
                self.error = Some(error.to_string().into());
                self.pending_replace_all = false;
            }
        }
        cx.notify();
    }

    fn build_query(&self, cx: &App) -> Result<SearchQuery> {
        let project = self.project.read(cx);
        let path_style = project.path_style(cx);
        let (included_files, excluded_files) = if self.filters_enabled {
            (
                parse_path_matches(&self.included_files_editor.read(cx).text(cx), path_style)?,
                parse_path_matches(&self.excluded_files_editor.read(cx).text(cx), path_style)?,
            )
        } else {
            (PathMatcher::default(), PathMatcher::default())
        };
        let match_full_paths = project.visible_worktrees(cx).count() > 1;
        self.search_options.build_query(
            self.query_text(cx),
            included_files,
            excluded_files,
            match_full_paths,
            None,
        )
    }

    fn search_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rebuild_results(cx);
        if self.pending_replace_all && self.search.read(cx).pending_search.is_none() {
            self.pending_replace_all = false;
            self.replace_all(&ReplaceAll, window, cx);
        }
        cx.notify();
    }

    fn rebuild_results(&mut self, cx: &App) {
        let search = self.search.read(cx);
        let snapshot = search.excerpts.read(cx).snapshot(cx);
        let project = self.project.read(cx);
        let path_style = project.path_style(cx);
        let show_worktree_root = project.visible_worktrees(cx).count() > 1;
        let mut files: Vec<FileResult> = Vec::new();
        let mut file_indices: HashMap<BufferId, usize> = HashMap::default();
        for range in &search.match_ranges {
            let Some(buffer_id) = range.start.buffer_id() else {
                continue;
            };
            let file_index = match file_indices.get(&buffer_id) {
                Some(index) => *index,
                None => {
                    let Some(file) = snapshot
                        .buffer_for_id(buffer_id)
                        .and_then(|buffer| project::File::from_dyn(buffer.file()))
                    else {
                        continue;
                    };
                    let mut directory = file
                        .path
                        .parent()
                        .map(|parent| parent.display(path_style).into_owned())
                        .unwrap_or_default();
                    if show_worktree_root {
                        let root_name = file.worktree.read(cx).root_name_str();
                        directory = if directory.is_empty() {
                            root_name.to_string()
                        } else {
                            format!("{root_name}{}{directory}", path_style.primary_separator())
                        };
                    }
                    files.push(FileResult {
                        buffer_id,
                        project_path: ProjectPath {
                            worktree_id: file.worktree_id(cx),
                            path: file.path.clone(),
                        },
                        file_name: file.path.file_name().unwrap_or_default().to_string().into(),
                        directory: directory.into(),
                        matches: Vec::new(),
                    });
                    let index = files.len() - 1;
                    file_indices.insert(buffer_id, index);
                    index
                }
            };
            if let Some(file) = files.get_mut(file_index) {
                file.matches.push(range.clone());
            }
        }
        self.files = files;
        self.rebuild_entries();
    }

    fn rebuild_entries(&mut self) {
        let mut entries = Vec::new();
        for (file_index, file) in self.files.iter().enumerate() {
            entries.push(Entry::File(file_index));
            if !self.collapsed_files.contains(&file.buffer_id) {
                entries.extend((0..file.matches.len()).map(|index| Entry::Match {
                    file: file_index,
                    index,
                }));
            }
        }
        self.entries = entries;
        if let Some(selected) = self.selected_entry
            && !self.entries.contains(&selected)
        {
            self.selected_entry = None;
        }
    }

    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected_entry?;
        self.entries.iter().position(|entry| *entry == selected)
    }

    fn match_count(&self) -> usize {
        self.files.iter().map(|file| file.matches.len()).sum()
    }

    fn select_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(index).copied() else {
            return;
        };
        if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }
        self.selected_entry = Some(entry);
        self.scroll_handle
            .scroll_to_item(index, ScrollStrategy::Nearest);
        if let Entry::Match { file, index } = entry {
            self.open_match(file, index, false, true, window, cx);
        }
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            cx.propagate();
            return;
        }
        let index = match self.selected_index() {
            Some(index) if self.focus_handle.is_focused(window) => {
                (index + 1).min(self.entries.len() - 1)
            }
            Some(index) => index,
            None => 0,
        };
        self.select_index(index, window, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, window: &mut Window, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            cx.propagate();
            return;
        }
        let index = match self.selected_index() {
            Some(index) if self.focus_handle.is_focused(window) => index.saturating_sub(1),
            Some(index) => index,
            None => 0,
        };
        self.select_index(index, window, cx);
    }

    fn select_first(&mut self, _: &SelectFirst, window: &mut Window, cx: &mut Context<Self>) {
        if self.entries.is_empty() || !self.focus_handle.is_focused(window) {
            cx.propagate();
            return;
        }
        self.select_index(0, window, cx);
    }

    fn select_last(&mut self, _: &SelectLast, window: &mut Window, cx: &mut Context<Self>) {
        if self.entries.is_empty() || !self.focus_handle.is_focused(window) {
            cx.propagate();
            return;
        }
        self.select_index(self.entries.len() - 1, window, cx);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_handle.is_focused(window) {
            match self.selected_entry {
                Some(Entry::File(file)) => self.toggle_file(file, cx),
                Some(Entry::Match { file, index }) => {
                    self.open_match(file, index, true, false, window, cx)
                }
                None => cx.propagate(),
            }
        } else {
            self.search(SearchMode::Manual, cx);
        }
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_handle.is_focused(window) {
            self.focus_query_editor(window, cx);
        } else {
            cx.propagate();
        }
    }

    fn tab(&mut self, _: &Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_field(1, window, cx);
    }

    fn backtab(&mut self, _: &Backtab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_field(-1, window, cx);
    }

    fn cycle_field(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
        let mut handles = vec![self.query_editor.focus_handle(cx)];
        if self.replace_enabled {
            handles.push(self.replacement_editor.focus_handle(cx));
        }
        if self.filters_enabled {
            handles.push(self.included_files_editor.focus_handle(cx));
            handles.push(self.excluded_files_editor.focus_handle(cx));
        }
        if !self.entries.is_empty() {
            handles.push(self.focus_handle.clone());
        }
        let Some(current) = handles.iter().position(|handle| handle.is_focused(window)) else {
            cx.propagate();
            return;
        };
        let next = (current as isize + direction).rem_euclid(handles.len() as isize) as usize;
        if let Some(handle) = handles.get(next) {
            window.focus(handle, cx);
        }
        if next == handles.len() - 1 && !self.entries.is_empty() && self.selected_entry.is_none() {
            self.selected_entry = self.entries.first().copied();
        }
        cx.notify();
    }

    fn toggle_file(&mut self, file_index: usize, cx: &mut Context<Self>) {
        let Some(file) = self.files.get(file_index) else {
            return;
        };
        if !self.collapsed_files.remove(&file.buffer_id) {
            self.collapsed_files.insert(file.buffer_id);
        }
        self.selected_entry = Some(Entry::File(file_index));
        self.rebuild_entries();
        cx.notify();
    }

    fn set_file_collapsed(&mut self, file_index: usize, collapsed: bool, cx: &mut Context<Self>) {
        let Some(file) = self.files.get(file_index) else {
            return;
        };
        if collapsed {
            self.collapsed_files.insert(file.buffer_id);
        } else {
            self.collapsed_files.remove(&file.buffer_id);
        }
        self.selected_entry = Some(Entry::File(file_index));
        self.rebuild_entries();
        cx.notify();
    }

    fn collapse_selected_entry(
        &mut self,
        _: &CollapseSelectedEntry,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected_entry {
            Some(Entry::File(file)) => self.set_file_collapsed(file, true, cx),
            Some(Entry::Match { file, .. }) => {
                self.selected_entry = Some(Entry::File(file));
                cx.notify();
            }
            None => {}
        }
    }

    fn expand_selected_entry(
        &mut self,
        _: &ExpandSelectedEntry,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Entry::File(file)) = self.selected_entry {
            self.set_file_collapsed(file, false, cx);
        }
    }

    fn collapse_all_entries(
        &mut self,
        _: &CollapseAllEntries,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.collapsed_files
            .extend(self.files.iter().map(|file| file.buffer_id));
        self.rebuild_entries();
        cx.notify();
    }

    fn expand_all_entries(&mut self, _: &ExpandAllEntries, _: &mut Window, cx: &mut Context<Self>) {
        self.collapsed_files.clear();
        self.rebuild_entries();
        cx.notify();
    }

    fn clear_results(&mut self, _: &ClearResults, window: &mut Window, cx: &mut Context<Self>) {
        self.query_editor
            .update(cx, |editor, cx| editor.set_text("", window, cx));
        self.collapsed_files.clear();
        self.selected_entry = None;
        self.search(SearchMode::Manual, cx);
        self.focus_query_editor(window, cx);
    }

    fn refresh_results(&mut self, _: &RefreshResults, _: &mut Window, cx: &mut Context<Self>) {
        self.search(SearchMode::Refresh, cx);
    }

    pub(crate) fn toggle_search_option(&mut self, option: SearchOptions, cx: &mut Context<Self>) {
        self.search_options.toggle(option);
        if !self.query_text(cx).is_empty() {
            self.search(SearchMode::Manual, cx);
        }
        cx.notify();
    }

    fn toggle_replace(&mut self, _: &ToggleReplace, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_enabled = !self.replace_enabled;
        if self.replace_enabled {
            window.focus(&self.replacement_editor.focus_handle(cx), cx);
        } else if self.replacement_editor.focus_handle(cx).is_focused(window) {
            self.focus_query_editor(window, cx);
        }
        cx.notify();
    }

    fn toggle_filters(&mut self, _: &ToggleFilters, window: &mut Window, cx: &mut Context<Self>) {
        self.filters_enabled = !self.filters_enabled;
        if !self.filters_enabled
            && (self
                .included_files_editor
                .focus_handle(cx)
                .is_focused(window)
                || self
                    .excluded_files_editor
                    .focus_handle(cx)
                    .is_focused(window))
        {
            self.focus_query_editor(window, cx);
        }
        if !self.query_text(cx).is_empty() {
            self.search(SearchMode::Manual, cx);
        }
        cx.notify();
    }

    fn replace_all(&mut self, _: &ReplaceAll, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.read(cx).pending_search.is_some() {
            self.pending_replace_all = true;
            return;
        }
        let query_text = self.query_text(cx);
        if self.search.read(cx).last_search_query_text() != Some(query_text.as_str()) {
            self.search(SearchMode::Manual, cx);
            self.pending_replace_all = self.search.read(cx).pending_search.is_some();
            return;
        }
        let match_ranges = self.search.read(cx).match_ranges.clone();
        self.replace_matches(match_ranges, window, cx);
    }

    fn replace_next(&mut self, _: &ReplaceNext, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected_entry {
            Some(Entry::Match { file, index }) => self.replace_match(file, index, window, cx),
            Some(Entry::File(file)) => self.replace_file(file, window, cx),
            None => {}
        }
    }

    fn replace_match(
        &mut self,
        file_index: usize,
        match_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(range) = self
            .files
            .get(file_index)
            .and_then(|file| file.matches.get(match_index))
            .cloned()
        else {
            return;
        };
        self.replace_matches(vec![range], window, cx);
    }

    fn replace_file(&mut self, file_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ranges) = self.files.get(file_index).map(|file| file.matches.clone()) else {
            return;
        };
        self.replace_matches(ranges, window, cx);
    }

    /// Applies the replacement to the underlying buffers, then saves the ones
    /// that were clean before the edit so the change lands on disk like it does
    /// in VS Code. Buffers that already had unsaved edits stay unsaved.
    fn replace_matches(
        &mut self,
        ranges: Vec<Range<Anchor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ranges.is_empty() {
            return;
        }
        let Some(query) = self.search.read(cx).active_query.clone() else {
            return;
        };
        let query = query.with_replacement(self.replacement_editor.read(cx).text(cx));
        let excerpts = self.search.read(cx).excerpts.clone();
        let clean_buffers: HashSet<Entity<Buffer>> = ranges
            .iter()
            .filter_map(|range| excerpts.read(cx).buffer(range.start.buffer_id()?))
            .filter(|buffer| !buffer.read(cx).is_dirty())
            .collect();
        let replace_editor = cx.new(|cx| Editor::for_multibuffer(excerpts, None, window, cx));
        replace_editor.update(cx, |editor, cx| {
            editor.replace_all(
                &mut ranges.iter(),
                &query,
                SearchToken::default(),
                window,
                cx,
            );
        });
        let save = self
            .project
            .update(cx, |project, cx| project.save_buffers(clean_buffers, cx));
        cx.spawn(async move |this, cx| {
            save.await?;
            this.update(cx, |this, cx| this.search(SearchMode::Refresh, cx))
        })
        .detach_and_log_err(cx);
    }

    fn open_in_editor(&mut self, _: &OpenInEditor, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let search = self.search.update(cx, |search, cx| search.clone(cx));
        let settings = ProjectSearchSettings {
            search_options: self.search_options,
            filters_enabled: self.filters_enabled,
        };
        let included_files = self.included_files_editor.read(cx).text(cx);
        let excluded_files = self.excluded_files_editor.read(cx).text(cx);
        workspace.update(cx, |workspace, cx| {
            ProjectSearchView::open_in_pane(
                workspace,
                search,
                settings,
                included_files,
                excluded_files,
                window,
                cx,
            );
        });
    }

    fn clear_match_highlights(&mut self, cx: &mut Context<Self>) {
        self.highlight_generation += 1;
        for editor in self.highlighted_editors.drain(..) {
            editor
                .update(cx, |editor, cx| {
                    editor.clear_background_highlights(HighlightKey::SearchPanelMatches, cx);
                })
                .ok();
        }
    }

    fn open_match(
        &mut self,
        file_index: usize,
        match_index: usize,
        focus_editor: bool,
        allow_preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(file) = self.files.get(file_index) else {
            return;
        };
        let snapshot = self.search.read(cx).excerpts.read(cx).snapshot(cx);
        let Some(buffer) = snapshot.buffer_for_id(file.buffer_id) else {
            return;
        };
        let match_ranges = file
            .matches
            .iter()
            .map(|range| range.start.text_anchor_in(buffer)..range.end.text_anchor_in(buffer))
            .collect::<Vec<_>>();
        if match_index >= match_ranges.len() {
            return;
        }
        let highlight_generation = self.highlight_generation;
        let project_path = file.project_path.clone();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        self.selected_entry = Some(Entry::Match {
            file: file_index,
            index: match_index,
        });
        let open = workspace.update(cx, |workspace, cx| {
            workspace.open_path_preview(
                project_path,
                None,
                focus_editor,
                allow_preview,
                true,
                window,
                cx,
            )
        });
        cx.spawn_in(window, async move |this, cx| {
            let item = open.await?;
            let highlight_is_current = this.read_with(cx, |this, _| {
                this.highlight_generation == highlight_generation
            })?;
            let highlighted_editor = cx.update(|window, cx| {
                let editor = item.act_as::<Editor>(cx)?;
                let highlighted = editor.update(cx, |editor, cx| {
                    let snapshot = editor.buffer().read(cx).snapshot(cx);
                    let editor_match_ranges = match_ranges
                        .iter()
                        .map(|range| snapshot.anchor_range_in_buffer(range.clone()))
                        .collect::<Option<Vec<_>>>()?;
                    let active_range = editor_match_ranges.get(match_index)?.clone();
                    if highlight_is_current {
                        editor.highlight_background(
                            HighlightKey::SearchPanelMatches,
                            &editor_match_ranges,
                            move |index, theme| {
                                if *index == match_index {
                                    theme.colors().search_active_match_background
                                } else {
                                    theme.colors().search_match_background
                                }
                            },
                            cx,
                        );
                    }
                    editor.unfold_ranges(std::slice::from_ref(&active_range), false, true, cx);
                    let selection_range = editor.range_for_match(&active_range);
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()).from_search(true),
                        window,
                        cx,
                        |selections| selections.select_anchor_ranges([selection_range]),
                    );
                    Some(highlight_is_current)
                })?;
                highlighted.then_some(editor)
            })?;
            if let Some(editor) = highlighted_editor {
                this.update(cx, |this, _| {
                    if !this
                        .highlighted_editors
                        .iter()
                        .any(|highlighted| highlighted.entity_id() == editor.entity_id())
                    {
                        this.highlighted_editors.push(editor.downgrade());
                    }
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
        cx.notify();
    }

    fn dispatch_context(&self, window: &Window) -> KeyContext {
        let mut context = KeyContext::new_with_defaults();
        context.add(SEARCH_PANEL_KEY);
        if self.focus_handle.is_focused(window) {
            context.add("menu");
        }
        context
    }

    fn render_input(
        &self,
        editor: &Entity<Editor>,
        trailing: Option<AnyElement>,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors();
        let focus_handle = editor.focus_handle(cx);
        let border_color = if focus_handle.is_focused(window) {
            colors.border_focused
        } else {
            colors.border_variant
        };
        h_flex()
            .id(SharedString::from(format!(
                "search-panel-input-{}",
                editor.entity_id()
            )))
            .flex_1()
            .min_w_0()
            .h_7()
            .pl_2()
            .pr_0p5()
            .gap_1()
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .bg(colors.editor_background)
            .cursor_text()
            .on_click(move |_, window, cx| window.focus(&focus_handle, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(render_text_input(editor, None, cx)),
            )
            .children(trailing)
    }

    fn render_inputs(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let query_focus_handle = self.query_editor.focus_handle(cx);
        let option_buttons = h_flex()
            .flex_none()
            .child(SearchOption::CaseSensitive.as_button(
                self.search_options,
                SearchSource::Panel(cx),
                query_focus_handle.clone(),
            ))
            .child(SearchOption::WholeWord.as_button(
                self.search_options,
                SearchSource::Panel(cx),
                query_focus_handle.clone(),
            ))
            .child(SearchOption::Regex.as_button(
                self.search_options,
                SearchSource::Panel(cx),
                query_focus_handle.clone(),
            ))
            .into_any_element();
        let replace_toggle = div()
            .id("search-panel-toggle-replace")
            .flex_none()
            .w_4()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(colors.ghost_element_hover))
            .when(self.replace_enabled, |this| {
                this.bg(colors.ghost_element_selected)
            })
            .tooltip({
                let focus_handle = query_focus_handle.clone();
                move |_, cx| {
                    Tooltip::for_action_in("Toggle Replace", &ToggleReplace, &focus_handle, cx)
                }
            })
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_replace(&ToggleReplace, window, cx);
            }))
            .child(
                Icon::new(if self.replace_enabled {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::Small)
                .color(Color::Muted),
            );
        let replace_all_button = IconButton::new("search-panel-replace-all", IconName::ReplaceAll)
            .shape(IconButtonShape::Square)
            .disabled(self.files.is_empty())
            .tooltip({
                let focus_handle = query_focus_handle.clone();
                move |_, cx| Tooltip::for_action_in("Replace All", &ReplaceAll, &focus_handle, cx)
            })
            .on_click(cx.listener(|this, _, window, cx| {
                this.replace_all(&ReplaceAll, window, cx);
            }))
            .into_any_element();
        let include_ignored_button = SearchOption::IncludeIgnored
            .as_button(
                self.search_options,
                SearchSource::Panel(cx),
                query_focus_handle,
            )
            .into_any_element();
        let filter_label = |text: &'static str| {
            Label::new(text)
                .size(LabelSize::XSmall)
                .color(Color::Muted)
                .mt_1()
        };

        v_flex()
            .px_2()
            .pt_2()
            .pb_1()
            .gap_1()
            .child(
                h_flex()
                    .gap_1()
                    .items_stretch()
                    .child(replace_toggle)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(self.render_input(
                                &self.query_editor,
                                Some(option_buttons),
                                window,
                                cx,
                            ))
                            .when(self.replace_enabled, |this| {
                                this.child(self.render_input(
                                    &self.replacement_editor,
                                    Some(replace_all_button),
                                    window,
                                    cx,
                                ))
                            }),
                    ),
            )
            .when(self.filters_enabled, |this| {
                this.child(
                    v_flex()
                        .pl_5()
                        .child(filter_label("files to include"))
                        .child(self.render_input(
                            &self.included_files_editor,
                            Some(include_ignored_button),
                            window,
                            cx,
                        ))
                        .child(filter_label("files to exclude"))
                        .child(self.render_input(&self.excluded_files_editor, None, window, cx)),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    Label::new(error)
                        .size(LabelSize::Small)
                        .color(Color::Error)
                        .ml_5(),
                )
            })
    }

    fn render_toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let search = self.search.read(cx);
        let state = search.search_state();
        let has_query = !self.query_text(cx).is_empty();
        let match_count = self.match_count();
        let summary: Option<SharedString> = if search.pending_search.is_some() && match_count == 0 {
            Some("Searching…".into())
        } else if match_count > 0 {
            let files = self.files.len();
            let mut summary = format!(
                "{match_count} result{} in {files} file{}",
                if match_count == 1 { "" } else { "s" },
                if files == 1 { "" } else { "s" }
            );
            if state.limit_reached() {
                summary.push_str(", limit reached");
            }
            Some(summary.into())
        } else if has_query && state.no_results_so_far() {
            Some("No results found".into())
        } else {
            None
        };
        let query_focus_handle = self.query_editor.focus_handle(cx);
        let action_button = |id: &'static str,
                             icon: IconName,
                             label: &'static str,
                             action: &'static dyn Action,
                             enabled: bool| {
            let focus_handle = query_focus_handle.clone();
            IconButton::new(id, icon)
                .shape(IconButtonShape::Square)
                .icon_size(IconSize::Small)
                .disabled(!enabled)
                .tooltip(move |_, cx| Tooltip::for_action_in(label, action, &focus_handle, cx))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        };
        let has_results = !self.files.is_empty();
        h_flex()
            .h_7()
            .pl_2()
            .pr_1()
            .gap_2()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                div().min_w_0().overflow_hidden().child(
                    Label::new(summary.unwrap_or_default())
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .single_line()
                        .truncate(),
                ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .child(action_button(
                        "search-panel-refresh",
                        IconName::ArrowCircle,
                        "Refresh",
                        &RefreshResults,
                        has_query,
                    ))
                    .child(action_button(
                        "search-panel-clear",
                        IconName::Eraser,
                        "Clear Search Results",
                        &ClearResults,
                        has_query || has_results,
                    ))
                    .child(action_button(
                        "search-panel-collapse-all",
                        IconName::ListCollapse,
                        "Collapse All",
                        &CollapseAllEntries,
                        has_results,
                    ))
                    .child(action_button(
                        "search-panel-open-in-editor",
                        IconName::FileDiff,
                        "Open in Editor",
                        &OpenInEditor,
                        has_results,
                    ))
                    .child(
                        IconButton::new("search-panel-toggle-filters", IconName::Ellipsis)
                            .shape(IconButtonShape::Square)
                            .icon_size(IconSize::Small)
                            .toggle_state(self.filters_enabled)
                            .tooltip({
                                let focus_handle = query_focus_handle.clone();
                                move |_, cx| {
                                    Tooltip::for_action_in(
                                        "Toggle Search Details",
                                        &ToggleFilters,
                                        &focus_handle,
                                        cx,
                                    )
                                }
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_filters(&ToggleFilters, window, cx);
                            })),
                    ),
            )
    }

    fn render_row(
        &self,
        id: impl Into<ElementId>,
        entry: Entry,
        item: ListItem,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let is_selected = self.selected_entry == Some(entry);
        let colors = cx.theme().colors();
        let focused = is_selected && self.focus_handle.is_focused(window);
        div()
            .id(id)
            .text_ui(cx)
            .border_l_2()
            .border_color(if focused {
                colors.panel_focused_border
            } else {
                gpui::transparent_black()
            })
            .child(item.toggle_state(is_selected))
            .into_any_element()
    }

    fn render_file(
        &self,
        file_index: usize,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let file = self.files.get(file_index)?;
        let is_expanded = !self.collapsed_files.contains(&file.buffer_id);
        let icon =
            file_icons::FileIcons::get_icon(file.project_path.path.as_std_path(), cx).map(|icon| {
                Icon::from_path(icon)
                    .color(Color::Muted)
                    .size(IconSize::Small)
            });
        let count = file.matches.len();
        let colors = cx.theme().colors();
        let replace_button = self.replace_enabled.then(|| {
            IconButton::new(
                ("search-panel-replace-file", file_index),
                IconName::ReplaceAll,
            )
            .shape(IconButtonShape::Square)
            .icon_size(IconSize::Small)
            .tooltip(Tooltip::text("Replace All in File"))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.replace_file(file_index, window, cx);
            }))
        });
        let item = ListItem::new(("search-panel-file", file_index))
            .toggle(is_expanded)
            .on_toggle(cx.listener(move |this, _, _, cx| this.toggle_file(file_index, cx)))
            .when_some(icon, |this, icon| this.start_slot(icon))
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .items_baseline()
                    .child(Label::new(file.file_name.clone()).single_line())
                    .when(!file.directory.is_empty(), |this| {
                        this.child(
                            div().min_w_0().overflow_hidden().child(
                                Label::new(file.directory.clone())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .single_line()
                                    .truncate(),
                            ),
                        )
                    }),
            )
            .end_slot(
                h_flex().flex_none().gap_1().children(replace_button).child(
                    div()
                        .px_1p5()
                        .rounded_full()
                        .bg(colors.element_background)
                        .child(
                            Label::new(count.to_string())
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        ),
                ),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_file(file_index, cx)));
        Some(self.render_row(
            ("search-panel-file-row", file_index),
            Entry::File(file_index),
            item,
            window,
            cx,
        ))
    }

    fn render_match(
        &self,
        file_index: usize,
        match_index: usize,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let file = self.files.get(file_index)?;
        let range = file.matches.get(match_index)?;
        let search = self.search.read(cx);
        let snapshot = search.excerpts.read(cx).snapshot(cx);
        let buffer = snapshot.buffer_for_id(file.buffer_id)?;
        let line = match_line(buffer, range);
        let colors = cx.theme().colors();
        let replacement = self
            .replace_enabled
            .then(|| self.replacement_editor.read(cx).text(cx))
            .filter(|replacement| !replacement.is_empty())
            .and_then(|replacement| {
                let query = search.active_query.clone()?.with_replacement(replacement);
                query
                    .replacement_for(&line.text, line.match_range.clone())
                    .map(|replacement| replacement.into_owned())
            });
        let mut text_style = window.text_style();
        text_style.font_size = ui::TextSize::Small.rems(cx).into();
        let (text, highlights) = match replacement {
            Some(replacement) => {
                let (before, rest) = line.text.split_at(line.match_range.start);
                let (matched, after) = rest.split_at(line.match_range.end - line.match_range.start);
                let matched_range = before.len()..before.len() + matched.len();
                let replacement_range = matched_range.end..matched_range.end + replacement.len();
                (
                    format!("{before}{matched}{replacement}{after}"),
                    vec![
                        (
                            matched_range,
                            HighlightStyle {
                                background_color: Some(colors.version_control_deleted.opacity(0.2)),
                                strikethrough: Some(StrikethroughStyle {
                                    thickness: px(1.),
                                    color: Some(colors.text_muted),
                                }),
                                ..HighlightStyle::default()
                            },
                        ),
                        (
                            replacement_range,
                            HighlightStyle {
                                background_color: Some(colors.version_control_added.opacity(0.2)),
                                ..HighlightStyle::default()
                            },
                        ),
                    ],
                )
            }
            None => (
                line.text,
                vec![(
                    line.match_range,
                    HighlightStyle {
                        background_color: Some(colors.search_match_background),
                        ..HighlightStyle::default()
                    },
                )],
            ),
        };
        let label = StyledText::new(text).with_default_highlights(&text_style, highlights);
        let replace_button = self.replace_enabled.then(|| {
            IconButton::new(
                SharedString::from(format!("search-panel-replace-{file_index}-{match_index}")),
                IconName::Replace,
            )
            .shape(IconButtonShape::Square)
            .icon_size(IconSize::Small)
            .tooltip(Tooltip::text("Replace"))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.replace_match(file_index, match_index, window, cx);
            }))
        });
        let item = ListItem::new(SharedString::from(format!(
            "search-panel-match-{file_index}-{match_index}"
        )))
        .indent_level(1)
        .indent_step_size(INDENT_STEP)
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label),
        )
        .when_some(replace_button, |this, button| {
            this.end_slot_on_hover(button)
        })
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            let focus_editor = event.click_count() > 1;
            if !this.focus_handle.is_focused(window) {
                window.focus(&this.focus_handle, cx);
            }
            this.open_match(
                file_index,
                match_index,
                focus_editor,
                !focus_editor,
                window,
                cx,
            );
        }));
        Some(self.render_row(
            SharedString::from(format!("search-panel-match-row-{file_index}-{match_index}")),
            Entry::Match {
                file: file_index,
                index: match_index,
            },
            item,
            window,
            cx,
        ))
    }

    fn render_results(&self, cx: &Context<Self>) -> impl IntoElement {
        let entry_count = self.entries.len();
        div().flex_1().min_h_0().pt_0p5().child(
            uniform_list(
                "search-panel-results",
                entry_count,
                cx.processor(|this, range: Range<usize>, window, cx| {
                    let entries = this.entries.get(range).map(<[Entry]>::to_vec);
                    entries
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|entry| match entry {
                            Entry::File(file) => this.render_file(file, window, cx),
                            Entry::Match { file, index } => {
                                this.render_match(file, index, window, cx)
                            }
                        })
                        .collect()
                }),
            )
            .track_scroll(&self.scroll_handle)
            .size_full(),
        )
    }
}

fn single_line_editor(
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Editor> {
    cx.new(|cx| {
        let mut editor = Editor::single_line(window, cx);
        editor.set_placeholder_text(placeholder, window, cx);
        editor
    })
}

fn parse_path_matches(text: &str, path_style: PathStyle) -> Result<PathMatcher> {
    let patterns = split_glob_patterns(text)
        .into_iter()
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(PathMatcher::new(&patterns, path_style)?)
}

fn match_line(buffer: &BufferSnapshot, range: &Range<Anchor>) -> MatchLine {
    let start = range.start.text_anchor_in(buffer).to_point(buffer);
    let end = range.end.text_anchor_in(buffer).to_point(buffer);
    let row = start.row;
    let line = buffer
        .text_for_range(Point::new(row, 0)..Point::new(row, buffer.line_len(row)))
        .collect::<String>();
    let match_start = (start.column as usize).min(line.len());
    let match_end = if end.row == row {
        (end.column as usize).clamp(match_start, line.len())
    } else {
        line.len()
    };

    let leading_whitespace = line.len() - line.trim_start().len();
    let mut cut_start = leading_whitespace.min(match_start);
    let before = &line[cut_start..match_start];
    let before_chars = before.chars().count();
    let mut text = String::new();
    if before_chars > LEFT_CONTEXT_CHARS {
        let skipped_bytes = before
            .chars()
            .take(before_chars - LEFT_CONTEXT_CHARS)
            .map(char::len_utf8)
            .sum::<usize>();
        cut_start += skipped_bytes;
        text.push('…');
    }
    let prefix_len = text.len();
    let cut_end = match_end
        + line[match_end..]
            .chars()
            .take(RIGHT_CONTEXT_CHARS)
            .map(char::len_utf8)
            .sum::<usize>();
    text.push_str(&line[cut_start..cut_end]);
    if cut_end < line.len() {
        text.push('…');
    }
    MatchLine {
        text,
        match_range: match_start - cut_start + prefix_len..match_end - cut_start + prefix_len,
    }
}

impl EventEmitter<PanelEvent> for SearchPanel {}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("search-panel")
            .key_context(self.dispatch_context(window))
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::backtab))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::collapse_selected_entry))
            .on_action(cx.listener(Self::expand_selected_entry))
            .on_action(cx.listener(Self::collapse_all_entries))
            .on_action(cx.listener(Self::expand_all_entries))
            .on_action(cx.listener(Self::clear_results))
            .on_action(cx.listener(Self::refresh_results))
            .on_action(cx.listener(Self::toggle_replace))
            .on_action(cx.listener(Self::toggle_filters))
            .on_action(cx.listener(Self::replace_all))
            .on_action(cx.listener(Self::replace_next))
            .on_action(cx.listener(Self::open_in_editor))
            .on_action(cx.listener(|this, _: &ToggleCaseSensitive, _, cx| {
                this.toggle_search_option(SearchOptions::CASE_SENSITIVE, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleWholeWord, _, cx| {
                this.toggle_search_option(SearchOptions::WHOLE_WORD, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleRegex, _, cx| {
                this.toggle_search_option(SearchOptions::REGEX, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleIncludeIgnored, _, cx| {
                this.toggle_search_option(SearchOptions::INCLUDE_IGNORED, cx)
            }))
            .child(self.render_inputs(window, cx))
            .child(self.render_toolbar(cx))
            .child(self.render_results(cx))
    }
}

impl Panel for SearchPanel {
    fn persistent_name() -> &'static str {
        "Search Panel"
    }

    fn panel_key() -> &'static str {
        SEARCH_PANEL_KEY
    }

    fn activation_focus_handle(&self, cx: &App) -> FocusHandle {
        self.query_editor.focus_handle(cx)
    }

    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if !active {
            self.clear_match_highlights(cx);
        }
    }

    fn position(&self, _: &Window, cx: &App) -> DockPosition {
        match SearchPanelSettings::get_global(cx).dock {
            DockSide::Left => DockPosition::Left,
            DockSide::Right => DockPosition::Right,
        }
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            let dock = match position {
                DockPosition::Left | DockPosition::Bottom => DockSide::Left,
                DockPosition::Right => DockSide::Right,
            };
            settings.search_panel.get_or_insert_default().dock = Some(dock);
        });
    }

    fn default_size(&self, _: &Window, cx: &App) -> Pixels {
        SearchPanelSettings::get_global(cx).default_width
    }

    fn icon(&self, _: &Window, cx: &App) -> Option<IconName> {
        SearchPanelSettings::get_global(cx)
            .button
            .then_some(IconName::ActivitySearch)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Search Panel")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        4
    }

    fn hide_button_setting(&self, _: &App) -> Option<HideStatusItem> {
        Some(HideStatusItem::new(|settings| {
            settings.search_panel.get_or_insert_default().button = Some(false);
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use project::FakeFs;
    use serde_json::json;
    use settings::SettingsStore;
    use util::path;
    use workspace::MultiWorkspace;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    async fn setup(
        cx: &mut TestAppContext,
    ) -> (
        Arc<FakeFs>,
        Entity<Workspace>,
        Entity<SearchPanel>,
        VisualTestContext,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            path!("/dir"),
            json!({
                "one.rs": "const ONE: usize = 1;",
                "two.rs": "const TWO: usize = one::ONE + one::ONE;",
                "three.rs": "const THREE: usize = one::ONE + two::TWO;",
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [path!("/dir").as_ref()], cx).await;
        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = window
            .read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone())
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let panel = workspace.update_in(&mut cx, |workspace, window, cx| {
            let panel = SearchPanel::new(workspace, window, cx);
            workspace.add_panel(panel.clone(), window, cx);
            panel
        });
        (fs, workspace, panel, cx)
    }

    fn file_names(panel: &SearchPanel) -> Vec<String> {
        panel
            .files
            .iter()
            .map(|file| file.file_name.to_string())
            .collect()
    }

    #[gpui::test]
    async fn test_search_groups_matches_by_file(cx: &mut TestAppContext) {
        let (_, _, panel, mut cx) = setup(cx).await;

        panel.update_in(&mut cx, |panel, window, cx| {
            panel
                .query_editor
                .update(cx, |editor, cx| editor.set_text("ONE", window, cx));
            panel.search(SearchMode::Manual, cx);
        });
        cx.run_until_parked();

        panel.read_with(&cx, |panel, _| {
            let mut names = file_names(panel);
            names.sort();
            assert_eq!(names, vec!["one.rs", "three.rs", "two.rs"]);
            assert_eq!(panel.match_count(), 7, "case-insensitive by default");
            assert_eq!(panel.entries.len(), 10);
        });

        panel.update_in(&mut cx, |panel, _, cx| {
            panel.toggle_file(0, cx);
        });
        panel.read_with(&cx, |panel, _| {
            let collapsed_matches = panel.files[0].matches.len();
            assert_eq!(panel.entries.len(), 10 - collapsed_matches);
        });

        panel.update_in(&mut cx, |panel, window, cx| {
            panel
                .query_editor
                .update(cx, |editor, cx| editor.set_text("", window, cx));
            panel.search(SearchMode::Manual, cx);
        });
        cx.run_until_parked();
        panel.read_with(&cx, |panel, _| {
            assert!(panel.files.is_empty());
            assert!(panel.entries.is_empty());
        });
    }

    #[gpui::test]
    async fn test_confirming_a_match_opens_it_in_the_editor(cx: &mut TestAppContext) {
        let (_, workspace, panel, mut cx) = setup(cx).await;

        panel.update_in(&mut cx, |panel, window, cx| {
            panel.search_options = SearchOptions::CASE_SENSITIVE;
            panel
                .query_editor
                .update(cx, |editor, cx| editor.set_text("TWO", window, cx));
            panel.search(SearchMode::Manual, cx);
        });
        cx.run_until_parked();

        let (file_index, match_index) = panel.read_with(&cx, |panel, _| {
            let file_index = panel
                .files
                .iter()
                .position(|file| file.file_name.as_ref() == "three.rs")
                .expect("three.rs should have a match");
            (file_index, 0)
        });
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.selected_entry = Some(Entry::Match {
                file: file_index,
                index: match_index,
            });
            window.focus(&panel.focus_handle, cx);
            panel.confirm(&Confirm, window, cx);
        });
        cx.run_until_parked();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let editor = workspace
                .active_item(cx)
                .and_then(|item| item.act_as::<Editor>(cx))
                .expect("an editor should be open");
            editor.update(cx, |editor, cx| {
                let selection = editor
                    .selections
                    .newest::<Point>(&editor.display_snapshot(cx));
                assert_eq!(selection.range(), Point::new(0, 37)..Point::new(0, 40));
                assert!(editor.focus_handle(cx).is_focused(window));
            });
        });
    }

    #[gpui::test]
    async fn test_opened_match_is_highlighted_like_a_buffer_search(cx: &mut TestAppContext) {
        let (_, workspace, panel, mut cx) = setup(cx).await;

        panel.update_in(&mut cx, |panel, window, cx| {
            panel.search_options = SearchOptions::CASE_SENSITIVE;
            panel
                .query_editor
                .update(cx, |editor, cx| editor.set_text("ONE", window, cx));
            panel.search(SearchMode::Manual, cx);
        });
        cx.run_until_parked();

        panel.update_in(&mut cx, |panel, window, cx| {
            let file_index = panel
                .files
                .iter()
                .position(|file| file.file_name.as_ref() == "two.rs")
                .expect("two.rs should have matches");
            panel.open_match(file_index, 1, true, false, window, cx);
        });
        cx.run_until_parked();

        let editor = workspace.update_in(&mut cx, |workspace, window, cx| {
            let editor = workspace
                .active_item(cx)
                .and_then(|item| item.act_as::<Editor>(cx))
                .expect("an editor should be open");
            editor.update(cx, |editor, cx| {
                assert!(editor.has_background_highlights(HighlightKey::SearchPanelMatches));
                let search_match_background = cx.theme().colors().search_match_background;
                let highlighted_ranges = editor
                    .all_text_background_highlights(window, cx)
                    .into_iter()
                    .filter(|(_, color)| *color == search_match_background)
                    .map(|(range, _)| range.start.column()..range.end.column())
                    .collect::<Vec<_>>();
                assert_eq!(highlighted_ranges, vec![24..27, 35..38]);
            });
            editor
        });

        panel.update_in(&mut cx, |panel, window, cx| {
            panel
                .query_editor
                .update(cx, |editor, cx| editor.set_text("TWO", window, cx));
            panel.search(SearchMode::Manual, cx);
        });
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert!(!editor.has_background_highlights(HighlightKey::SearchPanelMatches));
        });

        panel.update_in(&mut cx, |panel, window, cx| {
            let file_index = panel
                .files
                .iter()
                .position(|file| file.file_name.as_ref() == "two.rs")
                .expect("two.rs should have a match");
            panel.open_match(file_index, 0, true, false, window, cx);
        });
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert!(editor.has_background_highlights(HighlightKey::SearchPanelMatches));
        });
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.set_active(false, window, cx);
        });
        editor.read_with(&cx, |editor, _| {
            assert!(
                !editor.has_background_highlights(HighlightKey::SearchPanelMatches),
                "hiding the panel clears the highlights"
            );
        });
    }

    #[gpui::test]
    async fn test_opened_match_behaves_like_a_buffer_search_match(cx: &mut TestAppContext) {
        use workspace::searchable::{SearchToken, SearchableItem as _};

        let (_, workspace, panel, mut cx) = setup(cx).await;
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.search_options = SearchOptions::CASE_SENSITIVE;
            panel
                .query_editor
                .update(cx, |editor, cx| editor.set_text("ONE", window, cx));
            panel.search(SearchMode::Manual, cx);
        });
        cx.run_until_parked();
        let file_index = panel.read_with(&cx, |panel, _| {
            panel
                .files
                .iter()
                .position(|file| file.file_name.as_ref() == "two.rs")
                .expect("two.rs should have matches")
        });
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.open_match(file_index, 0, true, false, window, cx);
        });
        cx.run_until_parked();
        let editor = workspace.update_in(&mut cx, |workspace, _, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.act_as::<Editor>(cx))
                .expect("an editor should be open")
        });

        cx.executor()
            .advance_clock(editor::SELECTION_HIGHLIGHT_DEBOUNCE_TIMEOUT);
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert!(
                !editor.has_background_highlights(HighlightKey::SelectedTextHighlight),
                "a match opened from search should not highlight other occurrences of its text"
            );
        });

        editor.update_in(&mut cx, |editor, window, cx| {
            editor.fold_ranges(
                vec![Point::new(0, 35)..Point::new(0, 38)],
                false,
                window,
                cx,
            );
        });
        editor.update(&mut cx, |editor, cx| {
            assert!(!editor.display_text(cx).contains("one::ONE;"));
        });
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.open_match(file_index, 1, true, false, window, cx);
        });
        cx.run_until_parked();
        editor.update(&mut cx, |editor, cx| {
            assert!(
                editor.display_text(cx).contains("one::ONE;"),
                "opening a folded match unfolds it"
            );
        });

        editor.update_in(&mut cx, |editor, window, cx| {
            editor.update_matches(&[], None, SearchToken::default(), window, cx);
        });
        editor.read_with(&cx, |editor, _| {
            assert!(
                !editor.has_background_highlights(HighlightKey::SearchPanelMatches),
                "a find bar search replaces the panel highlights"
            );
        });

        panel.update_in(&mut cx, |panel, window, cx| {
            panel.open_match(file_index, 0, true, false, window, cx);
            panel.set_active(false, window, cx);
        });
        cx.run_until_parked();
        editor.read_with(&cx, |editor, _| {
            assert!(
                !editor.has_background_highlights(HighlightKey::SearchPanelMatches),
                "highlights from an open that finished after the panel was hidden are skipped"
            );
        });
    }

    #[gpui::test]
    async fn test_deploy_search_opens_the_panel_with_the_query(cx: &mut TestAppContext) {
        let (_, workspace, panel, mut cx) = setup(cx).await;

        workspace.update_in(&mut cx, |workspace, window, cx| {
            ProjectSearchView::deploy_search(
                workspace,
                &DeploySearch {
                    query: Some("THREE".to_string()),
                    replace_enabled: true,
                    case_sensitive: Some(true),
                    ..DeploySearch::default()
                },
                window,
                cx,
            );
        });
        cx.run_until_parked();

        workspace.read_with(&cx, |workspace, cx| {
            assert!(workspace.left_dock().read(cx).is_open());
            assert!(
                workspace
                    .active_item(cx)
                    .and_then(|item| item.downcast::<ProjectSearchView>())
                    .is_none(),
                "the panel replaces the search tab"
            );
        });
        panel.update_in(&mut cx, |panel, window, cx| {
            assert!(panel.query_editor.focus_handle(cx).is_focused(window));
            assert_eq!(panel.query_text(cx), "THREE");
            assert!(panel.replace_enabled);
            assert!(panel.search_options.contains(SearchOptions::CASE_SENSITIVE));
            assert_eq!(file_names(panel), vec!["three.rs"]);
        });
    }

    #[gpui::test]
    async fn test_open_in_editor_keeps_the_results(cx: &mut TestAppContext) {
        let (_, workspace, panel, mut cx) = setup(cx).await;

        panel.update_in(&mut cx, |panel, window, cx| {
            panel
                .query_editor
                .update(cx, |editor, cx| editor.set_text("ONE", window, cx));
            panel.search(SearchMode::Manual, cx);
        });
        cx.run_until_parked();
        panel.update_in(&mut cx, |panel, window, cx| {
            panel.open_in_editor(&OpenInEditor, window, cx);
        });
        cx.run_until_parked();

        workspace.read_with(&cx, |workspace, cx| {
            let search_view = workspace
                .active_item(cx)
                .and_then(|item| item.downcast::<ProjectSearchView>())
                .expect("a search tab should be open");
            let search_view = search_view.read(cx);
            assert_eq!(search_view.search_query_text(cx), "ONE");
            assert_eq!(search_view.get_matches(cx).len(), 7);
        });
    }

    #[gpui::test]
    async fn test_replace_all_saves_clean_buffers(cx: &mut TestAppContext) {
        let (fs, _, panel, mut cx) = setup(cx).await;

        panel.update_in(&mut cx, |panel, window, cx| {
            panel.replace_enabled = true;
            panel.search_options = SearchOptions::CASE_SENSITIVE;
            panel
                .query_editor
                .update(cx, |editor, cx| editor.set_text("ONE", window, cx));
            panel
                .replacement_editor
                .update(cx, |editor, cx| editor.set_text("UNO", window, cx));
            panel.search(SearchMode::Manual, cx);
        });
        cx.run_until_parked();

        panel.update_in(&mut cx, |panel, window, cx| {
            panel.replace_all(&ReplaceAll, window, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            fs.load(path!("/dir/one.rs").as_ref()).await.unwrap(),
            "const UNO: usize = 1;"
        );
        assert_eq!(
            fs.load(path!("/dir/two.rs").as_ref()).await.unwrap(),
            "const TWO: usize = one::UNO + one::UNO;"
        );
        panel.read_with(&cx, |panel, _| {
            assert_eq!(
                panel.match_count(),
                0,
                "replaced matches are gone after the refresh"
            );
        });
    }
}
