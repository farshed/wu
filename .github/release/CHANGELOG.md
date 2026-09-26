# Changelog

All notable user-facing changes to Wu are listed here, newest first. Add a
bullet under Unreleased with your change; the version bump commit turns that
section into the release, and the release workflow copies it into the GitHub
release body.

## Unreleased

## 1.0.10 - 2026-09-26

- Project search uses much less memory. Files in the results are only parsed for syntax highlighting once they're shown on screen.
- Breadcrumbs and sticky headers fill in as soon as a newly opened file finishes parsing, instead of waiting for the cursor to move.
- Selected files in lists like the project panel stand out more in the Catppuccin themes.
- Fixed a crash when uninstalling an extension whose theme has the same name as a built-in theme.
- Opening a result from the search panel highlights the matches in that file the same way in-file search does.
- Fixed cursor and scroll positions sometimes not being saved for files that were just opened.

## 1.0.9 - 2026-09-20

- Files copied in the project panel can now be pasted into another Wu window or into other apps.
- Closing the last tab keeps the window open by default.
- The project panel context menu has a Get Info item (Properties on Windows and Linux) that shows a file or folder's path, size, created and modified dates, and permissions.
- Removed the bundled Ayu and Gruvbox themes.

## 1.0.8 - 2026-09-12

- Material Icon Theme is now the default icon theme, with light and dark variants that follow the theme mode.
- Synced with Zed 1.19.2: multi-select in the Git panel, automatic language detection for untitled buffers, project search on type, recency-sorted command palette, the `reveal_if_open` setting, and many fixes.
- Removed "Delete Permanently" from the project panel context menu. Delete always moves to the Trash.
- Right-clicking the empty space below the file tree now opens the context menu.
- Tabs for files that no longer exist show "File not found. It was deleted or moved." without offering to recreate the file.
- Release builds compile every crate as a single codegen unit again for better runtime performance.

## 1.0.7 - 2026-09-11

- Files that were deleted while Wu was closed reopen as strikethrough tabs with a "file not found" message instead of a blank editor.
- Tabs show the file's icon before its name.
- The project panel's Delete action moves files to the Trash, with a separate Delete Permanently option.
- Activity bar icons match VS Code's, with more vertical spacing, and all left-dock panels share one width.
- Search moved into its own panel, and activity bar items can be reordered.
- The file tree scrolls a little past its last entry.
