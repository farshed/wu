# Changelog

All notable user-facing changes to Wu are listed here, newest first. Add a
bullet under Unreleased with your change; the version bump commit turns that
section into the release, and the release workflow copies it into the GitHub
release body.

## Unreleased

- Added an Enable Agent Chat setting. Turn it off to hide the agent panel, its commands and everything else agent-related. Wu asks first if an agent is still working, since turning it off stops it.
- Added settings for new agent chats: a fixed starting effort, and a starting permission mode for each agent.
- Added a While Working setting. Set it to Steer and a message you send while the agent works goes out right away instead of waiting in the queue.
- Wu now asks before quitting or restarting while an agent is working, since that stops it. You can turn this off in the Agent Chat settings.
- Added settings to hide Claude Code, Codex or OpenCode from the new chat menu and Continue Saved Chat.
- Added Auto-Compact to the Agent Chat settings. Turn it on and pick a context limit, and Claude Code and Codex chats compact once they reach it.
- Desktop notifications for agent chats are now off by default. On macOS, turning them on asks to allow Wu's notifications, and opens System Settings if they're turned off there.
- Added Continue Saved Chat to the new chat menu, which picks up a chat you started in Claude Code, Codex or OpenCode for the current project. Wu copies it into a new chat with its history, so the original stays as it was.
- Removed Claude Code's settings and info commands like /model, /usage and /context from the chat's / menu, since Wu has its own controls for them and their answers never showed up in the chat. Typing /clear now starts a new chat, /model and /effort open the model picker, and the rest explain that they only work in the terminal. Skills, your own commands, /review, /init and /compact are still there.
- Fixed Codex /review showing the review twice.


## 1.1.4 - 2026-10-08

- Made the time on agent chats in the sidebar easier to read.
- Fixed error and warning popups in the editor being see-through and hard to read in Wu Dark.
- Cleaned up the context menu options in the agent chat sidebar.
- Forked chats now continue from a real copy of the agent's own session.
- User messages in agent chats can now be selected and copied in part.
- Fixed open files sometimes not coming back when reopening a project.

## 1.1.3 - 2026-10-06

- Support for Intel Macs.
- Fixed subagent tabs not showing the working indicator while a background subagent was still running.
- Hovering the running command or agent indicator under the chat box now shows what's running in a tooltip.
- The title bar and status bar in Wu Dark now have the same tint as the panels.
- Fixed the working indicator in agent chats not coming back when the agent picks up again after a background process finishes.
- Agent Chat now supports OpenCode.
- The line number gutter is more compact.
- The activity bar can sit on the right side of the window. Set it with `activity_bar.position` or in Settings.
- A little more space between the activity bar icons.
- Activity bar icons now follow the UI font size by default.
- Fixed the side panel jumping wider when you start resizing it.
- Users can now choose permission modes from the model picker.
- Improve Wu Light's interface colors.

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
