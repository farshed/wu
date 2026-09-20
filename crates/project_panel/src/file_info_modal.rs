use std::path::{Path, PathBuf};
use std::time::SystemTime;

use file_icons::FileIcons;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{DismissEvent, EventEmitter, FocusHandle, Focusable, Task, rems};
use time::{OffsetDateTime, UtcOffset};
use time_format::TimestampFormat;
use ui::{ElevationIndex, Modal, ModalFooter, ModalHeader, Section, prelude::*};
use util::size::format_file_size;
use workspace::ModalView;

const PROGRESS_INTERVAL: usize = 256;

enum Kind {
    File,
    Folder,
    Symlink(PathBuf),
}

struct Details {
    kind: Kind,
    created: Option<OffsetDateTime>,
    modified: Option<OffsetDateTime>,
    read_only: bool,
    executable: bool,
}

struct FolderTotals {
    size: u64,
    files: usize,
    folders: usize,
    scanning: bool,
}

enum State {
    Loading,
    Failed(String),
    Loaded {
        details: Details,
        size: u64,
        totals: Option<FolderTotals>,
    },
}

enum Progress {
    Details(Result<Details, String>, u64),
    Totals(FolderTotals),
}

pub struct FileInfoModal {
    path: PathBuf,
    state: State,
    focus_handle: FocusHandle,
    _load_task: Task<()>,
}

impl EventEmitter<DismissEvent> for FileInfoModal {}
impl ModalView for FileInfoModal {}

impl Focusable for FileInfoModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl FileInfoModal {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let (progress_tx, mut progress_rx) = mpsc::unbounded();
        cx.background_spawn({
            let path = path.clone();
            async move { load(path, progress_tx) }
        })
        .detach();

        let load_task = cx.spawn(async move |this, cx| {
            while let Some(progress) = progress_rx.next().await {
                let updated = this.update(cx, |this, cx| {
                    this.apply(progress);
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        Self {
            path,
            state: State::Loading,
            focus_handle: cx.focus_handle(),
            _load_task: load_task,
        }
    }

    fn apply(&mut self, progress: Progress) {
        match progress {
            Progress::Details(Err(error), _) => self.state = State::Failed(error),
            Progress::Details(Ok(details), size) => {
                let totals = matches!(details.kind, Kind::Folder).then(|| FolderTotals {
                    size: 0,
                    files: 0,
                    folders: 0,
                    scanning: true,
                });
                self.state = State::Loaded {
                    details,
                    size,
                    totals,
                };
            }
            Progress::Totals(new_totals) => {
                if let State::Loaded { totals, .. } = &mut self.state {
                    *totals = Some(new_totals);
                }
            }
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn copy_path(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
            self.path.to_string_lossy().into_owned(),
        ));
    }

    fn header_icon(&self, cx: &App) -> Option<SharedString> {
        let is_folder = matches!(
            &self.state,
            State::Loaded {
                details: Details {
                    kind: Kind::Folder,
                    ..
                },
                ..
            }
        );
        if is_folder {
            FileIcons::get_folder_icon(false, &self.path, cx)
        } else {
            FileIcons::get_icon(&self.path, cx)
        }
    }

    fn rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = vec![("Where", self.path.to_string_lossy().into_owned())];
        let State::Loaded {
            details,
            size,
            totals,
        } = &self.state
        else {
            return rows;
        };

        let kind = match &details.kind {
            Kind::File => "File".to_string(),
            Kind::Folder => "Folder".to_string(),
            Kind::Symlink(target) => format!("Symbolic link to {}", target.display()),
        };
        rows.push(("Kind", kind));

        match totals {
            Some(totals) => {
                let mut size_text = format_size(totals.size);
                if totals.scanning {
                    size_text.push_str(" (calculating…)");
                }
                rows.push(("Size", size_text));
                rows.push((
                    "Contains",
                    format!(
                        "{} {}, {} {}",
                        totals.files,
                        plural(totals.files, "file"),
                        totals.folders,
                        plural(totals.folders, "folder"),
                    ),
                ));
            }
            None => rows.push(("Size", format_size(*size))),
        }

        rows.push(("Created", format_timestamp(details.created)));
        rows.push(("Modified", format_timestamp(details.modified)));

        let mut permissions = vec![];
        if details.read_only {
            permissions.push("Read only");
        }
        if details.executable {
            permissions.push("Executable");
        }
        if !permissions.is_empty() {
            rows.push(("Permissions", permissions.join(", ")));
        }

        rows
    }
}

impl Render for FileInfoModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned());

        let mut section = Section::new();
        match &self.state {
            State::Loading => {
                section = section.child(Label::new("Loading…").color(Color::Muted));
            }
            State::Failed(error) => {
                section = section.child(Label::new(error.clone()).color(Color::Error));
            }
            State::Loaded { .. } => {}
        }
        for (label, value) in self.rows() {
            section = section.child(
                h_flex()
                    .w_full()
                    .items_start()
                    .gap_2()
                    .py_0p5()
                    .child(
                        div()
                            .w_20()
                            .flex_none()
                            .child(Label::new(label).color(Color::Muted)),
                    )
                    .child(div().flex_1().min_w_0().child(Label::new(value))),
            );
        }

        div()
            .track_focus(&self.focus_handle)
            .elevation_3(cx)
            .on_action(cx.listener(Self::cancel))
            .occlude()
            .w(rems(30.))
            .max_h(rems(40.))
            .child(
                Modal::new("file-info", None)
                    .header(
                        ModalHeader::new()
                            .show_dismiss_button(true)
                            .when_some(self.header_icon(cx), |header, icon| {
                                header.icon(Icon::from_path(icon).color(Color::Muted))
                            })
                            .child(Headline::new(name).size(HeadlineSize::Small)),
                    )
                    .section(section)
                    .footer(
                        ModalFooter::new().end_slot(
                            Button::new("copy-path", "Copy Path")
                                .style(ButtonStyle::Filled)
                                .layer(ElevationIndex::ModalSurface)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.copy_path(window, cx)),
                                ),
                        ),
                    ),
            )
    }
}

fn load(path: PathBuf, progress_tx: mpsc::UnboundedSender<Progress>) {
    let (details, size) = match read_details(&path) {
        Ok((details, size)) => (details, size),
        Err(error) => {
            progress_tx
                .unbounded_send(Progress::Details(Err(error.to_string()), 0))
                .ok();
            return;
        }
    };
    let is_folder = matches!(details.kind, Kind::Folder);
    if progress_tx
        .unbounded_send(Progress::Details(Ok(details), size))
        .is_err()
    {
        return;
    }
    if !is_folder {
        return;
    }

    let mut totals = FolderTotals {
        size: 0,
        files: 0,
        folders: 0,
        scanning: true,
    };
    let mut visited = 0;
    for entry in walkdir::WalkDir::new(&path).min_depth(1).into_iter() {
        let Ok(entry) = entry else {
            continue;
        };
        let file_type = entry.file_type();
        if file_type.is_dir() {
            totals.folders += 1;
        } else {
            totals.files += 1;
            if let Ok(metadata) = entry.metadata() {
                totals.size += metadata.len();
            }
        }
        visited += 1;
        if visited % PROGRESS_INTERVAL == 0 {
            let snapshot = FolderTotals {
                size: totals.size,
                files: totals.files,
                folders: totals.folders,
                scanning: true,
            };
            if progress_tx
                .unbounded_send(Progress::Totals(snapshot))
                .is_err()
            {
                return;
            }
        }
    }
    totals.scanning = false;
    progress_tx.unbounded_send(Progress::Totals(totals)).ok();
}

fn read_details(path: &Path) -> anyhow::Result<(Details, u64)> {
    let symlink_metadata = std::fs::symlink_metadata(path)?;
    let kind = if symlink_metadata.is_symlink() {
        Kind::Symlink(std::fs::read_link(path)?)
    } else if symlink_metadata.is_dir() {
        Kind::Folder
    } else {
        Kind::File
    };
    let metadata = std::fs::metadata(path).unwrap_or(symlink_metadata);
    let details = Details {
        kind,
        created: metadata.created().ok().map(OffsetDateTime::from),
        modified: metadata.modified().ok().map(OffsetDateTime::from),
        read_only: metadata.permissions().readonly(),
        executable: is_executable(&metadata),
    };
    Ok((details, metadata.len()))
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    !metadata.is_dir() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn format_size(size: u64) -> String {
    if size < 1000 {
        return format!("{} {}", size, plural(size as usize, "byte"));
    }
    format!(
        "{} ({} bytes)",
        format_file_size(size, true),
        with_thousands_separators(size)
    )
}

fn with_thousands_separators(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

fn format_timestamp(timestamp: Option<OffsetDateTime>) -> String {
    let Some(timestamp) = timestamp else {
        return "–".to_string();
    };
    let timezone = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    time_format::format_localized_timestamp(
        timestamp,
        OffsetDateTime::from(SystemTime::now()),
        timezone,
        TimestampFormat::EnhancedAbsolute,
    )
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    }
}
