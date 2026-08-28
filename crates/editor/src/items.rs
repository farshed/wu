use crate::{
    ActiveDebugLine, Anchor, Autoscroll, BufferSerialization, Capability, Editor, EditorEvent,
    EditorSettings, FormatTarget, MultiBuffer, MultiBufferSnapshot, NavigationData,
    ReportEditorEvent, SelectionEffects, ToPoint as _,
    display_map::HighlightKey,
    editor_settings::SeedQuerySetting,
    persistence::{EditorDb, SerializedEditor},
    scroll::{ScrollAnchor, ScrollOffset},
};
use anyhow::{Context as _, Result, anyhow};
use collections::{HashMap, HashSet};
use file_icons::FileIcons;
use fs::MTime;
use futures::channel::oneshot;
use git::status::GitSummary;
use gpui::{
    AnyElement, App, AsyncWindowContext, Context, Entity, EntityId, EventEmitter, Font,
    IntoElement, ParentElement, Pixels, SharedString, Styled, Task, WeakEntity, Window,
};
use language::{
    Bias, Buffer, BufferRow, CharKind, CharScopeContext, HighlightedText, LocalFile, PLAIN_TEXT,
    Point,
    language_settings::{FormatOnSave, LanguageSettings},
};
use lsp::DiagnosticSeverity;
use multi_buffer::{BufferOffset, MultiBufferOffset, MultiBufferRow};
use project::{
    File, Project, ProjectItem as _, ProjectPath, git_store::GitStore, lsp_store::FormatTrigger,
    project_settings::ProjectSettings, search::SearchQuery,
};
use rope::TextSummary;
use settings::Settings;
use std::{
    any::{Any, TypeId},
    borrow::Cow,
    cmp::{self, Ordering},
    num::NonZeroU32,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};
use text::{BufferSnapshot, OffsetRangeExt, ToPoint as _};
use ui::{IconDecorationKind, prelude::*};
use util::{ResultExt, TryFutureExt, debug_panic, paths::PathExt};
use workspace::item::{ItemSettings, SerializableItem, TabContentParams};
use workspace::{
    ItemId, ItemNavHistory, ToolbarItemLocation, Workspace, WorkspaceId,
    invalid_item_view::InvalidItemView,
    item::{Item, ItemBufferKind, ItemEvent, ProjectItem, SaveOptions},
    searchable::{
        Direction, FilteredSearchRange, SearchEvent, SearchToken, SearchableItem,
        SearchableItemHandle,
    },
};
use workspace::{
    Pane, TabBarSettings, WorkspaceSettings,
    item::ProjectItemKind,
    searchable::SearchOptions,
};
use zed_actions::preview::{
    markdown::OpenPreview as OpenMarkdownPreview, svg::OpenPreview as OpenSvgPreview,
};

pub const MAX_TAB_TITLE_LEN: usize = 24;

impl Item for Editor {
    type Event = EditorEvent;

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if TypeId::of::<Self>() == type_id {
            Some(self_handle.clone().into())
        } else if TypeId::of::<MultiBuffer>() == type_id {
            Some(self_handle.read(cx).buffer.clone().into())
        } else {
            None
        }
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(data) = data.downcast_ref::<NavigationData>() {
            let newest_selection = self.selections.newest::<Point>(&self.display_snapshot(cx));
            let buffer = self.buffer.read(cx).read(cx);
            let offset = if buffer.can_resolve(&data.cursor_anchor) {
                data.cursor_anchor.to_point(&buffer)
            } else {
                buffer.clip_point(data.cursor_position, Bias::Left)
            };

            let mut scroll_anchor = data.scroll_anchor;
            if !buffer.can_resolve(&scroll_anchor.anchor) {
                scroll_anchor.anchor = buffer.anchor_before(
                    buffer.clip_point(Point::new(data.scroll_top_row, 0), Bias::Left),
                );
            }

            drop(buffer);

            if newest_selection.head() == offset {
                false
            } else {
                self.set_scroll_anchor(scroll_anchor, window, cx);
                self.change_selections(
                    SelectionEffects::default().nav_history(false),
                    window,
                    cx,
                    |s| s.select_ranges([offset..offset]),
                );
                true
            }
        } else {
            false
        }
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        let multi_buffer = self.buffer().read(cx);
        if let Some(file) = multi_buffer
            .as_singleton()
            .and_then(|buffer| buffer.read(cx).file())
            .and_then(|file| File::from_dyn(Some(file)))
        {
            Some(
                file.worktree
                    .read(cx)
                    .absolutize(&file.path)
                    .compact()
                    .to_string_lossy()
                    .into_owned()
                    .into(),
            )
        } else {
            let title = multi_buffer.title(cx);
            (!title.is_empty()).then(|| title.to_string().into())
        }
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        None
    }

    fn tab_content_text(&self, detail: usize, cx: &App) -> SharedString {
        if let Some(path) = path_for_buffer(&self.buffer, detail, true, cx) {
            path.to_string().into()
        } else {
            // Use the same logic as the displayed title for consistency
            self.buffer.read(cx).title(cx).to_string().into()
        }
    }

    fn suggested_filename(&self, cx: &App) -> SharedString {
        let multi_buffer = self.buffer.read(cx);
        let title = multi_buffer.title(cx);
        if let Some(buffer) = multi_buffer.as_singleton() {
            let buffer = buffer.read(cx);
            if buffer.file().is_none()
                && let Some(language) = buffer.language()
                && *language != *PLAIN_TEXT
                && let Some(suffix) = language.path_suffixes().first()
                && !suffix.is_empty()
                && !title.ends_with(&format!(".{suffix}"))
            {
                return format!("{title}.{suffix}").into();
            }
        }

        title.to_string().into()
    }

    fn tab_icon(&self, _: &Window, cx: &App) -> Option<Icon> {
        ItemSettings::get_global(cx)
            .file_icons
            .then(|| {
                path_for_buffer(&self.buffer, 0, true, cx)
                    .and_then(|path| FileIcons::get_icon(Path::new(&*path), cx))
            })
            .flatten()
            .map(Icon::from_path)
    }

    fn tab_content(&self, params: TabContentParams, _: &Window, cx: &App) -> AnyElement {
        let label_color = if ItemSettings::get_global(cx).git_status {
            self.buffer()
                .read(cx)
                .as_singleton()
                .and_then(|buffer| {
                    let buffer = buffer.read(cx);
                    let path = buffer.project_path(cx)?;
                    let buffer_id = buffer.remote_id();
                    let project = self.project()?.read(cx);
                    let entry = project.entry_for_path(&path, cx)?;
                    let status = project
                        .git_store()
                        .read(cx)
                        .display_status_for_buffer_id(buffer_id, cx)?;

                    Some(entry_git_aware_label_color(
                        status.summary(),
                        entry.is_ignored,
                        params.selected,
                    ))
                })
                .unwrap_or_else(|| entry_label_color(params.selected))
        } else {
            entry_label_color(params.selected)
        };

        let description = params.detail.and_then(|detail| {
            let path = path_for_buffer(&self.buffer, detail, false, cx)?;
            let description = path.trim();

            if description.is_empty() {
                return None;
            }

            Some(util::truncate_and_trailoff(
                description,
                params.max_title_len.unwrap_or(MAX_TAB_TITLE_LEN),
            ))
        });

        // Whether the file was saved in the past but is now deleted.
        let was_deleted: bool = self
            .buffer()
            .read(cx)
            .as_singleton()
            .and_then(|buffer| buffer.read(cx).file())
            .is_some_and(|file| file.disk_state().is_deleted());

        h_flex()
            .gap_1()
            .when(params.truncate_title_middle, |this| {
                this.w_full().min_w_0().overflow_hidden()
            })
            .child(
                Label::new(if params.truncate_title_middle {
                    self.title(cx).to_string()
                } else {
                    util::truncate_and_trailoff(
                        &self.title(cx),
                        params.max_title_len.unwrap_or(MAX_TAB_TITLE_LEN),
                    )
                })
                .single_line()
                .color(label_color)
                .when(params.truncate_title_middle, |this| {
                    this.truncate_middle().flex_1()
                })
                .when(params.preview, |this| this.italic())
                .when(was_deleted, |this| this.strikethrough()),
            )
            .when_some(description, |this, description| {
                this.child(
                    Label::new(description)
                        .single_line()
                        .size(LabelSize::XSmall)
                        .when(params.truncate_title_middle, |this| {
                            this.truncate_start().flex_shrink()
                        })
                        .color(Color::Muted),
                )
            })
            .into_any_element()
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(EntityId, &dyn project::ProjectItem),
    ) {
        self.buffer
            .read(cx)
            .for_each_buffer(&mut |buffer| f(buffer.entity_id(), buffer.read(cx)));
    }

    fn buffer_kind(&self, cx: &App) -> ItemBufferKind {
        match self.buffer.read(cx).is_singleton() {
            true => ItemBufferKind::Singleton,
            false => ItemBufferKind::Multibuffer,
        }
    }

    fn active_project_path(&self, cx: &App) -> Option<ProjectPath> {
        self.active_buffer(cx)?.read(cx).project_path(cx)
    }

    fn can_save_as(&self, cx: &App) -> bool {
        self.buffer.read(cx).is_singleton()
    }

    fn can_split(&self) -> bool {
        true
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Editor>>>
    where
        Self: Sized,
    {
        Task::ready(Some(cx.new(|cx| self.clone(window, cx))))
    }

    fn set_nav_history(
        &mut self,
        history: ItemNavHistory,
        _window: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.nav_history = Some(history);
    }

    fn on_removed(&self, cx: &mut Context<Self>) {
        self.report_editor_event(ReportEditorEvent::Closed, None, cx);
    }

    fn deactivated(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selections.newest_anchor();
        self.push_to_nav_history(selection.head(), None, true, false, cx);
    }

    fn workspace_deactivated(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.hide_hovered_link(cx);
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.buffer().read(cx).read(cx).is_dirty()
    }

    fn capability(&self, cx: &App) -> Capability {
        self.capability(cx)
    }

    // Note: this mirrors the logic in `Editor::toggle_read_only`, but is reachable
    // without relying on focus-based action dispatch.
    fn toggle_read_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(buffer) = self.buffer.read(cx).as_singleton() {
            buffer.update(cx, |buffer, cx| {
                buffer.set_capability(
                    match buffer.capability() {
                        Capability::ReadWrite => Capability::Read,
                        Capability::Read => Capability::ReadWrite,
                        Capability::ReadOnly => Capability::ReadOnly,
                    },
                    cx,
                );
            });
        }
        cx.notify();
        window.refresh();
    }

    fn has_deleted_file(&self, cx: &App) -> bool {
        self.buffer().read(cx).read(cx).has_deleted_file()
    }

    fn has_conflict(&self, cx: &App) -> bool {
        self.buffer().read(cx).read(cx).has_conflict()
    }

    fn can_save(&self, cx: &App) -> bool {
        if self.read_only(cx) {
            return false;
        }
        let buffer = &self.buffer().read(cx);
        if let Some(buffer) = buffer.as_singleton() {
            buffer.read(cx).project_path(cx).is_some()
        } else {
            true
        }
    }

    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        if self.read_only(cx) {
            return Task::ready(Ok(()));
        }
        // Add meta data tracking # of auto saves
        if options.autosave {
            self.report_editor_event(ReportEditorEvent::Saved { auto_saved: true }, None, cx);
        } else {
            self.report_editor_event(ReportEditorEvent::Saved { auto_saved: false }, None, cx);
        }

        let buffers = self.buffer().clone().read(cx).all_buffers();
        let buffers = buffers
            .into_iter()
            .map(|handle| handle.read(cx).base_buffer().unwrap_or(handle.clone()))
            .collect::<HashSet<_>>();

        let buffers_to_save = if self.buffer.read(cx).is_singleton() && !options.autosave {
            buffers
        } else {
            buffers
                .into_iter()
                // Skip untitled buffers: a multi-buffer (e.g. project search results) can
                // excerpt a buffer with no file on disk, which can only be persisted via
                // `save_as`. Trying to save it here errors and aborts the whole save.
                .filter(|buffer| {
                    let buffer = buffer.read(cx);
                    buffer.is_dirty() && !buffer.read_only() && buffer.file().is_some()
                })
                .collect()
        };

        let format_trigger = if options.force_format {
            FormatTrigger::Manual
        } else {
            FormatTrigger::Save
        };

        cx.spawn_in(window, async move |this, cx| {
            if options.format {
                let format_task = this.update_in(cx, |editor, window, cx| {
                    let format_target = compute_format_target(
                        &buffers_to_save,
                        format_trigger,
                        editor.buffer(),
                        project.read(cx).git_store(),
                        cx,
                    );
                    format_target.map(|target| {
                        editor.perform_format(project.clone(), format_trigger, target, window, cx)
                    })
                })?;
                if let Some(format_task) = format_task {
                    format_task.await?;
                }
            }

            if !buffers_to_save.is_empty() {
                project
                    .update(cx, |project, cx| {
                        project.save_buffers(buffers_to_save.clone(), cx)
                    })
                    .await?;
            }

            Ok(())
        })
    }

    fn save_as(
        &mut self,
        project: Entity<Project>,
        path: ProjectPath,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let buffer = self
            .buffer()
            .read(cx)
            .as_singleton()
            .expect("cannot call save_as on an excerpt list");

        let file_extension = path.path.extension().map(|a| a.to_string());
        self.report_editor_event(
            ReportEditorEvent::Saved { auto_saved: false },
            file_extension,
            cx,
        );

        project.update(cx, |project, cx| project.save_buffer_as(buffer, path, cx))
    }

    fn reload(
        &mut self,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let buffer = self.buffer().clone();
        let buffers = self.buffer.read(cx).all_buffers();
        let reload_buffers =
            project.update(cx, |project, cx| project.reload_buffers(buffers, true, cx));
        cx.spawn_in(window, async move |this, cx| {
            let transaction = reload_buffers.log_err().await;
            this.update(cx, |editor, cx| {
                editor.request_autoscroll(Autoscroll::fit(), cx)
            })?;
            buffer.update(cx, |buffer, cx| {
                if let Some(transaction) = transaction
                    && !buffer.is_singleton()
                {
                    buffer.push_transaction(&transaction.0, cx);
                }
            });
            Ok(())
        })
    }

    fn as_searchable(
        &self,
        handle: &Entity<Self>,
        _: &App,
    ) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(handle.clone()))
    }

    fn pixel_position_of_cursor(&self, _: &App) -> Option<gpui::Point<Pixels>> {
        self.pixel_position_of_newest_cursor
    }

    fn breadcrumb_location(&self, cx: &App) -> ToolbarItemLocation {
        if self.breadcrumbs_visible() && self.buffer().read(cx).is_singleton() {
            ToolbarItemLocation::PrimaryLeft
        } else {
            ToolbarItemLocation::Hidden
        }
    }

    // In a non-singleton case, the breadcrumbs are actually shown on sticky file headers of the multibuffer.
    fn breadcrumbs(&self, cx: &App) -> Option<(Vec<HighlightedText>, Option<Font>)> {
        if self.buffer.read(cx).is_singleton() {
            let font = theme_settings::ThemeSettings::get_global(cx)
                .buffer_font
                .clone();
            Some((self.breadcrumbs_inner(cx)?, Some(font)))
        } else {
            None
        }
    }

    fn breadcrumb_prefix(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        (!TabBarSettings::get_global(cx).show && ItemSettings::get_global(cx).file_icons)
            .then(|| {
                path_for_buffer(&self.buffer, 0, true, cx)
                    .and_then(|path| FileIcons::get_icon(Path::new(&*path), cx))
            })
            .flatten()
            .map(|icon_path| Icon::from_path(icon_path).into_any_element())
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace = Some((workspace.weak_handle(), workspace.database_id()));
        if let Some(workspace_entity) = &workspace.weak_handle().upgrade() {
            cx.subscribe(
                workspace_entity,
                |editor, _, event: &workspace::Event, cx| {
                    if let workspace::Event::ModalOpened = event {
                        editor.mouse_context_menu.take();
                        editor.hide_blame_popover(true, cx);
                    }
                },
            )
            .detach();
        }

        // Load persisted folds if this editor doesn't already have folds.
        // This handles manually-opened files (not workspace restoration).
        let display_snapshot = self
            .display_map
            .update(cx, |display_map, cx| display_map.snapshot(cx));
        let has_folds = display_snapshot
            .folds_in_range(MultiBufferOffset(0)..display_snapshot.buffer_snapshot().len())
            .next()
            .is_some();

        if !has_folds {
            if let Some(workspace_id) = workspace.database_id()
                && let Some(file_path) = self.buffer().read(cx).as_singleton().and_then(|buffer| {
                    project::File::from_dyn(buffer.read(cx).file()).map(|file| file.abs_path(cx))
                })
            {
                self.load_folds_from_db(workspace_id, file_path, window, cx);
            }
        }
    }

    fn pane_changed(&mut self, new_pane_id: EntityId, cx: &mut Context<Self>) {
        if self
            .highlighted_rows
            .get(&TypeId::of::<ActiveDebugLine>())
            .is_some_and(|lines| !lines.is_empty())
            && let Some(breakpoint_store) = self.breakpoint_store.as_ref()
        {
            breakpoint_store.update(cx, |store, _cx| {
                store.set_active_debug_pane_id(new_pane_id);
            });
        }
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        match event {
            EditorEvent::Saved | EditorEvent::TitleChanged => {
                f(ItemEvent::UpdateTab);
                f(ItemEvent::UpdateBreadcrumbs);
            }

            EditorEvent::Reparsed(_) => {
                f(ItemEvent::UpdateBreadcrumbs);
            }

            EditorEvent::SelectionsChanged { local } if *local => {
                f(ItemEvent::UpdateBreadcrumbs);
            }

            EditorEvent::BreadcrumbsChanged | EditorEvent::OutlineSymbolsChanged => {
                f(ItemEvent::UpdateBreadcrumbs);
            }

            EditorEvent::DirtyChanged | EditorEvent::CapabilityChanged => {
                f(ItemEvent::UpdateTab);
            }

            EditorEvent::BufferEdited => {
                f(ItemEvent::Edit);
                f(ItemEvent::UpdateBreadcrumbs);
            }

            EditorEvent::BufferRangesUpdated { .. } | EditorEvent::BuffersRemoved { .. } => {
                f(ItemEvent::Edit);
            }

            _ => {}
        }
    }

    fn tab_extra_context_menu_actions(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<(SharedString, Box<dyn gpui::Action>)> {
        let mut actions = Vec::new();

        let is_markdown = self
            .buffer()
            .read(cx)
            .as_singleton()
            .and_then(|buffer| buffer.read(cx).language())
            .is_some_and(|language| language.name().as_ref() == "Markdown");

        let is_svg = self
            .buffer()
            .read(cx)
            .as_singleton()
            .and_then(|buffer| buffer.read(cx).file())
            .is_some_and(|file| {
                std::path::Path::new(file.file_name(cx))
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
            });

        if is_markdown {
            actions.push((
                "Open Markdown Preview".into(),
                Box::new(OpenMarkdownPreview) as Box<dyn gpui::Action>,
            ));
        }

        if is_svg {
            actions.push((
                "Open SVG Preview".into(),
                Box::new(OpenSvgPreview) as Box<dyn gpui::Action>,
            ));
        }

        actions
    }

    fn preserve_preview(&self, cx: &App) -> bool {
        self.buffer.read(cx).preserve_preview(cx)
    }
}

impl SerializableItem for Editor {
    fn serialized_item_kind() -> &'static str {
        "Editor"
    }

    fn cleanup(
        workspace_id: WorkspaceId,
        alive_items: Vec<ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        workspace::delete_unloaded_items(
            alive_items,
            workspace_id,
            "editors",
            &EditorDb::global(cx),
            cx,
        )
    }

    fn deserialize(
        project: Entity<Project>,
        _workspace: WeakEntity<Workspace>,
        workspace_id: workspace::WorkspaceId,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let serialized_editor = match EditorDb::global(cx)
            .get_serialized_editor(item_id, workspace_id)
            .context("Failed to query editor state")
        {
            Ok(Some(serialized_editor)) => {
                if ProjectSettings::get_global(cx)
                    .session
                    .restore_unsaved_buffers
                {
                    serialized_editor
                } else {
                    SerializedEditor {
                        abs_path: serialized_editor.abs_path,
                        contents: None,
                        language: None,
                        mtime: None,
                    }
                }
            }
            Ok(None) => {
                return Task::ready(Err(anyhow!(
                    "Unable to deserialize editor: No entry in database for item_id: {item_id} and workspace_id {workspace_id:?}"
                )));
            }
            Err(error) => {
                return Task::ready(Err(error));
            }
        };
        log::debug!(
            "Deserialized editor {item_id:?} in workspace {workspace_id:?}, {serialized_editor:?}"
        );

        match serialized_editor {
            SerializedEditor {
                abs_path: None,
                contents: Some(contents),
                language,
                ..
            } => window.spawn(cx, {
                let project = project.clone();
                async move |cx| {
                    let language_registry =
                        project.read_with(cx, |project, _| project.languages().clone());

                    let language = if let Some(language_name) = language {
                        // We don't fail here, because we'd rather not set the language if the name changed
                        // than fail to restore the buffer.
                        language_registry
                            .language_for_name(&language_name)
                            .await
                            .ok()
                    } else {
                        None
                    };

                    // First create the empty buffer
                    let buffer = project
                        .update(cx, |project, cx| project.create_buffer(language, true, cx))
                        .await
                        .context("Failed to create buffer while deserializing editor")?;

                    // Then set the text so that the dirty bit is set correctly
                    buffer.update(cx, |buffer, cx| {
                        buffer.set_language_registry(language_registry);
                        buffer.set_text(contents, cx);
                        if let Some(entry) = buffer.peek_undo_stack() {
                            buffer.forget_transaction(entry.transaction_id());
                        }
                    });

                    cx.update(|window, cx| {
                        cx.new(|cx| {
                            let mut editor = Editor::for_buffer(buffer, Some(project), window, cx);

                            editor.read_metadata_from_db(item_id, workspace_id, window, cx);
                            editor
                        })
                    })
                }
            }),
            SerializedEditor {
                abs_path: Some(abs_path),
                contents,
                mtime,
                ..
            } => {
                let opened_buffer = project.update(cx, |project, cx| {
                    let (worktree, path) = project.find_worktree(&abs_path, cx)?;
                    let project_path = ProjectPath {
                        worktree_id: worktree.read(cx).id(),
                        path: path,
                    };
                    Some(project.open_path(project_path, cx))
                });

                match opened_buffer {
                    Some(opened_buffer) => window.spawn(cx, async move |cx| {
                        let (_, buffer) = opened_buffer
                            .await
                            .context("Failed to open path in project")?;

                        if let Some(contents) = contents {
                            buffer.update(cx, |buffer, cx| {
                                restore_serialized_buffer_contents(buffer, contents, mtime, cx);
                            });
                        }

                        cx.update(|window, cx| {
                            cx.new(|cx| {
                                let mut editor =
                                    Editor::for_buffer(buffer, Some(project), window, cx);

                                editor.read_metadata_from_db(item_id, workspace_id, window, cx);
                                editor
                            })
                        })
                    }),
                    None => {
                        // File is not in any worktree (e.g., opened as a standalone file).
                        // Open the buffer directly via the project rather than through
                        // workspace.open_abs_path(), which has the side effect of adding
                        // the item to a pane. The caller (deserialize_to) will add the
                        // returned item to the correct pane.
                        window.spawn(cx, async move |cx| {
                            let buffer = project
                                .update(cx, |project, cx| project.open_local_buffer(&abs_path, cx))
                                .await
                                .with_context(|| {
                                    format!("Failed to open buffer for {abs_path:?}")
                                })?;

                            if let Some(contents) = contents {
                                buffer.update(cx, |buffer, cx| {
                                    restore_serialized_buffer_contents(buffer, contents, mtime, cx);
                                });
                            }

                            cx.update(|window, cx| {
                                cx.new(|cx| {
                                    let mut editor =
                                        Editor::for_buffer(buffer, Some(project), window, cx);
                                    editor.read_metadata_from_db(item_id, workspace_id, window, cx);
                                    editor
                                })
                            })
                        })
                    }
                }
            }
            SerializedEditor {
                abs_path: None,
                contents: None,
                ..
            } => window.spawn(cx, async move |cx| {
                let buffer = project
                    .update(cx, |project, cx| project.create_buffer(None, true, cx))
                    .await
                    .context("Failed to create buffer")?;

                cx.update(|window, cx| {
                    cx.new(|cx| {
                        let mut editor = Editor::for_buffer(buffer, Some(project), window, cx);

                        editor.read_metadata_from_db(item_id, workspace_id, window, cx);
                        editor
                    })
                })
            }),
        }
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: ItemId,
        closing: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let buffer_serialization = self.buffer_serialization?;
        let project = self.project.clone()?;

        let serialize_dirty_buffers = match buffer_serialization {
            // Always serialize dirty buffers, including for worktree-less windows.
            // This enables hot-exit functionality for empty windows and single files.
            BufferSerialization::All => true,
            BufferSerialization::NonDirtyBuffers => false,
        };

        if closing && !serialize_dirty_buffers {
            return None;
        }

        let workspace_id = workspace.database_id()?;

        let buffer = self.buffer().read(cx).as_singleton()?;

        let abs_path = buffer.read(cx).file().and_then(|file| {
            let worktree_id = file.worktree_id(cx);
            project
                .read(cx)
                .worktree_for_id(worktree_id, cx)
                .map(|worktree| worktree.read(cx).absolutize(file.path()))
                .or_else(|| {
                    let full_path = file.full_path(cx);
                    let project_path = project.read(cx).find_project_path(&full_path, cx)?;
                    project.read(cx).absolute_path(&project_path, cx)
                })
        });

        let is_dirty = buffer.read(cx).is_dirty();
        let mtime = buffer.read(cx).saved_mtime();

        let snapshot = buffer.read(cx).snapshot();

        let db = EditorDb::global(cx);
        Some(cx.background_spawn(async move {
            let (contents, language) = if serialize_dirty_buffers && is_dirty {
                let contents = snapshot.text();
                let language = snapshot.language().map(|lang| lang.name().to_string());
                (Some(contents), language)
            } else {
                (None, None)
            };

            let editor = SerializedEditor {
                abs_path,
                contents,
                language,
                mtime,
            };
            log::debug!("Serializing editor {item_id:?} in workspace {workspace_id:?}");
            db.save_serialized_editor(item_id, workspace_id, editor)
                .await
                .context("failed to save serialized editor")
        }))
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        self.should_serialize_buffer()
            && matches!(
                event,
                EditorEvent::Saved
                    | EditorEvent::DirtyChanged
                    | EditorEvent::BufferEdited
                    | EditorEvent::FileHandleChanged
            )
    }
}

#[derive(Debug, Default)]
struct EditorRestorationData {
    entries: HashMap<PathBuf, RestorationData>,
}

#[derive(Default, Debug)]
pub struct RestorationData {
    pub scroll_position: (BufferRow, gpui::Point<ScrollOffset>),
    pub folds: Vec<Range<Point>>,
    pub selections: Vec<Range<Point>>,
}

impl ProjectItem for Editor {
    type Item = Buffer;

    fn project_item_kind() -> Option<ProjectItemKind> {
        Some(ProjectItemKind("Editor"))
    }

    fn for_project_item(
        project: Entity<Project>,
        pane: Option<&Pane>,
        buffer: Entity<Buffer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut editor = Self::for_buffer(buffer.clone(), Some(project), window, cx);
        let multibuffer_snapshot = editor.buffer().read(cx).snapshot(cx);

        if let Some(buffer_snapshot) = editor.buffer().read(cx).snapshot(cx).as_singleton()
            && WorkspaceSettings::get(None, cx).restore_on_file_reopen
            && let Some(restoration_data) = Self::project_item_kind()
                .and_then(|kind| pane.as_ref()?.project_item_restoration_data.get(&kind))
                .and_then(|data| data.downcast_ref::<EditorRestorationData>())
                .and_then(|data| {
                    let file = project::File::from_dyn(buffer.read(cx).file())?;
                    data.entries.get(&file.abs_path(cx))
                })
        {
            if !restoration_data.folds.is_empty() {
                editor.fold_ranges(
                    clip_ranges(&restoration_data.folds, buffer_snapshot),
                    false,
                    window,
                    cx,
                );
            }
            if !restoration_data.selections.is_empty() {
                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                    s.select_ranges(clip_ranges(&restoration_data.selections, buffer_snapshot));
                });
            }
            let (top_row, offset) = restoration_data.scroll_position;
            let anchor = multibuffer_snapshot.anchor_before(Point::new(top_row, 0));
            editor.set_scroll_anchor(ScrollAnchor { anchor, offset }, window, cx);
        }

        editor
    }

    fn for_broken_project_item(
        abs_path: &Path,
        is_local: bool,
        e: &anyhow::Error,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<InvalidItemView> {
        Some(InvalidItemView::new(abs_path, is_local, e, window, cx))
    }
}

fn clip_ranges<'a>(
    original: impl IntoIterator<Item = &'a Range<Point>> + 'a,
    snapshot: &'a BufferSnapshot,
) -> Vec<Range<Point>> {
    original
        .into_iter()
        .map(|range| {
            snapshot.clip_point(range.start, Bias::Left)
                ..snapshot.clip_point(range.end, Bias::Right)
        })
        .collect()
}

impl EventEmitter<SearchEvent> for Editor {}

impl Editor {
    pub fn update_restoration_data(
        &self,
        cx: &mut Context<Self>,
        write: impl for<'a> FnOnce(&'a mut RestorationData) + 'static,
    ) {
        if self.mode.is_minimap() || !WorkspaceSettings::get(None, cx).restore_on_file_reopen {
            return;
        }

        let editor = cx.entity();
        cx.defer(move |cx| {
            editor.update(cx, |editor, cx| {
                let kind = Editor::project_item_kind()?;
                let pane = editor.workspace()?.read(cx).pane_for(&cx.entity())?;
                let buffer = editor.buffer().read(cx).as_singleton()?;
                let file_abs_path = project::File::from_dyn(buffer.read(cx).file())?.abs_path(cx);
                pane.update(cx, |pane, _| {
                    let data = pane
                        .project_item_restoration_data
                        .entry(kind)
                        .or_insert_with(|| Box::new(EditorRestorationData::default()) as Box<_>);
                    let data = match data.downcast_mut::<EditorRestorationData>() {
                        Some(data) => data,
                        None => {
                            *data = Box::new(EditorRestorationData::default());
                            data.downcast_mut::<EditorRestorationData>()
                                .expect("just written the type downcasted to")
                        }
                    };

                    let data = data.entries.entry(file_abs_path).or_default();
                    write(data);
                    Some(())
                })
            });
        });
    }
}

// Replace-all commonly expands several hits against the same line.
#[derive(Default)]
struct SearchHitContext {
    row: Option<u32>,
    text: String,
}

impl SearchHitContext {
    fn for_hit(
        &mut self,
        snapshot: &MultiBufferSnapshot,
        hit: &Range<Anchor>,
    ) -> (&str, Range<usize>) {
        let start = hit.start.to_point(snapshot);
        let end = hit.end.to_point(snapshot);
        let range = if start.row == end.row {
            if self.row != Some(start.row) {
                self.text.clear();
                self.text.extend(snapshot.text_for_range(
                    Point::new(start.row, 0)
                        ..Point::new(start.row, snapshot.line_len(MultiBufferRow(start.row))),
                ));
                self.row = Some(start.row);
            }
            start.column as usize..end.column as usize
        } else {
            self.row = None;
            self.text.clear();
            self.text.extend(snapshot.text_for_range(start..end));
            0..self.text.len()
        };
        (&self.text, range)
    }
}

impl SearchableItem for Editor {
    type Match = Range<Anchor>;

    fn get_matches(&self, _window: &mut Window, _: &mut App) -> (Vec<Range<Anchor>>, SearchToken) {
        (
            self.background_highlights
                .get(&HighlightKey::BufferSearchHighlights)
                .map_or(Vec::new(), |(_color, ranges)| {
                    ranges.iter().cloned().collect()
                }),
            SearchToken::default(),
        )
    }

    fn clear_matches(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self
            .clear_background_highlights(HighlightKey::BufferSearchHighlights, cx)
            .is_some()
        {
            cx.emit(SearchEvent::MatchesInvalidated);
        }
    }

    fn update_matches(
        &mut self,
        matches: &[Range<Anchor>],
        active_match_index: Option<usize>,
        _token: SearchToken,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing_range = self
            .background_highlights
            .get(&HighlightKey::BufferSearchHighlights)
            .map(|(_, range)| range.as_ref());
        let updated = existing_range != Some(matches);
        self.highlight_background(
            HighlightKey::BufferSearchHighlights,
            matches,
            move |index, theme| {
                if active_match_index == Some(*index) {
                    theme.colors().search_active_match_background
                } else {
                    theme.colors().search_match_background
                }
            },
            cx,
        );
        if updated {
            cx.emit(SearchEvent::MatchesInvalidated);
        }
    }

    fn has_filtered_search_ranges(&mut self) -> bool {
        self.has_background_highlights(HighlightKey::SearchWithinRange)
    }

    fn toggle_filtered_search_ranges(
        &mut self,
        enabled: Option<FilteredSearchRange>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_filtered_search_ranges() {
            self.previous_search_ranges = self
                .clear_background_highlights(HighlightKey::SearchWithinRange, cx)
                .map(|(_, ranges)| ranges)
        }

        if let Some(range) = enabled {
            let ranges = self.selections.disjoint_anchor_ranges().collect::<Vec<_>>();

            if ranges.iter().any(|s| s.start != s.end) {
                self.set_search_within_ranges(&ranges, cx);
            } else if let Some(previous_search_ranges) = self.previous_search_ranges.take()
                && range != FilteredSearchRange::Selection
            {
                self.set_search_within_ranges(&previous_search_ranges, cx);
            }
        }
    }

    fn supported_options(&self) -> SearchOptions {
        if self.in_project_search {
            SearchOptions {
                case: true,
                word: true,
                regex: true,
                replacement: false,
                selection: false,
                select_all: true,
                find_in_results: true,
            }
        } else {
            SearchOptions {
                case: true,
                word: true,
                regex: true,
                replacement: true,
                selection: true,
                select_all: true,
                find_in_results: false,
            }
        }
    }

    fn query_suggestion(
        &mut self,
        seed_query_override: Option<SeedQuerySetting>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> String {
        let setting = seed_query_override
            .unwrap_or_else(|| EditorSettings::get_global(cx).seed_search_query_from_cursor);
        let snapshot = self.snapshot(window, cx);
        let selection = self.selections.newest_adjusted(&snapshot.display_snapshot);
        let buffer_snapshot = snapshot.buffer_snapshot();

        match setting {
            SeedQuerySetting::Never => String::new(),
            SeedQuerySetting::Selection | SeedQuerySetting::Always if !selection.is_empty() => {
                buffer_snapshot
                    .text_for_range(selection.start..selection.end)
                    .collect()
            }
            SeedQuerySetting::Selection => String::new(),
            SeedQuerySetting::Always => {
                let (range, kind) = buffer_snapshot
                    .surrounding_word(selection.start, Some(CharScopeContext::Completion));
                if kind == Some(CharKind::Word) {
                    let text: String = buffer_snapshot.text_for_range(range).collect();
                    if !text.trim().is_empty() {
                        return text;
                    }
                }
                String::new()
            }
        }
    }

    fn activate_match(
        &mut self,
        index: usize,
        matches: &[Range<Anchor>],
        _token: SearchToken,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.unfold_ranges(&[matches[index].clone()], false, true, cx);
        let range = self.range_for_match(&matches[index]);
        let autoscroll = if EditorSettings::get_global(cx).search.center_on_match {
            Autoscroll::center()
        } else {
            Autoscroll::fit()
        };
        self.change_selections(
            SelectionEffects::scroll(autoscroll).from_search(true),
            window,
            cx,
            |s| {
                s.select_ranges([range]);
            },
        )
    }

    fn select_matches(
        &mut self,
        matches: &[Self::Match],
        _token: SearchToken,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.unfold_ranges(matches, false, false, cx);
        self.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
            s.select_ranges(matches.iter().cloned())
        });
    }
    fn replace(
        &mut self,
        identifier: &Self::Match,
        query: &SearchQuery,
        _token: SearchToken,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let replacement = if query.replacement_requires_context() {
            let snapshot = self.buffer.read(cx).snapshot(cx);
            let mut context = SearchHitContext::default();
            let (line, hit) = context.for_hit(&snapshot, identifier);
            query
                .replacement_for(line, hit)
                .map(|replacement| Arc::<str>::from(&*replacement))
        } else {
            query.replacement().map(Arc::<str>::from)
        };

        if let Some(replacement) = replacement {
            self.transact(window, cx, |this, _, cx| {
                this.edit([(identifier.clone(), replacement)], cx);
            });
        }
    }
    fn replace_all(
        &mut self,
        matches: &mut dyn Iterator<Item = &Self::Match>,
        query: &SearchQuery,
        _token: SearchToken,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let mut edits = vec![];

        // A regex might have replacement variables so we cannot apply
        // the same replacement to all matches
        if query.replacement_requires_context() {
            let mut context = SearchHitContext::default();
            edits = matches
                .filter_map(|m| {
                    let (line, hit) = context.for_hit(&snapshot, m);
                    query
                        .replacement_for(line, hit)
                        .map(|replacement| (m.clone(), Arc::from(&*replacement)))
                })
                .collect();
        } else if let Some(replacement) = query.replacement().map(Arc::<str>::from) {
            edits = matches.map(|m| (m.clone(), replacement.clone())).collect();
        }

        if !edits.is_empty() {
            self.transact(window, cx, |this, _, cx| {
                this.edit(edits, cx);
            });
        }
    }

    /// Takes the current cursor position and finds the next match in the
    /// provided `direction`, the provide `count` number of times, wrapping
    /// around if necessary.
    fn match_index_for_direction(
        &mut self,
        matches: &[Range<Anchor>],
        current_index: usize,
        direction: Direction,
        count: usize,
        _token: SearchToken,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        if count == 0 {
            return current_index;
        }

        let cursor = if self.selections.disjoint_anchors_arc().len() == 1 {
            self.selections.newest_anchor().head()
        } else {
            matches[current_index].start
        };

        let buffer = self.buffer().read(cx).snapshot(cx);
        let new_idx = match direction {
            Direction::Next => matches
                .iter()
                .position(|m| m.start.cmp(&cursor, &buffer).is_gt())
                .unwrap_or(0),
            Direction::Prev => matches
                .iter()
                .rposition(|m| m.end.cmp(&cursor, &buffer).is_lt())
                .unwrap_or(matches.len() - 1),
        } as isize;

        // We'll use `count - 1` because the first jump to the next or previous
        // match already happens in the scenario above, when we find the next or
        // previous match starting from the cursor position.
        let count = count.saturating_sub(1);
        let count = match direction {
            Direction::Prev => -(count as isize),
            Direction::Next => count as isize,
        };

        let new_idx = (new_idx + count) % matches.len() as isize;
        let new_idx = if new_idx.is_negative() {
            // We need a `matches.len() - 1` here in case `next_idx` has now been
            // set to `0`, otherwise we'd end up returning `matches.len()`, which
            // would be out of bounds.
            new_idx + (matches.len() - 1) as isize
        } else {
            new_idx
        };
        assert!(new_idx < matches.len() as isize);
        new_idx as usize
    }

    fn find_matches(
        &mut self,
        query: Arc<project::search::SearchQuery>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Vec<Range<Anchor>>> {
        let buffer = self.buffer().read(cx).snapshot(cx);
        let search_within_ranges = self
            .background_highlights
            .get(&HighlightKey::SearchWithinRange)
            .map_or(vec![], |(_color, ranges)| {
                ranges.iter().cloned().collect::<Vec<_>>()
            });

        let executor = cx.background_executor().clone();
        cx.background_spawn(async move {
            let mut ranges = Vec::new();

            let search_within_ranges = if search_within_ranges.is_empty() {
                vec![buffer.anchor_before(MultiBufferOffset(0))..buffer.anchor_after(buffer.len())]
            } else {
                search_within_ranges
            };
            let num_cpus = executor.num_cpus();
            for range in search_within_ranges {
                for (search_buffer, search_range, deleted_hunk_anchor) in
                    buffer.range_to_buffer_ranges_with_deleted_hunks(range)
                {
                    let query = query.clone();

                    let mut results = Vec::new();
                    executor
                        .scoped(|scope| {
                            for search_range in chunk_search_range(
                                search_buffer.text.clone(),
                                &query,
                                num_cpus as u32,
                                search_range,
                            ) {
                                let query = query.clone();
                                let buffer = buffer.clone();

                                let (tx, rx) = oneshot::channel();
                                results.push(rx);
                                scope.spawn(async move {
                                    let chunk_result = query
                                        .search(
                                            search_buffer,
                                            Some(search_range.start..search_range.end),
                                        )
                                        .await
                                        .into_iter()
                                        .filter_map(|match_range| {
                                            if let Some(deleted_hunk_anchor) = deleted_hunk_anchor {
                                                let start = search_buffer.anchor_after(
                                                    search_range.start + match_range.start,
                                                );
                                                let end = search_buffer.anchor_before(
                                                    search_range.start + match_range.end,
                                                );
                                                Some(
                                                    deleted_hunk_anchor.with_diff_base_anchor(start)
                                                        ..deleted_hunk_anchor
                                                            .with_diff_base_anchor(end),
                                                )
                                            } else {
                                                let start = search_buffer.anchor_after(
                                                    search_range.start + match_range.start,
                                                );
                                                let end = search_buffer.anchor_before(
                                                    search_range.start + match_range.end,
                                                );
                                                buffer.anchor_range_in_buffer(start..end)
                                            }
                                        })
                                        .collect::<Vec<_>>();
                                    _ = tx.send(chunk_result);
                                });
                            }
                        })
                        .await;

                    for rx in results {
                        if let Ok(results) = rx.await {
                            ranges.extend(results);
                        }
                    }
                }
            }

            ranges
        })
    }

    fn active_match_index(
        &mut self,
        direction: Direction,
        matches: &[Range<Anchor>],
        _token: SearchToken,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        active_match_index(
            direction,
            matches,
            &self.selections.newest_anchor().head(),
            &self.buffer().read(cx).snapshot(cx),
        )
    }

    fn search_bar_visibility_changed(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) {
        self.expect_bounds_change = self.last_bounds;
    }

    fn set_search_is_case_sensitive(
        &mut self,
        case_sensitive: Option<bool>,
        _cx: &mut Context<Self>,
    ) {
        self.select_next_is_case_sensitive = case_sensitive;
    }
}

pub fn active_match_index(
    direction: Direction,
    ranges: &[Range<Anchor>],
    cursor: &Anchor,
    buffer: &MultiBufferSnapshot,
) -> Option<usize> {
    if ranges.is_empty() {
        None
    } else {
        let r = ranges.binary_search_by(|probe| {
            if probe.end.cmp(cursor, buffer).is_lt() {
                Ordering::Less
            } else if probe.start.cmp(cursor, buffer).is_gt() {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        });
        match direction {
            Direction::Prev => match r {
                Ok(i) => Some(i),
                Err(i) => Some(i.saturating_sub(1)),
            },
            Direction::Next => match r {
                Ok(i) | Err(i) => Some(cmp::min(i, ranges.len() - 1)),
            },
        }
    }
}

/// Opens a path-like target (e.g. `items.rs:100:5`) in the workspace, moving the cursor
/// to the one-based row/column if present. Returns whether the target was opened.
pub async fn open_resolved_target(
    workspace: &WeakEntity<Workspace>,
    open_target: &workspace::path_link::OpenTarget,
    cx: &mut AsyncWindowContext,
) -> Result<bool> {
    let path_to_open = open_target.path();
    let mut opened_items = workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_paths(
                vec![path_to_open.path.clone()],
                workspace::OpenOptions {
                    visible: Some(workspace::OpenVisible::OnlyDirectories),
                    ..Default::default()
                },
                None,
                window,
                cx,
            )
        })
        .context("workspace update")?
        .await;
    if opened_items.len() != 1 {
        debug_panic!(
            "Received {} items for one path {path_to_open:?}",
            opened_items.len(),
        );
    }
    let Some(opened_item) = opened_items.pop() else {
        return Ok(false);
    };

    if open_target.is_file() {
        let Some(opened_item) = opened_item else {
            return Ok(false);
        };
        let opened_item =
            opened_item.with_context(|| format!("opening {:?}", path_to_open.path))?;
        if let Some(row) = path_to_open.row
            && let Some(editor) = opened_item.downcast::<Editor>()
        {
            let column = path_to_open.column.unwrap_or(0);
            editor
                .downgrade()
                .update_in(cx, |editor, window, cx| {
                    if let Some(buffer) = editor.buffer().read(cx).as_singleton() {
                        let point = buffer.read(cx).snapshot().point_from_external_input(
                            row.saturating_sub(1),
                            column.saturating_sub(1),
                        );
                        editor.go_to_singleton_buffer_point(point, window, cx);
                    }
                })
                .log_err();
        }
        Ok(true)
    } else if open_target.is_dir() {
        workspace.update(cx, |workspace, cx| {
            workspace.project().update(cx, |_, cx| {
                cx.emit(project::Event::ActivateProjectPanel);
            })
        })?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn entry_label_color(selected: bool) -> Color {
    if selected {
        Color::Default
    } else {
        Color::Muted
    }
}

pub fn entry_diagnostic_aware_icon_name_and_color(
    diagnostic_severity: Option<DiagnosticSeverity>,
) -> Option<(IconName, Color)> {
    match diagnostic_severity {
        Some(DiagnosticSeverity::ERROR) => Some((IconName::Close, Color::Error)),
        Some(DiagnosticSeverity::WARNING) => Some((IconName::Triangle, Color::Warning)),
        _ => None,
    }
}

pub fn entry_diagnostic_aware_icon_decoration_and_color(
    diagnostic_severity: Option<DiagnosticSeverity>,
) -> Option<(IconDecorationKind, Color)> {
    match diagnostic_severity {
        Some(DiagnosticSeverity::ERROR) => Some((IconDecorationKind::X, Color::Error)),
        Some(DiagnosticSeverity::WARNING) => Some((IconDecorationKind::Triangle, Color::Warning)),
        _ => None,
    }
}

pub fn entry_git_aware_label_color(git_status: GitSummary, ignored: bool, selected: bool) -> Color {
    let tracked = git_status.index + git_status.worktree;
    if git_status.conflict > 0 {
        Color::Conflict
    } else if tracked.deleted > 0 {
        Color::Deleted
    } else if tracked.modified > 0 {
        Color::Modified
    } else if tracked.added > 0 || git_status.untracked > 0 {
        Color::Created
    } else if ignored {
        Color::Ignored
    } else {
        entry_label_color(selected)
    }
}

fn path_for_buffer<'a>(
    buffer: &Entity<MultiBuffer>,
    height: usize,
    include_filename: bool,
    cx: &'a App,
) -> Option<Cow<'a, str>> {
    let file = buffer.read(cx).as_singleton()?.read(cx).file()?;
    path_for_file(file, height, include_filename, cx)
}

fn path_for_file<'a>(
    file: &'a Arc<dyn language::File>,
    mut height: usize,
    include_filename: bool,
    cx: &'a App,
) -> Option<Cow<'a, str>> {
    if project::File::from_dyn(Some(file)).is_none() {
        return None;
    }

    let file = file.as_ref();
    // Ensure we always render at least the filename.
    height += 1;

    let mut prefix = file.path().as_ref();
    while height > 0 {
        if let Some(parent) = prefix.parent() {
            prefix = parent;
            height -= 1;
        } else {
            break;
        }
    }

    // The full_path method allocates, so avoid calling it if height is zero.
    if height > 0 {
        let mut full_path = file.full_path(cx);
        if !include_filename {
            if !full_path.pop() {
                return None;
            }
        }
        Some(full_path.to_string_lossy().into_owned().into())
    } else {
        let mut path = file.path().strip_prefix(prefix).ok()?;
        if !include_filename {
            path = path.parent()?;
        }
        Some(path.display(file.path_style(cx)))
    }
}

/// Restores serialized buffer contents by overwriting the buffer with saved text.
/// This is somewhat wasteful since we load the whole buffer from disk then overwrite it,
/// but keeps implementation simple as we don't need to persist all metadata from loading
/// (git diff base, etc.).
fn restore_serialized_buffer_contents(
    buffer: &mut Buffer,
    contents: String,
    mtime: Option<MTime>,
    cx: &mut Context<Buffer>,
) {
    // If we did restore an mtime, store it on the buffer so that
    // the next edit will mark the buffer as dirty/conflicted.
    if mtime.is_some() {
        buffer.did_reload(buffer.version(), buffer.line_ending(), mtime, cx);
    }
    buffer.set_text(contents, cx);
    if let Some(entry) = buffer.peek_undo_stack() {
        buffer.forget_transaction(entry.transaction_id());
    }
}

fn chunk_search_range(
    buffer: BufferSnapshot,
    query: &SearchQuery,
    num_cpus: u32,
    initial_range: Range<BufferOffset>,
) -> Box<dyn Iterator<Item = Range<usize>> + 'static> {
    let range = initial_range.to_offset(&buffer);
    if range.is_empty() {
        return Box::new(std::iter::empty());
    }

    let summary: TextSummary = buffer.text_summary_for_range(initial_range);
    let num_chunks = if !query.is_regex() && !query.as_str().contains('\n') {
        NonZeroU32::new(summary.lines.row.saturating_add(1).min(num_cpus.max(1)))
    } else {
        NonZeroU32::new(1)
    };

    let Some(num_chunks) = num_chunks else {
        return Box::new(std::iter::empty());
    };

    let mut chunk_start = range.start;
    let rope = buffer.as_rope().clone();
    let range_end = range.end;
    let average_chunk_length = summary.len.div_ceil(num_chunks.get() as usize);
    Box::new(std::iter::from_fn(move || {
        if chunk_start >= range_end {
            return None;
        }
        let candidate_position = chunk_start + average_chunk_length;
        let adjusted = rope.ceil_char_boundary(candidate_position);
        let mut as_point = rope.offset_to_point(adjusted);
        as_point.row += 1;
        as_point.column = 0;
        let end_offset = buffer.point_to_offset(as_point).min(range_end);
        let ret = chunk_start..end_offset;
        chunk_start = end_offset;
        Some(ret)
    }))
}

/// Decides what to format based on the `format_on_save` settings of the saved buffers.
///
/// In the modifications modes, only lines with unstaged changes are formatted.
/// When no git diff is available for a buffer, `modifications` skips formatting while `modifications_if_available`
/// falls back to formatting entire buffers.
/// When a diff is available but empty, nothing is formatted in either mode.
fn compute_format_target(
    buffers: &HashSet<Entity<Buffer>>,
    trigger: FormatTrigger,
    multi_buffer: &Entity<MultiBuffer>,
    git_store: &Entity<GitStore>,
    cx: &App,
) -> Option<FormatTarget> {
    if trigger == FormatTrigger::Manual {
        return Some(FormatTarget::Buffers(buffers.clone()));
    }

    let multi_buffer_snapshot = multi_buffer.read(cx).snapshot(cx);
    let git_store = git_store.read(cx);

    let mut fall_back_to_full_format = false;
    let mut modified_ranges: Vec<Range<Point>> = Vec::new();

    for buffer_entity in buffers.iter() {
        let buffer = buffer_entity.read(cx);
        let settings = LanguageSettings::for_buffer(buffer, cx);
        match settings.format_on_save {
            FormatOnSave::On | FormatOnSave::Off => {
                return Some(FormatTarget::Buffers(buffers.clone()));
            }
            FormatOnSave::Modifications | FormatOnSave::ModificationsIfAvailable => {}
        }

        let Some(diff_snapshot) = git_store
            .get_unstaged_diff(buffer.remote_id(), cx)
            .map(|diff| diff.read(cx).snapshot(cx))
        else {
            if settings.format_on_save == FormatOnSave::ModificationsIfAvailable {
                fall_back_to_full_format = true;
            }
            continue;
        };

        let anchor_ranges = compute_modified_ranges(&buffer.snapshot(), &diff_snapshot);
        let flat_anchors = anchor_ranges
            .iter()
            .flat_map(|range| [range.start, range.end])
            .collect::<Vec<_>>();
        let multi_buffer_anchors =
            multi_buffer_snapshot.text_anchors_to_visible_anchors(flat_anchors);
        for pair in multi_buffer_anchors.chunks_exact(2) {
            let (Some(start), Some(end)) = (&pair[0], &pair[1]) else {
                continue;
            };
            modified_ranges
                .push(start.to_point(&multi_buffer_snapshot)..end.to_point(&multi_buffer_snapshot));
        }
    }

    if fall_back_to_full_format {
        Some(FormatTarget::Buffers(buffers.clone()))
    } else if modified_ranges.is_empty() {
        None
    } else {
        Some(FormatTarget::Ranges(modified_ranges))
    }
}

/// Computes the buffer ranges that have unstaged changes, expanded to full lines and
/// with adjacent hunks merged, for use with format-on-save. An empty result means the
/// buffer has no formatable modifications.
fn compute_modified_ranges(
    buffer_snapshot: &language::BufferSnapshot,
    diff_snapshot: &buffer_diff::BufferDiffSnapshot,
) -> Vec<Range<text::Anchor>> {
    let mut merged: Vec<Range<text::Anchor>> = Vec::new();
    for hunk in diff_snapshot.hunks(buffer_snapshot) {
        let range = hunk.buffer_range;
        if range.start.cmp(&range.end, buffer_snapshot).is_eq() {
            // Deletion-only hunks produce no buffer content to format.
            continue;
        }
        let start_point = range.start.to_point(buffer_snapshot);
        let end_point = range.end.to_point(buffer_snapshot);
        let start_row = start_point.row;
        let end_row = if end_point.column == 0 && end_point.row > start_point.row {
            end_point.row - 1
        } else {
            end_point.row
        };
        let line_start = text::Point::new(start_row, 0);
        let line_end = text::Point::new(end_row, buffer_snapshot.line_len(end_row));
        let expanded =
            buffer_snapshot.anchor_before(line_start)..buffer_snapshot.anchor_after(line_end);

        if let Some(last) = merged.last_mut() {
            let last_end_point = last.end.to_point(buffer_snapshot);
            if start_row <= last_end_point.row + 1 {
                if expanded.end.to_point(buffer_snapshot) > last_end_point {
                    last.end = expanded.end;
                }
                continue;
            }
        }
        merged.push(expanded);
    }
    merged
}

#[cfg(test)]
mod tests {
    use crate::editor_tests::init_test;
    use fs::Fs;
    use workspace::MultiWorkspace;

    use super::*;
    use fs::MTime;
    use gpui::{App, VisualTestContext};
    use language::TestFile;
    use multi_buffer::PathKey;
    use project::FakeFs;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use util::{path, paths::PathWithPosition, rel_path::RelPath};
    use workspace::path_link::{OpenTarget, OpenTargetFoundBy};

    #[gpui::test]
    fn test_path_for_file(cx: &mut App) {
        let file: Arc<dyn language::File> = Arc::new(TestFile {
            path: RelPath::empty_arc(),
            root_name: String::new(),
            local_root: None,
        });
        assert_eq!(path_for_file(&file, 0, false, cx), None);
    }

    #[gpui::test]
    fn test_chunk_search_range_multi_line(cx: &mut App) {
        let text = "line one\nline two\nline three\nline four\nline five\nline six\n";
        let buffer = cx.new(|cx| Buffer::local(text, cx));
        let snapshot = buffer.read(cx).snapshot();

        let chunks = chunk_search_range_for_test(&snapshot, "line", 4, 0..text.len());

        assert_chunks_are_contiguous(&chunks, 0..text.len());
        assert!(
            chunks.len() <= 4,
            "got {} chunks, expected <= num_cpus (4)",
            chunks.len()
        );
        for chunk in &chunks {
            let end = chunk.end;
            assert!(
                end == text.len() || text.as_bytes()[end - 1] == b'\n',
                "chunk ending at {end} is not a line boundary",
            );
        }
    }

    #[gpui::test]
    fn test_chunk_search_range_single_line(cx: &mut App) {
        let text = "hello world hello again";
        let buffer = cx.new(|cx| Buffer::local(text, cx));
        let snapshot = buffer.read(cx).snapshot();

        let chunks = chunk_search_range_for_test(&snapshot, "hello", 4, 0..text.len());
        assert_chunks_are_contiguous(&chunks, 0..text.len());
    }

    #[gpui::test]
    fn test_chunk_search_range_empty_range(cx: &mut App) {
        let buffer = cx.new(|cx| Buffer::local("hello world", cx));
        let snapshot = buffer.read(cx).snapshot();

        let chunks = chunk_search_range_for_test(&snapshot, "hello", 4, 5..5);
        assert!(chunks.is_empty());
    }

    #[gpui::test]
    fn test_chunk_search_range_does_not_start_at_zero(cx: &mut App) {
        let line = "abcdefghij\n";
        let text = line.repeat(20);
        let buffer = cx.new(|cx| Buffer::local(text.clone(), cx));
        let snapshot = buffer.read(cx).snapshot();

        let start = line.len() * 7;
        let end = line.len() * 14;
        let chunks = chunk_search_range_for_test(&snapshot, "abc", 4, start..end);

        assert_chunks_are_contiguous(&chunks, start..end);
    }

    fn chunk_search_range_for_test(
        snapshot: &language::BufferSnapshot,
        query: &str,
        num_cpus: u32,
        range: Range<usize>,
    ) -> Vec<Range<usize>> {
        let query = SearchQuery::text(
            query,
            false,
            false,
            false,
            Default::default(),
            Default::default(),
            false,
            None,
        )
        .unwrap();
        chunk_search_range(
            snapshot.text.clone(),
            &query,
            num_cpus,
            BufferOffset(range.start)..BufferOffset(range.end),
        )
        .collect()
    }

    #[track_caller]
    fn assert_chunks_are_contiguous(chunks: &[Range<usize>], expected: Range<usize>) {
        assert!(!chunks.is_empty(), "expected at least one chunk");
        assert_eq!(
            chunks.first().unwrap().start,
            expected.start,
            "first chunk does not start at {}",
            expected.start
        );
        assert_eq!(
            chunks.last().unwrap().end,
            expected.end,
            "last chunk does not end at {}",
            expected.end
        );
        for chunk in chunks {
            assert!(chunk.start < chunk.end, "empty chunk: {:?}", chunk);
        }
        for window in chunks.windows(2) {
            assert_eq!(
                window[0].end, window[1].start,
                "gap or overlap between chunks {:?} and {:?}",
                window[0], window[1],
            );
        }
    }

    #[gpui::test]
    async fn test_suggested_filename_uses_language_extension_for_untitled_buffer(
        cx: &mut gpui::TestAppContext,
    ) {
        init_test(cx, |_| {});

        let buffer = cx.update(|cx| {
            cx.new(|cx| Buffer::local("", cx).with_language(languages::rust_lang(), cx))
        });
        let (editor, cx) =
            cx.add_window_view(|window, cx| Editor::for_buffer(buffer, None, window, cx));

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.suggested_filename(cx).as_ref(), "untitled.rs");
        });
    }

    #[gpui::test]
    async fn test_suggested_filename_appends_extension_to_content_title(
        cx: &mut gpui::TestAppContext,
    ) {
        init_test(cx, |_| {});

        let buffer = cx.update(|cx| {
            cx.new(|cx| {
                Buffer::local("sadsdsads\nmore text", cx).with_language(languages::rust_lang(), cx)
            })
        });
        let (editor, cx) =
            cx.add_window_view(|window, cx| Editor::for_buffer(buffer, None, window, cx));

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.tab_content_text(0, cx).as_ref(), "sadsdsads");
            assert_eq!(editor.suggested_filename(cx).as_ref(), "sadsdsads.rs");
        });
    }

    #[gpui::test]
    async fn test_suggested_filename_does_not_duplicate_extension(cx: &mut gpui::TestAppContext) {
        init_test(cx, |_| {});

        let buffer = cx.update(|cx| {
            cx.new(|cx| {
                Buffer::local("main.rs\nfn main() {}", cx).with_language(languages::rust_lang(), cx)
            })
        });
        let (editor, cx) =
            cx.add_window_view(|window, cx| Editor::for_buffer(buffer, None, window, cx));

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.suggested_filename(cx).as_ref(), "main.rs");
        });
    }

    #[gpui::test]
    async fn test_suggested_filename_keeps_content_title_for_plain_text(
        cx: &mut gpui::TestAppContext,
    ) {
        init_test(cx, |_| {});

        let buffer = cx.update(|cx| {
            cx.new(|cx| {
                Buffer::local("shopping list\nmilk", cx)
                    .with_language(language::PLAIN_TEXT.clone(), cx)
            })
        });
        let (editor, cx) =
            cx.add_window_view(|window, cx| Editor::for_buffer(buffer, None, window, cx));

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.suggested_filename(cx).as_ref(), "shopping list");
        });
    }

    #[gpui::test]
    async fn test_suggested_filename_keeps_content_title_without_language(
        cx: &mut gpui::TestAppContext,
    ) {
        init_test(cx, |_| {});

        let buffer = cx.update(|cx| cx.new(|cx| Buffer::local("shopping list\nmilk", cx)));
        let (editor, cx) =
            cx.add_window_view(|window, cx| Editor::for_buffer(buffer, None, window, cx));

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.suggested_filename(cx).as_ref(), "shopping list");
        });
    }

    async fn deserialize_editor(
        item_id: ItemId,
        workspace_id: WorkspaceId,
        workspace: Entity<Workspace>,
        project: Entity<Project>,
        cx: &mut VisualTestContext,
    ) -> Entity<Editor> {
        workspace
            .update_in(cx, |workspace, window, cx| {
                let pane = workspace.active_pane();
                pane.update(cx, |_, cx| {
                    Editor::deserialize(
                        project.clone(),
                        workspace.weak_handle(),
                        workspace_id,
                        item_id,
                        window,
                        cx,
                    )
                })
            })
            .await
            .unwrap()
    }

    #[gpui::test]
    async fn test_deserialize(cx: &mut gpui::TestAppContext) {
        init_test(cx, |_| {});

        let fs = FakeFs::new(cx.executor());
        fs.insert_file(path!("/file.rs"), Default::default()).await;

        // Test case 1: Deserialize with path and contents
        {
            let project = Project::test(fs.clone(), [path!("/file.rs").as_ref()], cx).await;
            let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
                MultiWorkspace::test_new(project.clone(), window, cx)
            });
            let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
            let db = cx.update(|_, cx| workspace::WorkspaceDb::global(cx));
            let workspace_id = db.next_id().await.unwrap();
            let editor_db = cx.update(|_, cx| EditorDb::global(cx));
            let item_id = 1234 as ItemId;
            let mtime = fs
                .metadata(Path::new(path!("/file.rs")))
                .await
                .unwrap()
                .unwrap()
                .mtime;

            let serialized_editor = SerializedEditor {
                abs_path: Some(PathBuf::from(path!("/file.rs"))),
                contents: Some("fn main() {}".to_string()),
                language: Some("Rust".to_string()),
                mtime: Some(mtime),
            };

            editor_db
                .save_serialized_editor(item_id, workspace_id, serialized_editor.clone())
                .await
                .unwrap();

            let deserialized =
                deserialize_editor(item_id, workspace_id, workspace, project, cx).await;

            deserialized.update(cx, |editor, cx| {
                assert_eq!(editor.text(cx), "fn main() {}");
                assert!(editor.is_dirty(cx));
                assert!(!editor.has_conflict(cx));
                let buffer = editor.buffer().read(cx).as_singleton().unwrap().read(cx);
                assert!(buffer.file().is_some());
            });
        }

        // Test case 2: Deserialize with only path
        {
            let project = Project::test(fs.clone(), [path!("/file.rs").as_ref()], cx).await;
            let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
                MultiWorkspace::test_new(project.clone(), window, cx)
            });
            let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
            let db = cx.update(|_, cx| workspace::WorkspaceDb::global(cx));
            let editor_db = cx.update(|_, cx| EditorDb::global(cx));

            let workspace_id = db.next_id().await.unwrap();

            let item_id = 5678 as ItemId;
            let serialized_editor = SerializedEditor {
                abs_path: Some(PathBuf::from(path!("/file.rs"))),
                contents: None,
                language: None,
                mtime: None,
            };

            editor_db
                .save_serialized_editor(item_id, workspace_id, serialized_editor)
                .await
                .unwrap();

            let deserialized =
                deserialize_editor(item_id, workspace_id, workspace, project, cx).await;

            deserialized.update(cx, |editor, cx| {
                assert_eq!(editor.text(cx), ""); // The file should be empty as per our initial setup
                assert!(!editor.is_dirty(cx));
                assert!(!editor.has_conflict(cx));

                let buffer = editor.buffer().read(cx).as_singleton().unwrap().read(cx);
                assert!(buffer.file().is_some());
            });
        }

        // Test case 3: Deserialize with no path (untitled buffer, with content and language)
        {
            let project = Project::test(fs.clone(), [path!("/file.rs").as_ref()], cx).await;
            // Add Rust to the language, so that we can restore the language of the buffer
            project.read_with(cx, |project, _| {
                project.languages().add(languages::rust_lang())
            });

            let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
                MultiWorkspace::test_new(project.clone(), window, cx)
            });
            let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
            let db = cx.update(|_, cx| workspace::WorkspaceDb::global(cx));
            let editor_db = cx.update(|_, cx| EditorDb::global(cx));

            let workspace_id = db.next_id().await.unwrap();

            let item_id = 9012 as ItemId;
            let serialized_editor = SerializedEditor {
                abs_path: None,
                contents: Some("hello".to_string()),
                language: Some("Rust".to_string()),
                mtime: None,
            };

            editor_db
                .save_serialized_editor(item_id, workspace_id, serialized_editor)
                .await
                .unwrap();

            let deserialized =
                deserialize_editor(item_id, workspace_id, workspace, project, cx).await;

            deserialized.update(cx, |editor, cx| {
                assert_eq!(editor.text(cx), "hello");
                assert!(editor.is_dirty(cx)); // The editor should be dirty for an untitled buffer

                let buffer = editor.buffer().read(cx).as_singleton().unwrap().read(cx);
                assert_eq!(
                    buffer.language().map(|lang| lang.name()),
                    Some("Rust".into())
                ); // Language should be set to Rust
                assert!(buffer.file().is_none()); // The buffer should not have an associated file
            });
        }

        // Test case 4: Deserialize with path, content, and old mtime
        {
            let project = Project::test(fs.clone(), [path!("/file.rs").as_ref()], cx).await;
            let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
                MultiWorkspace::test_new(project.clone(), window, cx)
            });
            let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
            let db = cx.update(|_, cx| workspace::WorkspaceDb::global(cx));
            let editor_db = cx.update(|_, cx| EditorDb::global(cx));

            let workspace_id = db.next_id().await.unwrap();

            let item_id = 9345 as ItemId;
            let old_mtime = MTime::from_seconds_and_nanos(0, 50);
            let serialized_editor = SerializedEditor {
                abs_path: Some(PathBuf::from(path!("/file.rs"))),
                contents: Some("fn main() {}".to_string()),
                language: Some("Rust".to_string()),
                mtime: Some(old_mtime),
            };

            editor_db
                .save_serialized_editor(item_id, workspace_id, serialized_editor)
                .await
                .unwrap();

            let deserialized =
                deserialize_editor(item_id, workspace_id, workspace, project, cx).await;

            deserialized.update(cx, |editor, cx| {
                assert_eq!(editor.text(cx), "fn main() {}");
                assert!(editor.has_conflict(cx)); // The editor should have a conflict
            });
        }

        // Test case 5: Deserialize with no path, no content, no language, and no old mtime (new, empty, unsaved buffer)
        {
            let project = Project::test(fs.clone(), [path!("/file.rs").as_ref()], cx).await;
            let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
                MultiWorkspace::test_new(project.clone(), window, cx)
            });
            let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
            let db = cx.update(|_, cx| workspace::WorkspaceDb::global(cx));
            let editor_db = cx.update(|_, cx| EditorDb::global(cx));

            let workspace_id = db.next_id().await.unwrap();

            let item_id = 10000 as ItemId;
            let serialized_editor = SerializedEditor {
                abs_path: None,
                contents: None,
                language: None,
                mtime: None,
            };

            editor_db
                .save_serialized_editor(item_id, workspace_id, serialized_editor)
                .await
                .unwrap();

            let deserialized =
                deserialize_editor(item_id, workspace_id, workspace, project, cx).await;

            deserialized.update(cx, |editor, cx| {
                assert_eq!(editor.text(cx), "");
                assert!(!editor.is_dirty(cx));
                assert!(!editor.has_conflict(cx));

                let buffer = editor.buffer().read(cx).as_singleton().unwrap().read(cx);
                assert!(buffer.file().is_none());
            });
        }

        // Test case 6: Deserialize with path and contents in an empty workspace (no worktree)
        // This tests the hot-exit scenario where a file is opened in an empty workspace
        // and has unsaved changes that should be restored.
        {
            let fs = FakeFs::new(cx.executor());
            fs.insert_file(path!("/standalone.rs"), "original content".into())
                .await;

            // Create an empty project with no worktrees
            let project = Project::test(fs.clone(), [], cx).await;
            let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
                MultiWorkspace::test_new(project.clone(), window, cx)
            });
            let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
            let db = cx.update(|_, cx| workspace::WorkspaceDb::global(cx));
            let editor_db = cx.update(|_, cx| EditorDb::global(cx));

            let workspace_id = db.next_id().await.unwrap();
            let item_id = 11000 as ItemId;

            let mtime = fs
                .metadata(Path::new(path!("/standalone.rs")))
                .await
                .unwrap()
                .unwrap()
                .mtime;

            // Simulate serialized state: file with unsaved changes
            let serialized_editor = SerializedEditor {
                abs_path: Some(PathBuf::from(path!("/standalone.rs"))),
                contents: Some("modified content".to_string()),
                language: Some("Rust".to_string()),
                mtime: Some(mtime),
            };

            editor_db
                .save_serialized_editor(item_id, workspace_id, serialized_editor)
                .await
                .unwrap();

            let deserialized =
                deserialize_editor(item_id, workspace_id, workspace, project, cx).await;

            deserialized.update(cx, |editor, cx| {
                // The editor should have the serialized contents, not the disk contents
                assert_eq!(editor.text(cx), "modified content");
                assert!(editor.is_dirty(cx));
                assert!(!editor.has_conflict(cx));

                let buffer = editor.buffer().read(cx).as_singleton().unwrap().read(cx);
                assert!(buffer.file().is_some());
            });
        }
    }

    // Verify that renaming an open file emits EditorEvent::FileHandleChanged so that
    // the workspace re-serializes the editor with the updated path.
    #[gpui::test]
    async fn test_file_handle_changed_on_rename(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        use std::cell::RefCell;
        use std::rc::Rc;
        use util::rel_path::rel_path;

        init_test(cx, |_| {});

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/root"), json!({ "file.rs": "fn main() {}" }))
            .await;

        let project = Project::test(fs.clone(), [path!("/root").as_ref()], cx).await;

        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/root/file.rs"), cx)
            })
            .await
            .unwrap();

        let received_file_handle_changed = Rc::new(RefCell::new(false));
        let (editor, cx) = cx.add_window_view({
            let project = project.clone();
            let received_file_handle_changed = received_file_handle_changed.clone();
            move |window, cx| {
                let mut editor = Editor::for_buffer(buffer, Some(project), window, cx);
                editor.set_should_serialize(true, cx);
                let entity = cx.entity();
                cx.subscribe_in(&entity, window, move |_, _, event: &EditorEvent, _, _| {
                    if matches!(event, EditorEvent::FileHandleChanged) {
                        *received_file_handle_changed.borrow_mut() = true;
                    }
                })
                .detach();
                editor
            }
        });

        cx.run_until_parked();

        let (entry_id, worktree_id) = project.update(cx, |project, cx| {
            let worktree = project.worktrees(cx).next().unwrap();
            let worktree = worktree.read(cx);
            let entry = worktree.entry_for_path(rel_path("file.rs")).unwrap();
            (entry.id, worktree.id())
        });

        project
            .update(cx, |project, cx| {
                project.rename_entry(entry_id, (worktree_id, rel_path("renamed.rs")).into(), cx)
            })
            .await
            .unwrap();

        cx.run_until_parked();

        assert!(
            *received_file_handle_changed.borrow(),
            "EditorEvent::FileHandleChanged must be emitted when the open file is renamed"
        );

        editor.update(cx, |editor, cx| {
            let buffer = editor.buffer().read(cx).as_singleton().unwrap();
            let path = buffer.read(cx).file().unwrap().path();
            assert!(
                path.as_std_path().ends_with("renamed.rs"),
                "buffer path must reflect the renamed file, got {path:?}"
            );
        });
    }

    // Regression test for https://github.com/zed-industries/zed/issues/35947
    // Verifies that deserializing a non-worktree editor does not add the item
    // to any pane as a side effect.
    #[gpui::test]
    async fn test_deserialize_non_worktree_file_does_not_add_to_pane(
        cx: &mut gpui::TestAppContext,
    ) {
        init_test(cx, |_| {});

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/outside"), json!({ "settings.json": "{}" }))
            .await;

        // Project with a different root — settings.json is NOT in any worktree
        let project = Project::test(fs.clone(), [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());
        let db = cx.update(|_, cx| workspace::WorkspaceDb::global(cx));
        let editor_db = cx.update(|_, cx| EditorDb::global(cx));

        let workspace_id = db.next_id().await.unwrap();
        let item_id = 99999 as ItemId;

        let serialized_editor = SerializedEditor {
            abs_path: Some(PathBuf::from(path!("/outside/settings.json"))),
            contents: None,
            language: None,
            mtime: None,
        };

        editor_db
            .save_serialized_editor(item_id, workspace_id, serialized_editor)
            .await
            .unwrap();

        // Count items in all panes before deserialization
        let pane_items_before = workspace.read_with(cx, |workspace, cx| {
            workspace
                .panes()
                .iter()
                .map(|pane| pane.read(cx).items_len())
                .sum::<usize>()
        });

        let deserialized =
            deserialize_editor(item_id, workspace_id, workspace.clone(), project, cx).await;

        cx.run_until_parked();

        // The editor should exist and have the file
        deserialized.update(cx, |editor, cx| {
            let buffer = editor.buffer().read(cx).as_singleton().unwrap().read(cx);
            assert!(buffer.file().is_some());
        });

        // No items should have been added to any pane as a side effect
        let pane_items_after = workspace.read_with(cx, |workspace, cx| {
            workspace
                .panes()
                .iter()
                .map(|pane| pane.read(cx).items_len())
                .sum::<usize>()
        });

        assert_eq!(
            pane_items_before, pane_items_after,
            "Editor::deserialize should not add items to panes as a side effect"
        );
    }

    #[gpui::test]
    async fn test_open_resolved_target_at_non_ascii_column(cx: &mut gpui::TestAppContext) {
        init_test(cx, |_| {});

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "src": {
                    "main.rs": "first\naéøbc\n",
                },
            }),
        )
        .await;

        let project = Project::test(fs.clone(), [path!("/root").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace = multi_workspace.read_with(cx, |mw, _| mw.workspace().clone());

        let open_target = OpenTarget::Path(
            PathWithPosition {
                path: PathBuf::from(path!("/root/src/main.rs")),
                row: Some(2),
                column: Some(4),
            },
            false,
            OpenTargetFoundBy::BackgroundPathResolution,
        );

        let opened = workspace
            .update_in(cx, |_, window, cx| {
                cx.spawn_in(window, async move |workspace, cx| {
                    open_resolved_target(&workspace, &open_target, cx).await
                })
            })
            .await
            .expect("opening the target should succeed");
        assert!(opened, "target should open as a file");

        let editor = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.act_as::<Editor>(cx))
                .expect("active item should be an editor")
        });
        let cursor = editor.update_in(cx, |editor, _, cx| {
            editor
                .selections
                .newest::<language::Point>(&editor.display_snapshot(cx))
                .head()
        });
        // Column 4 is the fourth character of `aéøbc` (the `b`), which starts at byte 5.
        assert_eq!(cursor, language::Point::new(1, 5));
    }

    #[gpui::test]
    fn test_compute_modified_ranges_git_diff(cx: &mut gpui::TestAppContext) {
        let base_text = "line0\nline1\nline2\nline3\nline4\nline5\nline6\n";
        // Modify line1 and line5 to create two non-adjacent hunks.
        let buffer_text = "line0\nMOD1\nline2\nline3\nline4\nMOD5\nline6\n";

        let buffer = cx.new(|cx| language::Buffer::local(buffer_text, cx));
        let diff_snapshot = buffer.update(cx, |buffer, cx| {
            let diff = cx.new(|cx| {
                buffer_diff::BufferDiff::new_with_base_text(base_text, &buffer.text_snapshot(), cx)
            });
            diff.read(cx).snapshot(cx)
        });

        let ranges = buffer.update(cx, |buffer, _cx| {
            compute_modified_ranges(&buffer.snapshot(), &diff_snapshot)
        });

        assert_eq!(ranges.len(), 2, "expected 2 non-adjacent ranges");

        buffer.update(cx, |buffer, _cx| {
            let text_snapshot: &text::BufferSnapshot = buffer;
            let r0 = ranges[0].start.to_point(text_snapshot)..ranges[0].end.to_point(text_snapshot);
            let r1 = ranges[1].start.to_point(text_snapshot)..ranges[1].end.to_point(text_snapshot);
            assert_eq!(r0.start.row, 1, "first hunk should start at row 1");
            assert_eq!(r0.end.row, 1, "first hunk should end at row 1");
            assert_eq!(r1.start.row, 5, "second hunk should start at row 5");
            assert_eq!(r1.end.row, 5, "second hunk should end at row 5");
        });
    }

    #[gpui::test]
    fn test_compute_modified_ranges_unchanged_buffer(cx: &mut gpui::TestAppContext) {
        let buffer_text = "line0\nline1\nline2\n";
        let buffer = cx.new(|cx| language::Buffer::local(buffer_text, cx));
        let diff_snapshot = buffer.update(cx, |buffer, cx| {
            let diff = cx.new(|cx| {
                buffer_diff::BufferDiff::new_with_base_text(
                    buffer_text,
                    &buffer.text_snapshot(),
                    cx,
                )
            });
            diff.read(cx).snapshot(cx)
        });

        let ranges = buffer.update(cx, |buffer, _cx| {
            compute_modified_ranges(&buffer.snapshot(), &diff_snapshot)
        });

        assert_eq!(
            ranges,
            Vec::new(),
            "buffer that matches its diff base should produce no modified ranges"
        );
    }

    #[gpui::test]
    fn test_compute_modified_ranges_deletion_only(cx: &mut gpui::TestAppContext) {
        let base_text = "line0\nline1\nline2\n";
        // Buffer has line1 deleted (pure deletion).
        let buffer_text = "line0\nline2\n";

        let buffer = cx.new(|cx| language::Buffer::local(buffer_text, cx));
        let diff_snapshot = buffer.update(cx, |buffer, cx| {
            let diff = cx.new(|cx| {
                buffer_diff::BufferDiff::new_with_base_text(base_text, &buffer.text_snapshot(), cx)
            });
            diff.read(cx).snapshot(cx)
        });

        // Verify the diff has a deletion hunk.
        let hunk_count = buffer.update(cx, |buffer, _cx| {
            let text_snapshot: &text::BufferSnapshot = buffer;
            diff_snapshot.hunks(text_snapshot).count()
        });
        assert!(hunk_count > 0, "diff should have hunks");

        let ranges = buffer.update(cx, |buffer, _cx| {
            compute_modified_ranges(&buffer.snapshot(), &diff_snapshot)
        });

        assert_eq!(
            ranges,
            Vec::new(),
            "deletion-only hunks should be skipped, leaving no ranges"
        );
    }

    #[gpui::test]
    fn test_compute_modified_ranges_adjacent_hunks(cx: &mut gpui::TestAppContext) {
        let base_text = "line0\nline1\nline2\nline3\nline4\n";
        // Modify lines 2 and 3 which are adjacent; they should merge into one range.
        let buffer_text = "line0\nline1\nMOD2\nMOD3\nline4\n";

        let buffer = cx.new(|cx| language::Buffer::local(buffer_text, cx));
        let diff_snapshot = buffer.update(cx, |buffer, cx| {
            let diff = cx.new(|cx| {
                buffer_diff::BufferDiff::new_with_base_text(base_text, &buffer.text_snapshot(), cx)
            });
            diff.read(cx).snapshot(cx)
        });

        let ranges = buffer.update(cx, |buffer, _cx| {
            compute_modified_ranges(&buffer.snapshot(), &diff_snapshot)
        });

        assert_eq!(
            ranges.len(),
            1,
            "adjacent hunks (rows 2 and 3) should be merged into one range"
        );
        buffer.update(cx, |buffer, _cx| {
            let text_snapshot: &text::BufferSnapshot = buffer;
            let r = ranges[0].start.to_point(text_snapshot)..ranges[0].end.to_point(text_snapshot);
            assert_eq!(r.start.row, 2, "merged range should start at row 2");
            assert_eq!(r.end.row, 3, "merged range should end at row 3");
        });
    }

    // Regression test for a multi-buffer (e.g. project search results) that excerpts
    // an untitled buffer alongside a file-backed one. Saving used to error out with
    // "buffer doesn't have a file", which aborted `workspace: reload` and quit flows.
    #[gpui::test]
    async fn test_save_multi_buffer_with_untitled_buffer_skips_untitled(
        cx: &mut gpui::TestAppContext,
    ) {
        init_test(cx, |_| {});

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/dir"), json!({ "file.txt": "the cat sat" }))
            .await;
        let project = Project::test(fs.clone(), [path!("/dir").as_ref()], cx).await;
        let window =
            cx.add_window(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let cx = &mut VisualTestContext::from_window(*window, cx);

        let file_buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/dir/file.txt"), cx)
            })
            .await
            .unwrap();
        let untitled_buffer = project.update(cx, |project, cx| {
            project.create_local_buffer("the cat", None, false, cx)
        });

        // Make both buffers dirty so both are candidates to be saved.
        file_buffer.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "X")], None, cx);
        });
        untitled_buffer.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "Y")], None, cx);
        });

        let multi_buffer = cx.new(|cx| {
            let mut multi_buffer = MultiBuffer::new(project.read(cx).capability());
            multi_buffer.set_excerpts_for_path(
                PathKey::sorted(0),
                file_buffer.clone(),
                [Point::new(0, 0)..Point::new(0, 3)],
                0,
                cx,
            );
            multi_buffer.set_excerpts_for_path(
                PathKey::sorted(1),
                untitled_buffer.clone(),
                [Point::new(0, 0)..Point::new(0, 3)],
                0,
                cx,
            );
            multi_buffer
        });
        let editor = cx.new_window_entity(|window, cx| {
            Editor::for_multibuffer(multi_buffer, Some(project.clone()), window, cx)
        });
        cx.run_until_parked();

        editor.update(cx, |editor, cx| {
            assert!(!editor.buffer().read(cx).is_singleton());
        });

        let save = editor.update_in(cx, |editor, window, cx| {
            editor.save(
                SaveOptions {
                    format: false,
                    force_format: false,
                    autosave: false,
                },
                project.clone(),
                window,
                cx,
            )
        });
        save.await
            .expect("saving a multi-buffer that excerpts an untitled buffer should not error");
        cx.run_until_parked();

        // The file-backed buffer is saved; the untitled buffer is skipped and stays dirty.
        file_buffer.update(cx, |buffer, _| assert!(!buffer.is_dirty()));
        untitled_buffer.update(cx, |buffer, _| {
            assert!(buffer.file().is_none());
            assert!(buffer.is_dirty());
        });
    }
}
