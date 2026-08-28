pub mod running;

use crate::{persistence::SerializedLayout, session::running::DebugTerminal};
use dap::client::SessionId;
use gpui::{App, Axis, Entity, EventEmitter, FocusHandle, Focusable, WeakEntity};
use project::debugger::session::Session;

use project::{Project, debugger::session::SessionQuirks};
use running::RunningState;
use ui::prelude::*;
use workspace::{Workspace, item::Item};

pub struct DebugSession {
    pub(crate) running_state: Entity<RunningState>,
    pub(crate) quirks: SessionQuirks,
}

impl DebugSession {
    pub(crate) fn running(
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        parent_terminal: Option<Entity<DebugTerminal>>,
        session: Entity<Session>,
        serialized_layout: Option<SerializedLayout>,
        dock_axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let running_state = cx.new(|cx| {
            RunningState::new(
                session.clone(),
                project.clone(),
                workspace.clone(),
                parent_terminal,
                serialized_layout,
                dock_axis,
                window,
                cx,
            )
        });
        let quirks = session.read(cx).quirks();

        cx.new(|_| Self {
            running_state,
            quirks,
        })
    }

    pub(crate) fn session_id(&self, cx: &App) -> SessionId {
        self.running_state.read(cx).session_id()
    }

    pub fn session(&self, cx: &App) -> Entity<Session> {
        self.running_state.read(cx).session().clone()
    }

    pub(crate) fn shutdown(&mut self, cx: &mut Context<Self>) {
        self.running_state
            .update(cx, |state, cx| state.shutdown(cx));
    }

    pub(crate) fn label(&self, cx: &mut App) -> Option<SharedString> {
        let session = self.running_state.read(cx).session().clone();
        session.update(cx, |session, cx| {
            let session_label = session.label();
            let quirks = session.quirks();
            let mut single_thread_name = || {
                let threads = session.threads(cx);
                match threads.as_slice() {
                    [(thread, _)] => Some(SharedString::from(&thread.name)),
                    _ => None,
                }
            };
            if quirks.prefer_thread_name {
                single_thread_name().or(session_label)
            } else {
                session_label.or_else(single_thread_name)
            }
        })
    }

    pub fn running_state(&self) -> &Entity<RunningState> {
        &self.running_state
    }
}

impl EventEmitter<()> for DebugSession {}

impl Focusable for DebugSession {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.running_state.focus_handle(cx)
    }
}

impl Item for DebugSession {
    type Event = ();
    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Debugger".into()
    }
}

impl Render for DebugSession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.running_state
            .update(cx, |this, cx| this.render(window, cx).into_any_element())
    }
}
