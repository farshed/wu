use super::ChatView;
use editor::Editor;
use gpui::{
    AnyElement, ClipboardItem, Context, DismissEvent, Entity, IntoElement, ParentElement,
    Focusable as _, Pixels, Point, SharedString, Subscription, Window, anchored, deferred,
};
use std::path::{Path, PathBuf};
use ui::{ContextMenu, prelude::*};
use util::ResultExt as _;
use workspace::{OpenOptions, OpenVisible};

const FILE_SCHEME: &str = "wu-file:";
const RECHECK_MISSING: std::time::Duration = std::time::Duration::from_secs(5);

pub(super) struct PathState {
    exists: bool,
    checked: std::time::Instant,
    pending: bool,
}

fn inline_code_spans(markdown: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        spans.extend(
            line.split('`')
                .skip(1)
                .step_by(2)
                .filter(|span| looks_like_path(span))
                .map(str::to_string),
        );
    }
    spans
}

pub(super) struct LinkMenu {
    pub menu: Entity<ContextMenu>,
    pub position: Point<Pixels>,
    _dismiss: Subscription,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct FileTarget {
    pub path: PathBuf,
    pub line: Option<u32>,
}

fn split_line(text: &str) -> (&str, Option<u32>) {
    if let Some((path, anchor)) = text.rsplit_once("#L") {
        let line = anchor.split(['-', 'C', 'L']).next().and_then(|line| line.parse().ok());
        return (path, line);
    }
    let mut parts = text.rsplitn(3, ':');
    let last = parts.next();
    let middle = parts.next();
    let rest = parts.next();
    match (rest, middle, last) {
        (Some(path), Some(line), Some(column))
            if line.parse::<u32>().is_ok() && column.parse::<u32>().is_ok() =>
        {
            (path, line.parse().ok())
        }
        (_, Some(_), Some(line)) if line.parse::<u32>().is_ok() => {
            let path_len = text.len() - line.len() - 1;
            (&text[..path_len], line.parse().ok())
        }
        _ => (text, None),
    }
}

pub(super) fn file_target(url: &str, cwd: &Path) -> Option<FileTarget> {
    let text = if let Some(path) = url.strip_prefix(FILE_SCHEME) {
        path
    } else if let Some(path) = url.strip_prefix("file://") {
        path
    } else if url.contains("://") || url.starts_with("mailto:") || url.starts_with('#') {
        return None;
    } else {
        url
    };
    let (path, line) = split_line(text);
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    Some(FileTarget { path, line })
}

fn looks_like_path(text: &str) -> bool {
    !text.is_empty()
        && text.len() < 300
        && !text.contains(char::is_whitespace)
        && !text.contains("://")
        && (text.contains('/') || text.rsplit_once('.').is_some_and(|(stem, extension)| {
            !stem.is_empty() && (1..=6).contains(&extension.len())
                && extension.chars().all(char::is_alphanumeric)
        }))
}

fn markdown_id(entry: &crate::session::Entry) -> gpui::EntityId {
    match entry {
        crate::session::Entry::Assistant { markdown, .. } => markdown.entity_id(),
        _ => gpui::EntityId::default(),
    }
}

impl ChatView {
    pub(super) fn code_span_url(&self, text: &str, cwd: &Path) -> Option<SharedString> {
        if !looks_like_path(text) {
            return None;
        }
        let target = file_target(text, cwd)?;
        self.known_paths
            .get(&target.path)
            .is_some_and(|state| state.exists)
            .then(|| format!("{FILE_SCHEME}{text}").into())
    }

    pub(super) fn refresh_path_links(&mut self, cx: &mut Context<Self>) {
        let cwd = self.session.read(cx).metadata().cwd.clone();
        let mut candidates: Vec<PathBuf> = Vec::new();
        for entry in self.entries(cx) {
            if let crate::session::Entry::Assistant { markdown, .. } = entry {
                let markdown = markdown.read(cx);
                let length = markdown.source().len();
                if self.scanned_sources.get(&markdown_id(entry)) == Some(&length) {
                    continue;
                }
                candidates.extend(
                    inline_code_spans(markdown.source())
                        .iter()
                        .filter_map(|span| file_target(span, &cwd))
                        .map(|target| target.path),
                );
            }
        }
        let scanned: Vec<(gpui::EntityId, usize)> = self
            .entries(cx)
            .iter()
            .filter_map(|entry| match entry {
                crate::session::Entry::Assistant { markdown, .. } => {
                    Some((markdown_id(entry), markdown.read(cx).source().len()))
                }
                _ => None,
            })
            .collect();
        self.scanned_sources.extend(scanned);
        candidates.extend(self.known_paths.iter().filter_map(|(path, state)| {
            (!state.exists && !state.pending && state.checked.elapsed() >= RECHECK_MISSING)
                .then(|| path.clone())
        }));
        candidates.retain(|path| {
            self.known_paths
                .get(path)
                .is_none_or(|state| !state.pending && !state.exists)
        });
        candidates.sort();
        candidates.dedup();
        candidates.retain(|path| {
            self.known_paths.get(path).is_none_or(|state| {
                state.checked.elapsed() >= RECHECK_MISSING
            })
        });
        if candidates.is_empty() {
            return;
        }
        for path in &candidates {
            self.known_paths.insert(
                path.clone(),
                PathState {
                    exists: false,
                    checked: std::time::Instant::now(),
                    pending: true,
                },
            );
        }
        let check = cx.background_spawn(async move {
            candidates
                .into_iter()
                .map(|path| {
                    let exists = path.exists();
                    (path, exists)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = check.await;
            this.update(cx, |this, cx| {
                let mut found = false;
                for (path, exists) in results {
                    found |= exists;
                    this.known_paths.insert(
                        path,
                        PathState {
                            exists,
                            checked: std::time::Instant::now(),
                            pending: false,
                        },
                    );
                }
                if found {
                    cx.notify();
                }
            })
            .log_err();
        })
        .detach();
    }

    pub(super) fn open_link(&mut self, url: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.session.read(cx).metadata().cwd.clone();
        match file_target(&url, &cwd) {
            Some(target) if target.path.exists() => self.open_file(target, window, cx),
            Some(_) if url.starts_with(FILE_SCHEME) => {}
            _ => cx.open_url(&url),
        }
    }

    fn open_file(&mut self, target: FileTarget, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let opening = workspace.update(cx, |workspace, cx| {
            workspace.open_abs_path(
                target.path.clone(),
                OpenOptions {
                    visible: Some(OpenVisible::None),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        let line = target.line;
        cx.spawn_in(window, async move |_, cx| {
            let item = opening.await?;
            if let (Some(line), Some(editor)) = (line, item.downcast::<Editor>()) {
                editor.update_in(cx, |editor, window, cx| {
                    editor.go_to_singleton_buffer_point(
                        language::Point::new(line.saturating_sub(1), 0),
                        window,
                        cx,
                    )
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    pub(super) fn show_link_menu(
        &mut self,
        url: SharedString,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.session.read(cx).metadata().cwd.clone();
        let file = file_target(&url, &cwd).filter(|target| target.path.exists());
        let view = cx.entity().downgrade();
        let menu = ContextMenu::build(window, cx, move |menu, _, _| match file {
            Some(target) => {
                let path = target.path.clone();
                let open_view = view.clone();
                let open_target = target;
                menu.entry("Open", None, move |window, cx| {
                    open_view
                        .update(cx, |this, cx| this.open_file(open_target.clone(), window, cx))
                        .log_err();
                })
                .entry("Open with Default App", None, {
                    let path = path.clone();
                    move |_, cx| cx.open_with_system(&path)
                })
                .entry("Show in Folder", None, {
                    let path = path.clone();
                    move |_, cx| cx.reveal_path(&path)
                })
                .separator()
                .entry("Copy Path", None, move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        path.to_string_lossy().into_owned(),
                    ))
                })
            }
            None => {
                let open_url = url.clone();
                let copy_url = url.clone();
                menu.entry("Open Link", None, move |_, cx| cx.open_url(&open_url))
                    .entry("Copy Link", None, move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_url.to_string()))
                    })
            }
        });
        let dismiss = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.link_menu = None;
            cx.notify();
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.link_menu = Some(LinkMenu {
            menu,
            position,
            _dismiss: dismiss,
        });
        cx.notify();
    }

    pub(super) fn render_link_menu(&self) -> Option<AnyElement> {
        let link_menu = self.link_menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(link_menu.position)
                    .anchor(gpui::Anchor::TopLeft)
                    .child(link_menu.menu.clone()),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_targets_understand_lines_and_schemes() {
        let cwd = Path::new("/repo");
        assert_eq!(
            file_target("src/main.rs:12", cwd),
            Some(FileTarget {
                path: "/repo/src/main.rs".into(),
                line: Some(12)
            })
        );
        assert_eq!(
            file_target("wu-file:/abs/a.rs:3:9", cwd),
            Some(FileTarget {
                path: "/abs/a.rs".into(),
                line: Some(3)
            })
        );
        assert_eq!(
            file_target("lib.rs#L40-L42", cwd),
            Some(FileTarget {
                path: "/repo/lib.rs".into(),
                line: Some(40)
            })
        );
        assert_eq!(file_target("https://example.com/a.rs", cwd), None);
        assert_eq!(
            inline_code_spans("See `src/a.rs` and `Vec<T>`\n```\n`inside/fence.rs`\n```\n"),
            ["src/a.rs"]
        );
        assert!(looks_like_path("crates/a/b.rs"));
        assert!(looks_like_path("Cargo.toml"));
        assert!(!looks_like_path("Vec<T>"));
        assert!(!looks_like_path("foo bar.rs"));
    }
}
