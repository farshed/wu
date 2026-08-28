# Zed crate map

This document maps every Cargo crate below `crates/` to its responsibility and
its place in Zed's dependency hierarchy. It covers 188 manifests: 187 workspace
packages plus the excluded `gpui_web/examples/hello_web` example. It does not
cover the separate crates below `extensions/` or `tooling/`.

The descriptions reflect the code and manifests on August 28, 2026. AI/agent,
collaboration, call, and audio crates were removed from this fork on that date. Crate
boundaries change frequently; treat each linked `Cargo.toml` and crate root as
the source of truth.

## How to read the hierarchy

The workspace is a directed acyclic graph at build time, not a strict tree. In
the diagrams below, `A -> B` means that A depends on B. The useful conceptual
layers are:

```text
Executables
  zed, cli, remote_server, developer tools
      |
Feature UI
  editor, git_ui, debugger_ui, project_panel, ...
      |
Application coordination
  workspace, project, extension_host, client, ...
      |
Domain models and services
  worktree, language, text, dap, task, settings, ...
      |
UI and infrastructure
  ui, gpui, fs, rpc, http_client, db, ...
      |
Foundations
  util, collections, sum_tree, rope, path, clock, macros
```

The most important dependency spines are:

```text
zed -> workspace -> project -> worktree -> fs
                 |          -> language -> text -> rope -> sum_tree
                 -> editor -> multi_buffer -> text

zed -> ui -> component -> gpui -> gpui_platform-facing traits
gpui_platform -> gpui_{macos,linux,windows,web}
gpui_{linux,web} -> gpui_wgpu
gpui_macos -> gpui_apple

zed -> extension_host -> extension -> language / dap / task
zed -> theme_extension -> extension / theme
zed -> client -> rpc -> proto
```

Crates named `*_ui` generally sit above a non-UI model or service crate. Crates
named `*_core`, `*_types`, or `*_settings` split stable lower-level types from a
larger feature so unrelated consumers do not need the whole feature. Crates
named `*_macros` are procedural macro leaves used by their corresponding
runtime crate. Platform crates implement interfaces defined by `gpui` and are
selected through `gpui_platform`.

## Entrypoints and developer tools

| Crate                                                                  | Target                        | Responsibility and hierarchy                                                                                                                                                  |
| ---------------------------------------------------------------------- | ----------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`zed`](../../crates/zed/)                                             | Binary                        | The desktop application composition root. It initializes nearly every user-facing subsystem and selects the GPUI platform implementation.                                     |
| [`cli`](../../crates/cli/)                                             | Library and binary            | Implements the `zed` command-line client, request/response IPC with a running app, shell completions, and platform launch behavior.                                           |
| [`remote_server`](../../crates/remote_server/)                         | Binary and library            | Runs the headless Zed server on SSH, WSL, or other remote hosts and exposes project operations to a desktop client.                                                           |
| [`auto_update_helper`](../../crates/auto_update_helper/)               | Binary                        | Privileged/platform update helper that replaces an installed Zed application and reports update errors.                                                                       |
| [`docs_preprocessor`](../../crates/docs_preprocessor/)                 | Binary                        | mdBook preprocessor/postprocessor for Zed docs, including action and keybinding expansion, generated settings data, redirects, and page metadata.                             |
| [`extension_cli`](../../crates/extension_cli/)                         | Binary                        | Command-line tooling for compiling, validating, and packaging Zed extensions for publication.                                                                                 |
| [`schema_generator`](../../crates/schema_generator/)                   | Binary                        | Generates JSON schemas for Zed settings, keymaps, tasks, debug configurations, and related configuration types.                                                               |
| [`theme_importer`](../../crates/theme_importer/)                       | Binary                        | Converts VS Code theme data into Zed theme JSON.                                                                                                                              |
| [`editor_benchmarks`](../../crates/editor_benchmarks/)                 | Binary                        | Standalone benchmark harness for editor rendering and editing workloads.                                                                                                      |
| [`fs_benchmarks`](../../crates/fs_benchmarks/)                         | Binary                        | Standalone benchmark harness for filesystem implementations and file operations.                                                                                              |
| [`project_benchmarks`](../../crates/project_benchmarks/)               | Binary                        | Standalone benchmark harness for project-level workloads such as search and language-server coordination.                                                                     |
| [`worktree_benchmarks`](../../crates/worktree_benchmarks/)             | Binary                        | Standalone benchmark harness for worktree scanning, snapshots, and file-change processing.                                                                                    |
| [`benchmarks`](../../crates/benchmarks/)                               | Library and benchmark targets | Central Criterion benchmark package; keeps benchmark-only dependencies out of production crates.                                                                              |
| [`component_preview`](../../crates/component_preview/)                 | Library and example           | Developer application for browsing registered UI component examples and persisting preview state.                                                                             |
| [`explorer_command_injector`](../../crates/explorer_command_injector/) | Windows `cdylib`              | Windows Explorer COM extension that adds an "Open with Zed" shell command.                                                                                                    |
| [`hello_web`](../../crates/gpui_web/examples/hello_web/)               | Example binary                | Minimal browser example for `gpui_web`; excluded from the root workspace.                                                                                                     |

## Foundational data structures and utilities

These crates are near the bottom of the dependency graph. Most application
features depend on some of them, but they depend on little Zed-specific code.

| Crate                                                             | Responsibility and hierarchy                                                                                                                 |
| ----------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| [`collections`](../../crates/collections/)                        | Re-exports Zed's standard hash/map/set choices and supplies collection helpers such as `VecMap`; built on `gpui_util`.                       |
| [`util`](../../crates/util/)                                      | Shared command, filesystem, shell, serialization, path-list, redaction, archive, time, and test helpers used across Zed.                     |
| [`util_macros`](../../crates/util_macros/)                        | Procedural and declarative utility macros, including compile-time paths/URIs, line endings, and performance-test support.                    |
| [`gpui_util`](../../crates/gpui_util/)                            | Lowest-level utilities shared by GPUI and Zed, including copy-on-write arcs, command construction, measurements, and result logging helpers. |
| [`gpui_shared_string`](../../crates/gpui_shared_string/)          | Defines GPUI's cheap-to-clone `SharedString` type without depending on the full UI framework.                                                |
| [`sum_tree`](../../crates/sum_tree/)                              | Persistent, summary-augmented B-tree used for text, buffers, selections, and other indexed sequences.                                        |
| [`rope`](../../crates/rope/)                                      | UTF-8 text rope and point/offset conversion types, built on `sum_tree`.                                                                      |
| [`clock`](../../crates/clock/)                                    | Replica identifiers and Lamport/global logical timestamps used by collaborative data structures.                                             |
| [`watch`](../../crates/watch/)                                    | GPUI-aware watch channels that publish the latest value to asynchronous receivers.                                                           |
| [`path`](../../crates/path/)                                      | Validated absolute and normalized relative path types plus path-style conversion helpers.                                                    |
| [`paths`](../../crates/paths/)                                    | Central definitions for Zed's config, data, cache, database, log, socket, and remote-server locations.                                       |
| [`env_var`](../../crates/env_var/)                                | Typed, lazily read environment-variable definitions with optional inventory-based discovery.                                                 |
| [`zed_env_vars`](../../crates/zed_env_vars/)                      | Declares Zed-specific environment variables, currently including stateless-mode configuration, on top of `env_var`.                          |
| [`refineable`](../../crates/refineable/)                          | Runtime traits and containers for partially overriding and cascading complex configuration structures.                                       |
| [`derive_refineable`](../../crates/refineable/derive_refineable/) | Procedural macro that derives the refinement types consumed by `refineable`.                                                                 |
| [`scheduler`](../../crates/scheduler/)                            | Priority task scheduler, executor, timers, runnable metadata, and deterministic test scheduler used by GPUI.                                 |
| [`time_format`](../../crates/time_format/)                        | Localized date, time, and relative timestamp formatting helpers.                                                                             |
| [`zlog`](../../crates/zlog/)                                      | Zed's structured logging facade, filters, sinks, and initialization.                                                                         |
| [`zlog_settings`](../../crates/zlog_settings/)                    | Connects runtime log-filter settings to `zlog`.                                                                                              |
| [`ztracing`](../../crates/ztracing/)                              | Lightweight tracing spans and backend initialization, bridging instrumentation to Zed logging and optional profilers.                        |
| [`ztracing_macro`](../../crates/ztracing_macro/)                  | `instrument` procedural macro used by `ztracing`.                                                                                            |
| [`feature_flags`](../../crates/feature_flags/)                    | Registers feature flags, loads local/remote values, and exposes GPUI observation APIs.                                                       |
| [`feature_flags_macros`](../../crates/feature_flags_macros/)      | Derive support for enum-backed feature flags.                                                                                                |
| [`release_channel`](../../crates/release_channel/)                | Zed version, commit, app identifier, URL, and stable/preview/nightly/development channel types.                                              |
| [`windows_resources`](../../crates/windows_resources/)            | Build helper for compiling Windows manifests, icons, and version resources.                                                                  |

## GPUI and platform backends

`gpui` defines the framework-facing platform traits. Applications normally use
`gpui_platform`, which chooses the current backend. The backend crates then use
shared renderer crates where appropriate.

| Crate                                          | Responsibility and hierarchy                                                                                                                                 |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [`gpui`](../../crates/gpui/)                   | Zed's GPU UI framework: application/entity contexts, elements, layout, styling, windows, input, actions, executors, assets, rendering primitives, and tests. |
| [`gpui_macros`](../../crates/gpui_macros/)     | Derives and attributes for GPUI actions, rendering, elements, contexts, styles, tests, and benchmarks.                                                       |
| [`gpui_platform`](../../crates/gpui_platform/) | Selects and re-exports the active GPUI backend and constructors for normal, headless, and web applications.                                                  |
| [`gpui_apple`](../../crates/gpui_apple/)       | Apple-shared Metal renderer, atlas, shaders, and GPU resource management below the macOS backend.                                                            |
| [`gpui_macos`](../../crates/gpui_macos/)       | macOS windowing, events, text, clipboard, display, screen capture, notifications, and platform integration; uses `gpui_apple`.                               |
| [`gpui_linux`](../../crates/gpui_linux/)       | Linux platform implementation for windows, input, displays, clipboard, text, and rendering; uses `gpui_wgpu`.                                                |
| [`gpui_windows`](../../crates/gpui_windows/)   | Windows platform implementation using Win32, DirectWrite, DirectManipulation, and a DirectX renderer.                                                        |
| [`gpui_web`](../../crates/gpui_web/)           | Browser platform implementation using a document canvas, DOM input/IME integration, and WebGPU or WebGL rendering through `gpui_wgpu`.                       |
| [`gpui_wgpu`](../../crates/gpui_wgpu/)         | Shared WGPU/WebGL renderer, atlases, shaders, and text system used by Linux and web backends.                                                                |
| [`gpui_tokio`](../../crates/gpui_tokio/)       | Integrates a Tokio runtime and task spawning with GPUI's executors.                                                                                          |
| [`media`](../../crates/media/)                 | Low-level macOS CoreMedia and CoreVideo bindings used by media-capable GPUI code.                                                                            |

## UI primitives and application chrome

| Crate                                                          | Responsibility and hierarchy                                                                                                               |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| [`assets`](../../crates/assets/)                               | Embeds and exposes static application assets through GPUI's asset source interface.                                                        |
| [`icons`](../../crates/icons/)                                 | Defines the canonical `IconName` enumeration shared by UI components.                                                                      |
| [`component`](../../crates/component/)                         | Registers visual-testable components and lays out component examples; sits between `gpui` and the higher `ui` crate.                       |
| [`ui`](../../crates/ui/)                                       | Zed's reusable design-system components, styles, traits, and preludes, built on `component`, `theme`, and `gpui`.                          |
| [`ui_macros`](../../crates/ui_macros/)                         | Derives UI component registration and dynamic-spacing support for `ui`.                                                                    |
| [`ui_input`](../../crates/ui_input/)                           | Form-oriented text and number inputs backed by the editor; separate from `ui` to avoid making all UI primitives depend on `editor`.        |
| [`ui_prompt`](../../crates/ui_prompt/)                         | Renders markdown prompts in workspace modals using Zed UI and theme primitives.                                                            |
| [`menu`](../../crates/menu/)                                   | Shared menu navigation actions and initialization used by pickers, context menus, and panels.                                              |
| [`panel`](../../crates/panel/)                                 | Small reusable panel header and tab components used by feature panels.                                                                     |
| [`picker`](../../crates/picker/)                               | Generic searchable picker state, delegate API, list rendering, persistence, menus, and preview abstraction.                                |
| [`picker_preview`](../../crates/picker_preview/)               | Editor-backed implementation of the lower-level `picker` preview interface.                                                                |
| [`platform_title_bar`](../../crates/platform_title_bar/)       | Cross-platform title-bar component and native window-control placement.                                                                    |
| [`title_bar`](../../crates/title_bar/)                         | Application title bar, menus, account state, remote state, and update indicators; built above `workspace`.                                |
| [`breadcrumbs`](../../crates/breadcrumbs/)                     | Reusable breadcrumb layout and text rendering used by editors and terminal views.                                                          |
| [`notifications`](../../crates/notifications/)                 | Workspace notifications, status toasts, and cloud notification state.                                                                      |
| [`activity_indicator`](../../crates/activity_indicator/)       | Status-bar activity model/UI for background project, language-server, extension, and update work.                                          |
| [`command_palette_hooks`](../../crates/command_palette_hooks/) | Low-dependency registry for filtering or intercepting command-palette actions without making feature crates depend on the full palette UI. |
| [`command_palette`](../../crates/command_palette/)             | Searchable action picker, fuzzy matching, persistence, telemetry, action dispatch, and registered interception hooks.                      |
| [`input_latency_ui`](../../crates/input_latency_ui/)           | Captures, formats, and reports input-to-frame latency diagnostics and telemetry.                                                           |
| [`inspector_ui`](../../crates/inspector_ui/)                   | Developer inspector for GPUI elements, styles, and editor context.                                                                         |
| [`miniprofiler_ui`](../../crates/miniprofiler_ui/)             | Developer window for viewing runtime profiler data received through RPC.                                                                   |

## Filesystem, persistence, networking, and process infrastructure

| Crate                                                                | Responsibility and hierarchy                                                                                                                 |
| -------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| [`fs`](../../crates/fs/)                                             | Abstract asynchronous filesystem, watcher, metadata, atomic writes, trash support, fake filesystem, and Git-aware test helpers.              |
| [`db`](../../crates/db/)                                             | Application database facade, scopes, key-value storage, and domain migrations; uses `sqlez` and Zed's database paths.                        |
| [`sqlez`](../../crates/sqlez/)                                       | Typed SQLite connections, statements, transactions/savepoints, migrations, domains, and thread-safe access.                                  |
| [`sqlez_macros`](../../crates/sqlez_macros/)                         | SQL procedural macro used by `sqlez` clients for checked statement definitions.                                                              |
| [`http_client`](../../crates/http_client/)                           | Object-safe asynchronous HTTP abstraction, request helpers, proxy wrapping, redirects, timeouts, and GitHub downloads.                       |
| [`reqwest_client`](../../crates/reqwest_client/)                     | Production `http_client` implementation backed by Reqwest and its runtime.                                                                   |
| [`http_client_tls`](../../crates/http_client_tls/)                   | Shared rustls root-certificate and TLS configuration for HTTP and realtime clients.                                                          |
| [`net`](../../crates/net/)                                           | Runtime-neutral asynchronous TCP listeners, sockets, streams, and network utility types.                                                     |
| [`proxy_handshake`](../../crates/proxy_handshake/)                   | Sans-I/O HTTP CONNECT, SOCKS4/4a, and SOCKS5 client handshakes, with Futures and Tokio drivers.                                              |
| [`node_runtime`](../../crates/node_runtime/)                         | Locates/downloads Node, chooses versions, and runs npm commands for language servers, Prettier, and extensions.                              |
| [`credentials_provider`](../../crates/credentials_provider/)         | Platform-neutral asynchronous interface for reading, writing, and deleting credentials.                                                      |
| [`zed_credentials_provider`](../../crates/zed_credentials_provider/) | Global Zed credential-provider implementation and initialization, using release-aware service names and platform storage.                    |
| [`oauth_callback_server`](../../crates/oauth_callback_server/)       | Loopback OAuth callback listener and shared browser success/error response page.                                                             |
| [`askpass`](../../crates/askpass/)                                   | Secure askpass sessions and encrypted password transport for Git and other subprocess authentication prompts.                                |
| [`system_specs`](../../crates/system_specs/)                         | Collects OS, CPU, memory, and GPU information for diagnostics, crash reports, and telemetry.                                                 |
| [`etw_tracing`](../../crates/etw_tracing/)                           | Windows Event Tracing for Windows recording control and workspace actions.                                                                   |

## Text, language, and language-server stack

The editing stack is deliberately split. `rope` stores text, `text` adds
collaborative buffer semantics, `language` adds syntax and language behavior,
and `multi_buffer` projects excerpts from one or more buffers for `editor`.

| Crate                                                      | Responsibility and hierarchy                                                                                                                                                    |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`text`](../../crates/text/)                               | Collaborative text `Buffer`, anchors, selections, edits, transactions, undo, network operations, patches, and snapshots; built on `rope`, `sum_tree`, and `clock`.              |
| [`language_core`](../../crates/language_core/)             | Low-dependency language definitions: grammar/config/query types, highlight maps, language names, toolchains, and language-server adapter contracts.                             |
| [`grammars`](../../crates/grammars/)                       | Embeds native Tree-sitter grammars and query/config assets and exposes feature-gated loaders to `language`.                                                                     |
| [`language`](../../crates/language/)                       | Syntax-aware buffers, Tree-sitter parsing, highlighting ranges, diagnostics, outlines, modelines, language registry, and language settings; extends `text` and `language_core`. |
| [`languages`](../../crates/languages/)                     | Registers Zed's built-in language configurations and language-server adapters for Rust, Python, TypeScript, C/C++, JSON, CSS, and others.                                       |
| [`lsp`](../../crates/lsp/)                                 | Language Server Protocol process, transport, request/notification, capability, selector, and binary-management abstractions.                                                    |
| [`lsp_locations`](../../crates/lsp_locations/)             | Picker UI for language-server locations such as definitions, declarations, implementations, references, and type definitions.                                                   |
| [`language_extension`](../../crates/language_extension/)   | Bridges extension-provided language servers and adapters into `language`, `lsp`, and `project`.                                                                                 |
| [`language_tools`](../../crates/language_tools/)           | Developer-facing syntax tree, highlight tree, key-context, language-server log, and status views.                                                                               |
| [`language_selector`](../../crates/language_selector/)     | Picker for changing an editor buffer's language and its active-language status control.                                                                                         |
| [`language_onboarding`](../../crates/language_onboarding/) | Language-specific onboarding notices, currently the BasedPyright suggestion for Python projects.                                                                                |
| [`toolchain_selector`](../../crates/toolchain_selector/)   | Picker and active-state control for selecting a language toolchain in a project.                                                                                                |
| [`json_schema_store`](../../crates/json_schema_store/)     | Loads, caches, matches, and serves JSON schemas and schema-derived completions from built-ins and extensions.                                                                   |
| [`prettier`](../../crates/prettier/)                       | Manages a Prettier server process and formats buffers through the project's Node runtime.                                                                                       |
| [`snippet`](../../crates/snippet/)                         | Parsed snippet representation, tab stops, variables, and insertion behavior used by completions and editors.                                                                    |
| [`snippet_provider`](../../crates/snippet_provider/)       | Loads, formats, registers, and looks up built-in, user, and extension snippets by language scope.                                                                               |
| [`snippets_ui`](../../crates/snippets_ui/)                 | UI for editing snippets and selecting their language scope.                                                                                                                     |

## Buffers, editor, search, and navigation

| Crate                                                        | Responsibility and hierarchy                                                                                                                                         |
| ------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`buffer_diff`](../../crates/buffer_diff/)                   | Maintains diff state between a live language buffer and a Git/custom base, including hunks, staged/secondary status, and incremental recomputation.                  |
| [`multi_buffer`](../../crates/multi_buffer/)                 | Combines excerpts from multiple language buffers into one editable coordinate space with anchors, transactions, and diff hunks.                                      |
| [`editor`](../../crates/editor/)                             | Zed's main text-editor model, element renderer, display maps, selections, input, scrolling, completions, diagnostics, code actions, hovers, inlays, and breadcrumbs. |
| [`search`](../../crates/search/)                             | Buffer and project search models and UI, search bar/options, match navigation, replacement, and status controls.                                                     |
| [`fuzzy`](../../crates/fuzzy/)                               | Zed's original fuzzy matcher for strings and paths, including character-bag prefiltering.                                                                            |
| [`fuzzy_nucleo`](../../crates/fuzzy_nucleo/)                 | Nucleo-based fuzzy matching implementation used by high-volume pickers and searches.                                                                                 |
| [`file_finder`](../../crates/file_finder/)                   | File-open picker, multi-selection, path matching, and recent/search result handling.                                                                                 |
| [`open_path_prompt`](../../crates/open_path_prompt/)         | Prompt for opening or creating a path, with path completion and file-finder settings.                                                                                |
| [`go_to_line`](../../crates/go_to_line/)                     | Line/column/offset parser and modal for navigating the active editor.                                                                                                |
| [`outline`](../../crates/outline/)                           | In-editor outline popover for symbols in the current buffer.                                                                                                         |
| [`outline_panel`](../../crates/outline_panel/)               | Persistent workspace panel showing searchable file symbols and project structure.                                                                                    |
| [`project_symbols`](../../crates/project_symbols/)           | Workspace-symbol picker backed by project language servers.                                                                                                          |
| [`call_hierarchy`](../../crates/call_hierarchy/)             | Incoming/outgoing language-server call hierarchy picker and navigation view.                                                                                         |
| [`encoding_selector`](../../crates/encoding_selector/)       | Picker and status item for reopening or saving the active buffer with a text encoding.                                                                               |
| [`line_ending_selector`](../../crates/line_ending_selector/) | Picker and status item for changing active-buffer line endings.                                                                                                      |
| [`tab_switcher`](../../crates/tab_switcher/)                 | Fuzzy recent-tab switcher and next/previous tab interaction.                                                                                                         |
| [`tabular_data_preview`](../../crates/tabular_data_preview/) | Read-only table preview, parser, renderer, and data engine for CSV/TSV-like files.                                                                                   |
| [`image_viewer`](../../crates/image_viewer/)                 | Workspace item for decoding, displaying, inspecting, and zooming image files.                                                                                        |
| [`svg_preview`](../../crates/svg_preview/)                   | Rendered SVG preview workspace item and editor integration.                                                                                                          |
| [`markdown`](../../crates/markdown/)                         | Parses and renders markdown, code blocks, links, selections, HTML, and Mermaid blocks as GPUI elements.                                                              |
| [`markdown_preview`](../../crates/markdown_preview/)         | Live workspace preview synchronized with markdown editor buffers and persisted preview settings.                                                                     |
| [`diagnostics`](../../crates/diagnostics/)                   | Project diagnostics view, buffer diagnostic navigation/rendering, toolbar controls, filtering, and editor integration.                                               |
| [`html_to_markdown`](../../crates/html_to_markdown/)         | Converts parsed HTML structure into normalized Markdown, used when ingesting rich model/tool content.                                                                |
| [`mermaid_render`](../../crates/mermaid_render/)             | Renders Mermaid source to raster-safe, Zed-themed SVG with diagram accent colors.                                                                                    |
| [`keymap_editor`](../../crates/keymap_editor/)               | Settings UI for searching, adding, editing, and resolving key bindings with action completion.                                                                       |
| [`vim`](../../crates/vim/)                                   | Vim and Helix modal editing state, motions, operators, text objects, registers, commands, and editor action overrides.                                               |
| [`vim_mode_setting`](../../crates/vim_mode_setting/)         | Small settings-only crate for enabling Vim or Helix mode without depending on the full `vim` implementation.                                                         |
| [`which_key`](../../crates/which_key/)                       | Which-key modal that shows available continuation bindings for a key sequence.                                                                                       |

## Settings, themes, and migration

| Crate                                                                  | Responsibility and hierarchy                                                                                                             |
| ---------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| [`settings_content`](../../crates/settings_content/)                   | Serializable schema for user/project settings content, split below the runtime settings store to avoid GPUI and filesystem dependencies. |
| [`settings_json`](../../crates/settings_json/)                         | Syntax-preserving operations for replacing or inserting values in JSON-with-comments settings text.                                      |
| [`settings_macros`](../../crates/settings_macros/)                     | Derives settings registration, merge behavior, and fallible option wrappers.                                                             |
| [`settings`](../../crates/settings/)                                   | Runtime settings store, sources, profiles, defaults, keymaps, EditorConfig, imports, observation, and project/worktree overrides.        |
| [`settings_ui`](../../crates/settings_ui/)                             | Searchable Settings Editor pages and controls generated from registered setting metadata.                                                |
| [`settings_profile_selector`](../../crates/settings_profile_selector/) | Picker for choosing and managing the active settings profile.                                                                            |
| [`migrator`](../../crates/migrator/)                                   | Ordered, syntax-aware migrations for renamed or structurally changed settings and keymap actions.                                        |
| [`syntax_theme`](../../crates/syntax_theme/)                           | Maps Tree-sitter capture names to highlight styles and merges bundled/user syntax overrides.                                             |
| [`theme`](../../crates/theme/)                                         | Core UI/icon theme models, schemas, registries, color spaces, appearance, font/scale data, and active-theme global state.                |
| [`theme_settings`](../../crates/theme_settings/)                       | Connects themes to settings, loads bundled/user themes and icon themes, and applies refinements.                                         |
| [`theme_extension`](../../crates/theme_extension/)                     | Loads and registers themes and icon themes contributed by installed extensions.                                                          |
| [`theme_selector`](../../crates/theme_selector/)                       | Theme and icon-theme pickers with live preview and telemetry.                                                                            |
| [`file_icons`](../../crates/file_icons/)                               | Resolves file/folder icon names and folder indicators through active icon-theme settings.                                                |

## Worktrees, projects, workspaces, and session UI

The codebase uses these terms at different levels. A `worktree` models one file
tree. A `project` coordinates one or more worktrees plus language servers, Git,
debugging, and other services. A `workspace` is the window-level UI containing
panes and items.

| Crate                                                  | Responsibility and hierarchy                                                                                                                                       |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [`worktree`](../../crates/worktree/)                   | Local and remote file-tree scanning, snapshots, entries, ignore handling, file loading, change observation, and worktree settings.                                 |
| [`project`](../../crates/project/)                     | Central project service coordinating worktrees, buffers, language servers, Git, search, tasks, debugger state, formatters, extensions, and remote synchronization. |
| [`workspace`](../../crates/workspace/)                 | Window-level panes, items, docks, modals, navigation history, persistence, notifications, task integration, project security, and multi-workspace management.      |
| [`session`](../../crates/session/)                     | Generates application session IDs and persists/restores window stacking information across launches.                                                               |
| [`project_panel`](../../crates/project_panel/)         | Project Panel tree UI for files, folders, worktrees, selection, drag/drop, file operations, and undo.                                                              |
| [`recent_projects`](../../crates/recent_projects/)     | Recent local/remote projects, SSH/WSL/dev-container connection entries, disconnected overlays, and recent-project sidebar UI.                                      |
| [`remote_connection`](../../crates/remote_connection/) | Modal and prompt UI for creating and editing SSH or WSL connection options.                                                                                        |
| [`journal`](../../crates/journal/)                     | Creates and opens date-based journal entries according to journal settings.                                                                                        |
| [`onboarding`](../../crates/onboarding/)               | First-run flow for keymap/theme selection, editor basics, and importing VS Code or Cursor settings.                                                                |
| [`dev_container`](../../crates/dev_container/)         | Parses devcontainer manifests/features, talks to Docker or Podman, and opens projects inside development containers.                                               |

## Git

| Crate                                                          | Responsibility and hierarchy                                                                                                                           |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [`git`](../../crates/git/)                                     | Git repository abstraction and CLI-backed implementation for status, blame, commits, branches, remotes, stashes, diffs, and hosting-provider metadata. |
| [`git_hosting_providers`](../../crates/git_hosting_providers/) | Detects configured Git remotes and supplies GitHub/GitLab/Bitbucket/source-host URL behavior.                                                          |
| [`git_ui_core`](../../crates/git_ui_core/)                     | Shared Git UI services and components: askpass, file diffs, branch/worktree pickers, notifications, and worktree creation.                             |
| [`git_ui`](../../crates/git_ui/)                               | Full Git Panel and workflows for staging, commits, branches, blame, graph, conflicts, stashes, repository selection, and multi-file diffs.             |

## Tasks, terminals, debugger, and REPL

| Crate                                                              | Responsibility and hierarchy                                                                                                   |
| ------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------ |
| [`task`](../../crates/task/)                                       | Low-level task templates, resolved tasks, variables, spawn specifications, and Zed/VS Code task/debug serialization formats.   |
| [`tasks_ui`](../../crates/tasks_ui/)                               | Task picker, task configuration helpers, and workspace actions for spawning/rerunning tasks.                                   |
| [`terminal`](../../crates/terminal/)                               | Headless terminal model around Alacritty and PTYs, terminal settings, ANSI parsing, search, and task metadata.                 |
| [`terminal_view`](../../crates/terminal_view/)                     | Interactive terminal workspace item, renderer, panel, scrollbars, persistence, blocks, links, and terminal actions.            |
| [`dap`](../../crates/dap/)                                         | Debug Adapter Protocol client, transport, adapter registry/types, settings, inline values, protocol conversion, and telemetry. |
| [`dap_adapters`](../../crates/dap_adapters/)                       | Built-in CodeLLDB, GDB, Go, JavaScript, and Python debug-adapter discovery and launch configuration.                           |
| [`debug_adapter_extension`](../../crates/debug_adapter_extension/) | Adapts extension-provided debug adapters and locator commands into the core DAP registry.                                      |
| [`debugger_tools`](../../crates/debugger_tools/)                   | Developer commands and views for inspecting debug-adapter launch arguments and protocol logs.                                  |
| [`debugger_ui`](../../crates/debugger_ui/)                         | Debug Panel, sessions, process/attach modals, stack frames, variables, breakpoints, console, and debugger persistence.         |
| [`repl`](../../crates/repl/)                                       | Jupyter kernel/session management, notebook outputs, inline REPL blocks, editor integration, and REPL UI/settings.             |

## Extensions

The host side and guest side are separate. `extension` and `extension_host`
live in the application. `zed_extension_api` is compiled into Rust/Wasm
extensions.

| Crate                                              | Responsibility and hierarchy                                                                                                                 |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| [`extension`](../../crates/extension/)             | Host-side extension manifests, capabilities, worktree/project delegates, Wasm proxy types, events, and extension builder.                    |
| [`extension_host`](../../crates/extension_host/)   | Installs, indexes, upgrades, loads, grants capabilities to, and executes extensions in Wasmtime; coordinates extension-contributed features. |
| [`zed_extension_api`](../../crates/extension_api/) | Guest-side public Rust API and generated Wasm bindings used by third-party Zed extensions. The package name differs from its folder name.    |
| [`extensions_ui`](../../crates/extensions_ui/)     | Extensions page, install/update/remove controls, search, categories, version selection, suggestions, and development-extension rebuilds.     |

## Client, protocol, and remote editing

There are two distinct network planes. `client`/`rpc` communicate with Zed's
cloud services. `remote` and `remote_server` connect the desktop UI to a
headless editor process. Both reuse protocol types where appropriate.

| Crate                                                | Responsibility and hierarchy                                                                                                                         |
| ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`proto`](../../crates/proto/)                       | Generated protobuf messages, typed envelopes, error conversion, and shared wire protocol between Zed clients and servers.                            |
| [`rpc`](../../crates/rpc/)                           | Peer-to-peer RPC connections, authentication, request/response routing, notifications, message streams, and reconnecting proto clients over `proto`. |
| [`client`](../../crates/client/)                     | Long-lived cloud client: authentication, reconnecting RPC, users, telemetry upload, proxy/TLS setup, URLs, and service clients.                      |
| [`cloud_api_types`](../../crates/cloud_api_types/)   | Serializable HTTP/WebSocket API types for accounts, organizations, plans, extensions, models, feedback, and system settings.                         |
| [`cloud_api_client`](../../crates/cloud_api_client/) | Typed HTTP and WebSocket client for Zed's cloud API, including LLM token acquisition.                                                                |
| [`remote`](../../crates/remote/)                     | Desktop-side remote-editing protocol, identity, transport, proxy process, and remote client state.                                                   |

## Updates, diagnostics, telemetry, and top-level actions

| Crate                                                | Responsibility and hierarchy                                                                                                  |
| ---------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| [`auto_update`](../../crates/auto_update/)           | Checks release channels, downloads update assets, records update state, and invokes the platform update helper.               |
| [`auto_update_ui`](../../crates/auto_update_ui/)     | Update notifications, release-note presentation, restart/install actions, and update-related prompt migration UI.             |
| [`install_cli`](../../crates/install_cli/)           | Workspace actions for installing the `zed` CLI and registering Zed URL/file handlers.                                         |
| [`crashes`](../../crates/crashes/)                   | Installs crash/panic handlers, gathers crash and GPU/user context, writes reports, and coordinates crash-server shutdown.     |
| [`feedback`](../../crates/feedback/)                 | In-app feedback/report form and submission, including system and extension diagnostics.                                       |
| [`telemetry_events`](../../crates/telemetry_events/) | Serializable event payloads shared by the application, client, evaluations, and telemetry uploader.                           |
| [`telemetry`](../../crates/telemetry/)               | Application telemetry initialization and event dispatch facade over `telemetry_events`.                                       |
| [`zed_actions`](../../crates/zed_actions/)           | Central declarations for cross-feature GPUI actions so crates can dispatch them without depending on feature implementations. |

## Maintaining this map

Use Cargo metadata to find additions and verify package/target names:

```sh
cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.manifest_path | contains("/crates/")) | .name' \
  | sort
```

That reports workspace packages. Also search for excluded or nested manifests:

```sh
rg --files crates -g Cargo.toml | sort
```

When a crate changes responsibility, check its `Cargo.toml`, crate root, public
modules, and direct internal dependencies before updating its entry. Do not
infer a dependency from naming alone: Cargo features and target-specific
dependencies make several apparently lower-level crates depend on adapters or
test-support crates only in selected builds.
