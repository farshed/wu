# Changelog

All notable user-facing changes to Wu are listed here, newest first. Add a
bullet under Unreleased with your change; the version bump commit turns that
section into the release, and the release workflow copies it into the GitHub
release body.

## Unreleased

- The title bar and status bar in Wu Dark now have the same tint as the panels instead of showing the plain blurred desktop.
- Fixed the working indicator in agent chats not coming back when the agent picks up again after a background command or subagent finishes.
- Agent Chat now works with OpenCode. Start an OpenCode chat from the new chat menu to use any model you've set up in OpenCode, with its slash commands, skills, thinking levels and subagents.
- The line number gutter is more compact: it reserves room for three digits instead of four (more only when a file needs them) and has less padding around the numbers.
- The activity bar can sit on the right side of the window. Set it with `activity_bar.position` or in Settings, and importing VS Code settings with the side bar on the right moves the activity bar and the side panels there too.
- A little more space between the activity bar icons.
- Activity bar icons now follow the UI font size by default (1.25 times it, 20px at the default size). Setting `activity_bar.icon_size` still fixes them at that many pixels.
- Fixed the side panel jumping wider when you start resizing it, which left the resize handle away from the pointer.
- Choose how much an agent can do without asking from the model picker: Claude Code's and Codex's own permission modes. Chats start in Auto, which only asks about risky actions, and each chat remembers its choice. The model chip shows the current mode, and Wu says so when Claude can't use Auto. Permission prompts now offer just Allow and Deny.
- Wu Light now uses the same interface colors as One Light, with Wu's own code colors.

## 1.1.2 - 2026-10-05

- Fixed images dropped onto an agent chat opening in a preview instead of being attached.
- When an agent is waiting on a background command or subagent, the bar under the chat box shows what's running and for how long.
- Search inside an open agent chat with the Find bar (Cmd-F, or Ctrl-F on Windows and Linux).
- Fixed macOS asking Wu for access to Desktop, Documents and Photos when agent chat looked up the available models.
- Fixed the tab and the split and zoom buttons flickering after clicking into a chat message and scrolling away from it.
- Fixed empty thought blocks in agent chats.
- Thoughts in agent chats stay collapsed until you open them.
- Claude models now use their largest context window by default, and Wu picks up the available sizes from Claude Code.
- Agent notifications now use the system's native notifications.
- In the model picker, effort is set with a slider and context window show every choice as a button.
- Fixed the dark bar that showed at the top of the file tree when scrolling in Wu themes.
- The sidebar, activity bar and tab bar in Wu Light now have solid colors.
- Fixed the context window and plan usage popups in agent chats staying open when clicking elsewhere.
- Removed the options menu from the top of the agent chat sidebar.
- Agent chats in the sidebar no longer repeat the project name on every row, so the list is shorter and easier to scan.
- The agent chat sidebar now uses the same background as the other panels in every theme.

## 1.1.1 - 2026-10-05

- `/compact` and other slash commands now work mid-conversation in Claude chats.
- The chat shows "Compacting conversation…" while the agent compacts and the context ring updates right after.
- Long agent chats scroll smoothly and typing in the chat box feels instant, since only the messages on screen are drawn.
- Fixed a black bar at the top of the file tree when scrolling with the Wu themes.
- Fixed text in the chat box being hard to read after switching between Wu Dark and Wu Light.

## 1.1.0 - 2026-10-04

- The activity bar's icon size is now configurable in settings and via the `activity_bar.icon_size` property.
- New Wu Light and Wu Dark themes.
- Symbols is the new default icon theme.
- Geist is the new interface font, and JetBrains Mono is the new code font.
- Redesigned tabs.
- New Agent Chat panel in the activity bar. Chat with Claude Code or Codex using the CLIs you already have installed and signed in; no API keys needed.
- Agent chats support slash commands, `@` file mentions and `$` skills, with a menu that completes them as you type.
- Attach images to a message by pasting, dropping or using the paperclip button.
- The chat sidebar has pins, custom sections, an archive, search and grouping by project. Chats can be forked, and side chats open next to the main one.
- Wu can play a sound and show a notification when the agent finishes, needs your input or hits an error.
- Voice dictation in the chat box, using a local speech model that downloads on first use.
- New Agent Chat page in settings.
- Idle windows on macOS stop redrawing until something changes, which saves power.
- Fixed some list scrolling glitches and images that were sized wrongly in some layouts.

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
