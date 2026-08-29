use std::sync::Arc;

use serde::{Deserialize, Serialize};
use strum::{EnumIter, EnumString, IntoStaticStr};

#[derive(
    Debug, PartialEq, Eq, Copy, Clone, EnumIter, EnumString, IntoStaticStr, Serialize, Deserialize,
)]
#[strum(serialize_all = "snake_case")]
pub enum IconName {
    Archive,
    ArrowCircle,
    ArrowDown,
    ArrowDown10,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowUpRight,
    Attach,
    AudioOff,
    Backspace,
    Bell,
    Binary,
    Bitbucket,
    Blocks,
    Bookmark,
    BoltFilled,
    BoltOutlined,
    Book,
    Box,
    BoxOpen,
    CaseSensitive,
    Chat,
    Check,
    ChevronDown,
    ChevronDownUp,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronUpDown,
    Circle,
    CircleHelp,
    Clock,
    Close,
    CloudDownload,
    Code,
    Codeberg,
    Command,
    Compact,
    Control,
    Copy,
    Crosshair,
    CursorIBeam,
    Dash,
    Debug,
    DebugBreakpoint,
    DebugContinue,
    DebugContinueThread,
    DebugDetach,
    DebugDisabledBreakpoint,
    DebugDisabledLogBreakpoint,
    DebugLogBreakpoint,
    DebugPause,
    DebugStepInto,
    DebugStepOut,
    DebugStepOver,
    Diff,
    DiffSplit,
    DiffSplitAuto,
    DiffUnified,
    Disconnected,
    Download,
    EditorAtom,
    EditorCursor,
    EditorEmacs,
    EditorJetBrains,
    EditorSublime,
    EditorVsCode,
    Ellipsis,
    Envelope,
    Eraser,
    Escape,
    Exit,
    ExpandDown,
    ExpandUp,
    ExpandVertical,
    Eye,
    EyeOff,
    File,
    FileDiff,
    FileDoc,
    FileGit,
    FileIgnored,
    FileLock,
    FileMultiple,
    FileTree,
    Filter,
    Flame,
    FoldVertical,
    Folder,
    FolderInclude,
    FolderOpen,
    FolderSearch,
    Font,
    FontSize,
    FontWeight,
    Forgejo,
    GenericClose,
    GenericMaximize,
    GenericMinimize,
    GenericRestore,
    Gerrit,
    GitBranch,
    GitBranchPlus,
    GitCommit,
    GitGraph,
    GitWorktree,
    Gitea,
    Github,
    Gitlab,
    Hash,
    HistoryRerun,
    Image,
    Indicator,
    Info,
    Json,
    Keyboard,
    LineHeight,
    Link,
    Linux,
    ListCollapse,
    ListTree,
    LoadCircle,
    LocationEdit,
    Lock,
    MagnifyingGlass,
    Maximize,
    MaximizeAlt,
    Menu,
    MicMute,
    Minimize,
    Notepad,
    Option,
    PageDown,
    PageUp,
    Pencil,
    Person,
    Pin,
    PlayFilled,
    PlayOutlined,
    Plus,
    Power,
    Public,
    PullRequest,
    Quote,
    Regex,
    Replace,
    ReplaceAll,
    ReplaceNext,
    ReplyArrowRight,
    Rerun,
    Return,
    RotateCcw,
    RotateCw,
    Screen,
    SelectAll,
    Send,
    Server,
    Settings,
    Shift,
    Slash,
    Sourcehut,
    Space,
    Sparkle,
    Split,
    SplitAlt,
    SquareDot,
    SquareMinus,
    SquarePlus,
    Star,
    Stop,
    Tab,
    Table,
    Terminal,
    TerminalAlt,
    TextWrap,
    TextUnwrap,
    ThisWindow,
    ToolWeb,
    Trash,
    Triangle,
    Undo,
    Unpin,
    UserCheck,
    Warning,
    Wu,
    WholeWord,
    XCircle,
}

impl IconName {
    /// Returns the path to this icon.
    pub fn path(&self) -> Arc<str> {
        let file_stem: &'static str = self.into();
        format!("icons/{file_stem}.svg").into()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use strum::{IntoEnumIterator as _, ParseError};

    use crate::IconName;

    #[test]
    fn test_all_icons_exist() {
        let asset_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");

        for icon in IconName::iter() {
            let icon_path = asset_path.join(&*icon.path());
            assert!(
                icon_path.exists(),
                "Icon {icon:?} does not exist at {icon_path:?}",
            );
        }
    }

    #[test]
    fn test_no_dangling_icons() -> Result<(), ParseError> {
        let icons_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons");

        for entry in std::fs::read_dir(&icons_dir).expect("failed to read icons directory") {
            let path = entry.expect("failed to read icons directory entry").path();
            if path.extension().is_none_or(|extension| extension != "svg") {
                continue;
            }
            let file_stem = path
                .file_stem()
                .and_then(|file_stem| file_stem.to_str())
                .expect("icon file name is not valid UTF-8");

            file_stem.parse::<IconName>()?;
        }

        Ok(())
    }
}
