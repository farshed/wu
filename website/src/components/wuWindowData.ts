export type Language = 'rust' | 'toml' | 'markdown';

export interface Hunk {
  start: number;
  end: number;
  kind: 'added' | 'modified';
  before?: string[];
}

export interface MockFile {
  path: string;
  language: Language;
  firstLine: number;
  modified?: boolean;
  hunks?: Hunk[];
  enclosingSymbol?: string;
  code: string;
}

export interface TreeFolder {
  name: string;
  path: string;
  children: TreeNode[];
}

export type TreeNode = TreeFolder | string;

export type ToolKind = 'run' | 'read' | 'edit' | 'search';

export type ChatStep =
  | { kind: 'thought'; text: string }
  | { kind: 'tool'; tool: ToolKind; detail: string; added?: number; removed?: number };

export type ChatEntry =
  | { kind: 'user'; text: string; time: string }
  | { kind: 'steps'; steps: ChatStep[] }
  | { kind: 'assistant'; text: string };

export interface MockChat {
  id: string;
  title: string;
  age: string;
  entries: ChatEntry[];
}

const paneRs = `    pub fn close_current_preview_item(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let item_idx = self.preview_item_idx()?;
        let id = self.preview_item_id()?;
        self.preview_item_id = None;

        let prev_active_item_index = self.active_item_index;
        self.remove_item(id, false, false, window, cx);
        self.active_item_index = prev_active_item_index;
        if item_idx < prev_active_item_index {
            self.active_item_index -= 1;
        }
        self.nav_history.0.lock().preview_item_id = None;

        if item_idx < self.items.len() {
            Some(item_idx)
        } else {
            None
        }
    }

    pub fn add_item_inner(
        &mut self,
        item: Box<dyn ItemHandle>,
        activate_pane: bool,
        focus_item: bool,
        activate: bool,
        destination_index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item_already_exists = self
            .items
            .iter()
            .any(|existing_item| existing_item.item_id() == item.item_id());

        if !item_already_exists {
            self.close_items_on_item_open(window, cx);
        }

        if item.is_singleton(cx)
            && let Some(&entry_id) = item.project_entry_ids(cx).first()
        {
            let Some(project) = self.project.upgrade() else {
                return;
            };
            let project = project.read(cx);
            if let Some(project_path) = project.path_for_entry(entry_id, cx) {
                let abs_path = project.absolute_path(&project_path, cx);
                self.nav_history
                    .0
                    .lock()
                    .paths_by_item
                    .insert(item.item_id(), (project_path, abs_path));
            }
        }
    }`;

const paneGroupRs = `use crate::{Pane, PaneAxis, SplitDirection};
use gpui::Entity;

#[derive(Clone)]
pub struct PaneGroup {
    pub root: Member,
    pub is_center: bool,
}

#[derive(Clone)]
pub enum Member {
    Axis(PaneAxis),
    Pane(Entity<Pane>),
}

impl PaneGroup {
    pub fn new(pane: Entity<Pane>) -> Self {
        Self {
            root: Member::Pane(pane),
            is_center: false,
        }
    }

    pub fn split(
        &mut self,
        old_pane: &Entity<Pane>,
        new_pane: &Entity<Pane>,
        direction: SplitDirection,
    ) {
        match &mut self.root {
            Member::Pane(pane) => {
                if pane == old_pane {
                    self.root = Member::new_axis(old_pane.clone(), new_pane.clone(), direction);
                }
            }
            Member::Axis(axis) => axis.split(old_pane, new_pane, direction),
        }
    }

    pub fn panes(&self) -> Vec<&Entity<Pane>> {
        let mut panes = Vec::new();
        self.root.collect_panes(&mut panes);
        panes
    }
}`;

const workspaceRs = `pub mod activity_bar;
pub mod dock;
pub mod item;
pub mod pane;
pub mod pane_group;
pub mod toolbar;

use gpui::{Context, Entity, Window, actions};
pub use pane::*;
pub use pane_group::*;

actions!(
    workspace,
    [
        /// Opens a new window.
        NewWindow,
        /// Closes the active item.
        CloseActiveItem,
        /// Splits the active pane to the right.
        SplitRight,
    ]
);

pub struct Workspace {
    center: PaneGroup,
    panes: Vec<Entity<Pane>>,
    active_pane: Entity<Pane>,
}

impl Workspace {
    pub fn active_pane(&self) -> &Entity<Pane> {
        &self.active_pane
    }

    pub fn split_pane(
        &mut self,
        pane: Entity<Pane>,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        let new_pane = self.add_pane(window, cx);
        self.center.split(&pane, &new_pane, direction);
        cx.notify();
        new_pane
    }
}`;

const workspaceToml = `[package]
name = "workspace"
version = "0.1.0"
edition.workspace = true
publish.workspace = true
license = "GPL-3.0-or-later"

[lib]
path = "src/workspace.rs"
doctest = false

[features]
test-support = ["gpui/test-support", "project/test-support"]

[dependencies]
anyhow.workspace = true
collections.workspace = true
gpui.workspace = true
project.workspace = true
serde.workspace = true
settings.workspace = true
theme.workspace = true
ui.workspace = true
util.workspace = true

[dev-dependencies]
gpui = { workspace = true, features = ["test-support"] }`;

const sessionRs = `use agent_harness::{AgentEvent, BackgroundTaskInfo};
use gpui::Context;
use std::time::Instant;

#[derive(Clone)]
pub struct BackgroundTask {
    pub task_id: String,
    pub description: String,
    pub started_at: Instant,
}

impl Session {
    pub fn handle_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::Text { text } => self.transcript.append_text(&text),
            AgentEvent::Compacting { active } => {
                self.compacting = active;
            }
            AgentEvent::BackgroundTasksChanged { tasks } => {
                self.background_tasks = tasks
                    .into_iter()
                    .map(|task| self.track_task(task))
                    .collect();
            }
            _ => {}
        }
        cx.notify();
    }

    pub fn background_tasks(&self) -> &[BackgroundTask] {
        &self.background_tasks
    }

    fn track_task(&self, task: BackgroundTaskInfo) -> BackgroundTask {
        let started_at = self
            .background_tasks
            .iter()
            .find(|known| known.task_id == task.task_id)
            .map_or_else(Instant::now, |known| known.started_at);
        BackgroundTask {
            task_id: task.task_id,
            description: task.description,
            started_at,
        }
    }
}`;

const rootToml = `[workspace]
resolver = "2"
members = [
    "crates/agent_chat",
    "crates/editor",
    "crates/gpui",
    "crates/workspace",
    "crates/wu",
]
default-members = ["crates/wu"]

[workspace.package]
edition = "2024"
publish = false

[workspace.dependencies]
agent_chat = { path = "crates/agent_chat" }
editor = { path = "crates/editor" }
gpui = { path = "crates/gpui", default-features = false }
workspace = { path = "crates/workspace" }

anyhow = "1.0.86"
serde = { version = "1.0.221", features = ["derive", "rc"] }
smol = "2.0"

[profile.release]
debug = "limited"
lto = "thin"
codegen-units = 1`;

const cargoLock = `# This file is automatically @generated by Cargo.
# It is not intended for manual editing.
version = 4

[[package]]
name = "agent_chat"
version = "0.1.0"
dependencies = [
 "agent_harness",
 "anyhow",
 "gpui",
 "markdown",
 "workspace",
]

[[package]]
name = "anyhow"
version = "1.0.98"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "e16d2d3311acee920a9eb8d33b8cbc1787ce4a264e85f964c2404b969bdcd487"

[[package]]
name = "smol"
version = "2.0.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "a33bd3e260892199c3ccfc487c88b2da2265080acb316cd920da72fdfd7c599f"`;

const readme = `# Wu

The fast, native code editor. Written in Rust, drawn on the GPU.

## Features

- Coding agents built in: chat with **Claude Code** or **Codex**
- Light on memory, quick to open and smooth on big projects
- Works with every Zed extension

## Building

Install Rust, then run \`cargo run --release\` from the repository root.

See [the docs](docs/) for how Wu differs from Zed.`;

export const files: MockFile[] = [
  {
    path: 'crates/workspace/src/pane.rs',
    language: 'rust',
    firstLine: 1198,
    modified: true,
    hunks: [
      {
        start: 9,
        end: 12,
        kind: 'modified',
        before: [
          '        let active_index = self.active_item_index;',
          '        self.remove_item(id, false, false, window, cx);',
          '        self.active_item_index = active_index;',
          '        if item_idx < active_index {'
        ]
      },
      { start: 15, end: 15, kind: 'added' }
    ],
    enclosingSymbol: 'impl Pane',
    code: paneRs
  },
  { path: 'crates/workspace/src/pane_group.rs', language: 'rust', firstLine: 1, code: paneGroupRs },
  { path: 'crates/workspace/src/workspace.rs', language: 'rust', firstLine: 1, code: workspaceRs },
  { path: 'crates/workspace/Cargo.toml', language: 'toml', firstLine: 1, code: workspaceToml },
  {
    path: 'crates/agent_chat/src/session.rs',
    language: 'rust',
    firstLine: 1,
    modified: true,
    hunks: [
      {
        start: 19,
        end: 22,
        kind: 'modified',
        before: ['                self.background_tasks = tasks.into_iter().map(BackgroundTask::from).collect();']
      },
      { start: 32, end: 44, kind: 'added' }
    ],
    code: sessionRs
  },
  { path: 'Cargo.lock', language: 'toml', firstLine: 1, code: cargoLock },
  { path: 'Cargo.toml', language: 'toml', firstLine: 1, code: rootToml },
  { path: 'README.md', language: 'markdown', firstLine: 1, modified: true, hunks: [{ start: 6, end: 6, kind: 'modified', before: ['- Built-in coding agents for **Claude Code** and **Codex**'] }],
    code: readme
  }
];

export const fileByPath = new Map(files.map((file) => [file.path, file]));

export const tree: TreeFolder = {
  name: 'wu',
  path: '',
  children: [
    { name: '.github', path: '.github', children: [] },
    { name: 'assets', path: 'assets', children: [] },
    {
      name: 'crates',
      path: 'crates',
      children: [
        {
          name: 'agent_chat',
          path: 'crates/agent_chat',
          children: [{ name: 'src', path: 'crates/agent_chat/src', children: ['crates/agent_chat/src/session.rs'] }]
        },
        {
          name: 'workspace',
          path: 'crates/workspace',
          children: [
            {
              name: 'src',
              path: 'crates/workspace/src',
              children: [
                'crates/workspace/src/pane.rs',
                'crates/workspace/src/pane_group.rs',
                'crates/workspace/src/workspace.rs'
              ]
            },
            'crates/workspace/Cargo.toml'
          ]
        }
      ]
    },
    { name: 'docs', path: 'docs', children: [] },
    'Cargo.lock',
    'Cargo.toml',
    'README.md'
  ]
};

export const initiallyExpanded = ['', 'crates', 'crates/workspace', 'crates/workspace/src'];

export const chats: MockChat[] = [
  {
    id: 'preview-tests',
    title: 'Add a test for preview tabs',
    age: '2m',
    entries: [
      { kind: 'user', text: 'Add a test that closing the preview tab keeps the right tab active.', time: 'Oct 5, 3:42 PM' },
      {
        kind: 'steps',
        steps: [
          {
            kind: 'thought',
            text: 'The logic lives in `close_current_preview_item` in pane.rs. It removes the preview item and moves `active_item_index` back by one when the closed tab sat before the active one. A good test opens three tabs, makes the first one a preview, activates the last one, closes the preview and checks which tab is active.'
          },
          { kind: 'tool', tool: 'read', detail: 'crates/workspace/src/pane.rs' },
          { kind: 'tool', tool: 'search', detail: 'fn add_labeled_item in crates/workspace' },
          { kind: 'tool', tool: 'read', detail: 'crates/workspace/src/pane/tests.rs' },
          { kind: 'tool', tool: 'edit', detail: 'crates/workspace/src/pane/tests.rs', added: 21 },
          { kind: 'tool', tool: 'run', detail: 'cargo test -p workspace preview' }
        ]
      },
      {
        kind: 'assistant',
        text: 'Added `closing_preview_tab_keeps_active_tab` to the pane tests. It opens three tabs, makes the first one a preview, activates the last one and closes the preview:\n\n```rust\n#[gpui::test]\nasync fn closing_preview_tab_keeps_active_tab(cx: &mut TestAppContext) {\n    let (pane, cx) = init_pane(cx).await;\n    add_labeled_item(&pane, "A", false, cx);\n    add_labeled_item(&pane, "B", false, cx);\n    add_labeled_item(&pane, "C", false, cx);\n    set_preview_item(&pane, "A", cx);\n\n    pane.update_in(cx, |pane, window, cx| {\n        pane.activate_item(2, false, false, window, cx);\n        pane.close_current_preview_item(window, cx);\n    });\n\n    assert_item_labels(&pane, ["B", "C*"], cx);\n}\n```\n\nThe `*` marks the active tab, so the test checks that C is still active after A goes away. It passes, along with the other 213 tests in `workspace`.'
      },
      { kind: 'user', text: 'Also cover the case where the preview tab is after the active one.', time: 'Oct 5, 3:45 PM' },
      {
        kind: 'steps',
        steps: [
          {
            kind: 'thought',
            text: 'When the preview tab sits after the active one, `item_idx < prev_active_item_index` is false, so the index must stay the same. Same setup, but the preview goes last and the first tab stays active.'
          },
          { kind: 'tool', tool: 'edit', detail: 'crates/workspace/src/pane/tests.rs', added: 17 },
          { kind: 'tool', tool: 'run', detail: 'cargo test -p workspace preview' }
        ]
      },
      {
        kind: 'assistant',
        text: 'Added `closing_later_preview_tab_keeps_active_tab`. Both cases now pass:\n\n- **Preview before the active tab:** the active index moves back by one, so the same tab stays active.\n- **Preview after the active tab:** the active index stays where it is.\n\nI kept both tests next to the existing preview tests so they are easy to find.'
      }
    ]
  },
  {
    id: 'pane-groups',
    title: 'How do pane splits work?',
    age: '1h',
    entries: [
      { kind: 'user', text: 'How does splitting a pane work?', time: 'Oct 5, 2:10 PM' },
      {
        kind: 'steps',
        steps: [
          { kind: 'tool', tool: 'read', detail: 'crates/workspace/src/pane_group.rs' },
          { kind: 'tool', tool: 'search', detail: 'fn new_axis in crates/workspace' }
        ]
      },
      {
        kind: 'assistant',
        text: "The center of each window is a `PaneGroup`. Its `root` is a tree: each node is either a single `Pane` or an `Axis` that holds more members side by side or stacked.\n\n```rust\npub enum Member {\n    Axis(PaneAxis),\n    Pane(Entity<Pane>),\n}\n```\n\n- `split` finds the pane you're splitting and swaps it for a new axis that holds the old pane and the new one.\n- If the parent axis already runs in the same direction, the new pane is added to it instead of nesting another axis.\n- `panes` walks the tree and returns every pane, left to right and top to bottom."
      },
      { kind: 'user', text: 'Where does the split direction come from?', time: 'Oct 5, 2:12 PM' },
      {
        kind: 'steps',
        steps: [
          { kind: 'tool', tool: 'search', detail: 'SplitDirection in crates/workspace' },
          { kind: 'tool', tool: 'read', detail: 'crates/workspace/src/workspace.rs' }
        ]
      },
      {
        kind: 'assistant',
        text: 'From the action you run. `SplitRight`, `SplitLeft`, `SplitUp` and `SplitDown` each map to a `SplitDirection`, and `Workspace::split_pane` passes it on to the pane group.\n\nDragging a tab to the edge of a pane does the same thing: the pane works out which edge you dropped on and picks the direction from that.'
      }
    ]
  },
  {
    id: 'background-tasks',
    title: 'Track background commands',
    age: '1d',
    entries: [
      { kind: 'user', text: 'Keep track of which background commands are still running.', time: 'Oct 4, 6:05 PM' },
      {
        kind: 'steps',
        steps: [
          {
            kind: 'thought',
            text: 'The CLI sends the full list of running tasks whenever it changes. The session can replace its list each time and keep the start time of tasks it already knew about.'
          },
          { kind: 'tool', tool: 'read', detail: 'crates/agent_chat/src/session.rs' },
          { kind: 'tool', tool: 'edit', detail: 'crates/agent_chat/src/session.rs', added: 41, removed: 6 },
          { kind: 'tool', tool: 'run', detail: 'cargo test -p agent_chat background' }
        ]
      },
      {
        kind: 'assistant',
        text: 'Sessions now keep a list of running background tasks. `background_tasks()` returns them with the time each one started, so the chat can show how long each one has been running.\n\nWhen a run ends, the list is cleared, so a stopped agent never leaves a task stuck on screen.'
      },
      { kind: 'user', text: 'Show them under the chat box, with how long each one has been running.', time: 'Oct 4, 6:20 PM' },
      {
        kind: 'steps',
        steps: [
          { kind: 'tool', tool: 'read', detail: 'crates/agent_chat/src/chat_view/composer.rs' },
          { kind: 'tool', tool: 'edit', detail: 'crates/agent_chat/src/chat_view/background_tasks.rs', added: 118 },
          { kind: 'tool', tool: 'edit', detail: 'crates/agent_chat/src/chat_view/composer.rs', added: 1 },
          { kind: 'tool', tool: 'run', detail: 'cargo test -p agent_chat background' }
        ]
      },
      {
        kind: 'assistant',
        text: 'The bar under the chat box now shows a small pill while anything runs in the background:\n\n- **One command:** "Command running" with the elapsed time.\n- **Several:** "3 tasks running".\n- **Click it** to see each command, or to open a subagent in its own tab.\n\nThe time updates once a second and stops as soon as the last task ends.'
      }
    ]
  }
];

export const previewReply =
  "This is only a preview, so there's no agent behind it. In Wu, Claude Code or Codex answers right here, using the CLI you already have signed in.";

export type TokenKind =
  | 'keyword'
  | 'type'
  | 'function'
  | 'string'
  | 'number'
  | 'comment'
  | 'property'
  | 'punctuation'
  | 'self'
  | 'constant'
  | 'attribute'
  | 'title'
  | 'emphasis'
  | 'link'
  | 'marker'
  | 'literal';

export interface Token {
  text: string;
  kind?: TokenKind;
}

const rustKeywords = new Set(
  'as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return static struct super trait type unsafe use where while'.split(
    ' '
  )
);

function highlightRust(line: string): Token[] {
  const tokens: Token[] = [];
  const pattern =
    /(\/\/.*)|("(?:[^"\\]|\\.)*")|(#\[[^\]]*\])|(\b\d[\d_]*(?:\.\d+)?\b)|([A-Za-z_][A-Za-z0-9_]*)|(->|=>|==|<=|>=|&&|\|\||[-+*/=<>!&|?]=?)|([{}()[\];,.:@'])|(\s+)|(.)/g;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(line))) {
    const [text, comment, string, attribute, number, word, operator] = match;
    if (comment) tokens.push({ text, kind: 'comment' });
    else if (string) tokens.push({ text, kind: 'string' });
    else if (attribute) tokens.push({ text, kind: 'attribute' });
    else if (number) tokens.push({ text, kind: 'number' });
    else if (word) tokens.push({ text, kind: classifyRustWord(word, line, match.index) });
    else if (operator) tokens.push({ text, kind: 'punctuation' });
    else tokens.push({ text, kind: /\s/.test(text) ? undefined : 'punctuation' });
  }
  return tokens;
}

function classifyRustWord(word: string, line: string, index: number): TokenKind | undefined {
  const next = line.slice(index + word.length);
  const previous = line.slice(0, index);
  if (word === 'self') return 'self';
  if (rustKeywords.has(word)) return 'keyword';
  if (word === 'true' || word === 'false') return 'constant';
  if (next.startsWith('!')) return 'function';
  if (/^[A-Z][A-Z0-9_]+$/.test(word)) return 'constant';
  if (/^[A-Z]/.test(word)) return 'type';
  if (next.startsWith('(') || next.startsWith('::<')) return 'function';
  if (previous.endsWith('.')) return 'property';
  return undefined;
}

function highlightToml(line: string): Token[] {
  if (/^\s*#/.test(line)) return [{ text: line, kind: 'comment' }];
  const header = /^(\s*)(\[\[?)([^\]]+)(\]\]?)(.*)$/.exec(line);
  if (header) {
    const [, indent = '', open = '', name = '', close = '', rest = ''] = header;
    return [{ text: indent }, { text: open, kind: 'punctuation' }, { text: name, kind: 'type' }, { text: close, kind: 'punctuation' }, { text: rest }];
  }
  const tokens: Token[] = [];
  const keyMatch = /^(\s*)([A-Za-z0-9_.-]+)(\s*=)/.exec(line);
  let rest = line;
  if (keyMatch) {
    const [whole, indent = '', key = '', equals = ''] = keyMatch;
    tokens.push({ text: indent }, { text: key, kind: 'property' }, { text: equals, kind: 'punctuation' });
    rest = line.slice(whole.length);
  }
  const pattern = /("(?:[^"\\]|\\.)*")|(\b\d[\d.]*\b)|(\btrue\b|\bfalse\b)|([A-Za-z_][\w-]*)(?=\s*=)|([[\]{},=])|(\s+|[^\s"[\]{},=]+|.)/g;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(rest))) {
    const [text, string, number, boolean, key, punctuation] = match;
    if (string) tokens.push({ text, kind: 'string' });
    else if (number) tokens.push({ text, kind: 'number' });
    else if (boolean) tokens.push({ text, kind: 'constant' });
    else if (key) tokens.push({ text, kind: 'property' });
    else if (punctuation) tokens.push({ text, kind: 'punctuation' });
    else tokens.push({ text });
  }
  return tokens;
}

function highlightMarkdown(line: string): Token[] {
  if (/^#{1,6}\s/.test(line)) return [{ text: line, kind: 'title' }];
  const tokens: Token[] = [];
  let rest = line;
  const marker = /^(\s*[-*]\s)/.exec(rest);
  if (marker?.[1]) {
    tokens.push({ text: marker[1], kind: 'marker' });
    rest = rest.slice(marker[1].length);
  }
  const pattern = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\[[^\]]+\])(\([^)]+\))|([^`*[]+|[`*[])/g;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(rest))) {
    const [text, code, strong, linkText, linkUrl] = match;
    if (code) tokens.push({ text, kind: 'literal' });
    else if (strong) tokens.push({ text, kind: 'emphasis' });
    else if (linkText && linkUrl) tokens.push({ text: linkText, kind: 'link' }, { text: linkUrl, kind: 'attribute' });
    else tokens.push({ text });
  }
  return tokens;
}

export function highlight(line: string, language: Language): Token[] {
  switch (language) {
    case 'rust':
      return highlightRust(line);
    case 'toml':
      return highlightToml(line);
    case 'markdown':
      return highlightMarkdown(line);
  }
}

export interface OutlineItem {
  label: string;
  line: number;
  depth: number;
}

export function outline(file: MockFile): OutlineItem[] {
  const items: OutlineItem[] = file.enclosingSymbol ? [{ label: file.enclosingSymbol, line: 0, depth: 0 }] : [];
  file.code.split('\n').forEach((text, index) => {
    if (file.language === 'rust') {
      const match = /^(\s*)(?:pub(?:\([^)]*\))?\s+)?(fn|struct|enum|impl|trait|mod)\s+([^({;]+)/.exec(text);
      if (match) {
        const [, indent = '', keyword = '', name = ''] = match;
        const depth = Math.floor(indent.length / 4);
        items.push({ label: `${keyword} ${name.trim()}`, line: index, depth });
      }
    } else if (file.language === 'toml') {
      const match = /^\[\[?([^\]]+)\]\]?/.exec(text);
      if (match?.[1]) items.push({ label: match[1], line: index, depth: 0 });
    } else {
      const match = /^(#{1,6})\s+(.*)$/.exec(text);
      if (match?.[1] && match[2]) items.push({ label: match[2], line: index, depth: match[1].length - 1 });
    }
  });
  return items;
}

export function fileName(path: string) {
  return path.slice(path.lastIndexOf('/') + 1);
}

const folderIcons: Record<string, string> = {
  '.github': 'file-folder-github',
  assets: 'file-folder-assets',
  docs: 'file-folder-documents',
  src: 'file-folder-orange-code'
};

export function folderIcon(name: string) {
  return `/wu-ui/${folderIcons[name] ?? 'file-folder'}.svg`;
}

export function fileIcon(path: string) {
  if (path.endsWith('.rs')) return '/wu-ui/file-rust.svg';
  if (path.endsWith('.toml')) return '/wu-ui/file-toml.svg';
  if (path.endsWith('.md')) return '/wu-ui/file-markdown.svg';
  if (path.endsWith('.lock')) return '/wu-ui/file-lock.svg';
  return '/wu-ui/file-document.svg';
}

export function languageName(language: Language) {
  return { rust: 'Rust', toml: 'TOML', markdown: 'Markdown' }[language];
}
