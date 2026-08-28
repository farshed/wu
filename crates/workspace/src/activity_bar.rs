use crate::dock::{Dock, PanelHandle, activate_panel_button, panel_button_context_menu};
use crate::Workspace;
use gpui::{
    Action, Anchor, App, Context, Entity, FocusHandle, Focusable as _, IntoElement, ParentElement,
    Pixels, Render, SharedString, Styled, Subscription, WeakEntity, Window, px,
};
use settings::SettingsStore;
use std::sync::Arc;
use ui::{ButtonSize, IconButton, IconName, IconSize, Tooltip, prelude::*, right_click_menu};
use util::ResultExt as _;

pub const ACTIVITY_BAR_WIDTH: Pixels = px(48.);

/// Entries are shown in this order by `Panel::panel_key()`. Panels not listed here
/// come after, in dock order (left dock first, then right dock).
const PREFERRED_ORDER: [&str; 4] = [
    "ProjectPanel",
    "GitPanel",
    "OutlinePanel",
    "DebugPanel",
];

/// A vertical bar on the left edge of the window with one button per left or right
/// dock panel, like the activity bar in VS Code.
pub struct ActivityBar {
    workspace: WeakEntity<Workspace>,
    left_dock: Entity<Dock>,
    right_dock: Entity<Dock>,
    _subscriptions: Vec<Subscription>,
}

pub enum ActivityBarEntry {
    Panel {
        dock: Entity<Dock>,
        panel_index: usize,
        panel: Arc<dyn PanelHandle>,
        icon: IconName,
        icon_tooltip: &'static str,
    },
}

impl ActivityBarEntry {
    pub fn key(&self) -> &'static str {
        match self {
            ActivityBarEntry::Panel { panel, .. } => panel.panel_key(),
        }
    }

    /// The action the entry's button dispatches, its tooltip, whether the button is
    /// shown as active, and the focus handle to focus before dispatching.
    fn button_state(
        &self,
        window: &Window,
        cx: &App,
    ) -> (Box<dyn Action>, SharedString, bool, FocusHandle) {
        match self {
            ActivityBarEntry::Panel {
                dock,
                panel_index,
                icon_tooltip,
                ..
            } => {
                let dock = dock.read(cx);
                let (action, tooltip, is_active) =
                    dock.panel_button_action(*panel_index, icon_tooltip, window, cx);
                (action, tooltip, is_active, dock.focus_handle(cx))
            }
        }
    }
}

impl ActivityBar {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        left_dock: Entity<Dock>,
        right_dock: Entity<Dock>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&left_dock, |_, _, cx| cx.notify()),
            cx.observe(&right_dock, |_, _, cx| cx.notify()),
            cx.observe_global::<SettingsStore>(|_, cx| cx.notify()),
        ];
        Self {
            workspace,
            left_dock,
            right_dock,
            _subscriptions: subscriptions,
        }
    }

    pub fn entries(&self, window: &Window, cx: &App) -> Vec<ActivityBarEntry> {
        let mut entries = Vec::new();
        for dock in [&self.left_dock, &self.right_dock] {
            for (panel_index, panel) in dock.read(cx).panels().enumerate() {
                let Some(icon) = panel.icon(window, cx) else {
                    continue;
                };
                let Some(icon_tooltip) = panel
                    .icon_tooltip(window, cx)
                    .ok_or_else(|| {
                        anyhow::anyhow!("can't render an activity bar button without a tooltip")
                    })
                    .log_err()
                else {
                    continue;
                };
                entries.push(ActivityBarEntry::Panel {
                    dock: dock.clone(),
                    panel_index,
                    panel: panel.clone(),
                    icon,
                    icon_tooltip,
                });
            }
        }
        entries.sort_by_key(|entry| {
            PREFERRED_ORDER
                .iter()
                .position(|key| *key == entry.key())
                .unwrap_or(PREFERRED_ORDER.len())
        });
        entries
    }

    pub fn entry_keys(&self, window: &Window, cx: &App) -> Vec<&'static str> {
        self.entries(window, cx)
            .iter()
            .map(ActivityBarEntry::key)
            .collect()
    }

    /// Does what clicking the entry's button does. Returns false when there is no
    /// entry with the given key.
    pub fn activate_entry(&self, key: &str, window: &mut Window, cx: &mut App) -> bool {
        let entries = self.entries(window, cx);
        let Some(entry) = entries.iter().find(|entry| entry.key() == key) else {
            return false;
        };
        let (action, _, _, focus_handle) = entry.button_state(window, cx);
        activate_panel_button(&focus_handle, &*action, window, cx);
        true
    }

    fn render_entry(
        &self,
        entry: ActivityBarEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let key = entry.key();
        let (action, tooltip, is_active, focus_handle) = entry.button_state(window, cx);
        let (icon, icon_tooltip) = match &entry {
            ActivityBarEntry::Panel {
                icon, icon_tooltip, ..
            } => (*icon, *icon_tooltip),
        };

        let button = move |is_menu_open: bool| {
            let action = action.boxed_clone();
            let focus_handle = focus_handle.clone();
            let tooltip = tooltip.clone();
            // Include active state in element ID to invalidate the cached
            // tooltip when panel state changes (e.g., via keyboard shortcut)
            IconButton::new((key, is_active as u64), icon)
                .size(ButtonSize::Large)
                .icon_size(IconSize::Custom(rems_from_px(22_f32)))
                .toggle_state(is_active)
                .tab_index(0isize)
                .aria_label(icon_tooltip)
                .on_click({
                    let action = action.boxed_clone();
                    move |_, window, cx| {
                        activate_panel_button(&focus_handle, &*action, window, cx)
                    }
                })
                .when(!is_menu_open, |this| {
                    this.tooltip(move |_window, cx| {
                        Tooltip::for_action(tooltip.clone(), &*action, cx)
                    })
                })
        };

        match entry {
            ActivityBarEntry::Panel { dock, panel, .. } => {
                let workspace = self.workspace.clone();
                right_click_menu(format!("activity-bar-{key}"))
                    .menu(move |window, cx| {
                        panel_button_context_menu(&panel, &dock, &workspace, window, cx)
                    })
                    .anchor(Anchor::TopLeft)
                    .attach(Anchor::TopRight)
                    .trigger(move |is_menu_open, _window, _cx| button(is_menu_open))
                    .into_any_element()
            }
        }
    }
}

impl Render for ActivityBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let bar = v_flex()
            .id("activity-bar")
            .flex_none()
            .w(ACTIVITY_BAR_WIDTH)
            .h_full()
            .items_center()
            .gap_1()
            .py_1()
            .bg(colors.status_bar_background)
            .border_r_1()
            .border_color(colors.border);
        let mut buttons = Vec::new();
        for entry in self.entries(window, cx) {
            buttons.push(self.render_entry(entry, window, cx));
        }
        bar.children(buttons)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dock::{DockPosition, Panel, PanelEvent, PanelSizeState};
    use crate::tests::init_test;
    use gpui::{EventEmitter, Focusable, TestAppContext, actions, div};
    use project::{FakeFs, Project};

    actions!(activity_bar_test, [ToggleFirstPanel, ToggleSecondPanel]);

    const KEYS: [&str; 2] = ["OutlinePanel", "ProjectPanel"];

    struct IconPanel<const INDEX: usize> {
        focus_handle: FocusHandle,
    }

    impl<const INDEX: usize> EventEmitter<PanelEvent> for IconPanel<INDEX> {}

    impl<const INDEX: usize> Focusable for IconPanel<INDEX> {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl<const INDEX: usize> Render for IconPanel<INDEX> {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().track_focus(&self.focus_handle(cx))
        }
    }

    impl<const INDEX: usize> Panel for IconPanel<INDEX> {
        fn persistent_name() -> &'static str {
            KEYS[INDEX]
        }

        fn panel_key() -> &'static str {
            KEYS[INDEX]
        }

        fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
            DockPosition::Left
        }

        fn position_is_valid(&self, position: DockPosition) -> bool {
            position == DockPosition::Left
        }

        fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

        fn default_size(&self, _window: &Window, _cx: &App) -> Pixels {
            px(300.)
        }

        fn initial_size_state(&self, _window: &Window, _cx: &App) -> PanelSizeState {
            PanelSizeState {
                size: None,
                flex: None,
            }
        }

        fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
            Some(IconName::FileTree)
        }

        fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
            Some(KEYS[INDEX])
        }

        fn toggle_action(&self) -> Box<dyn Action> {
            if INDEX == 0 {
                ToggleFirstPanel.boxed_clone()
            } else {
                ToggleSecondPanel.boxed_clone()
            }
        }

        fn activation_priority(&self) -> u32 {
            INDEX as u32
        }
    }

    #[gpui::test]
    async fn test_activity_bar_entries_and_toggle(cx: &mut TestAppContext) {
        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (multi_workspace, cx) = cx.add_window_view(|window, cx| {
            crate::MultiWorkspace::test_new(project.clone(), window, cx)
        });
        let workspace = multi_workspace.read_with(cx, |multi_workspace, _| {
            multi_workspace.workspace().clone()
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let outline_panel = cx.new(|cx| IconPanel::<0> {
                focus_handle: cx.focus_handle(),
            });
            let project_panel = cx.new(|cx| IconPanel::<1> {
                focus_handle: cx.focus_handle(),
            });
            workspace.add_panel(outline_panel, window, cx);
            workspace.add_panel(project_panel, window, cx);
        });
        // Real panels register their toggle actions on the workspace during
        // app init; global handlers stand in for that here.
        let window_handle = cx.update(|window, _| window.window_handle());
        cx.update(|_, cx| {
            let first_workspace = workspace.clone();
            cx.on_action(move |_: &ToggleFirstPanel, cx| {
                let workspace = first_workspace.clone();
                // The action is dispatched inside a window update, so the
                // toggle has to wait until that update finishes.
                cx.defer(move |cx| {
                    window_handle
                        .update(cx, |_, window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.toggle_panel_focus::<IconPanel<0>>(window, cx);
                            })
                        })
                        .ok();
                });
            });
            let second_workspace = workspace.clone();
            cx.on_action(move |_: &ToggleSecondPanel, cx| {
                let workspace = second_workspace.clone();
                // The action is dispatched inside a window update, so the
                // toggle has to wait until that update finishes.
                cx.defer(move |cx| {
                    window_handle
                        .update(cx, |_, window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.toggle_panel_focus::<IconPanel<1>>(window, cx);
                            })
                        })
                        .ok();
                });
            });
        });
        cx.run_until_parked();

        let keys = workspace.update_in(cx, |workspace, window, cx| {
            workspace.activity_bar().read(cx).entry_keys(window, cx)
        });
        assert_eq!(keys, vec!["ProjectPanel", "OutlinePanel"]);

        let is_open =
            workspace.read_with(cx, |workspace, cx| workspace.left_dock().read(cx).is_open());
        assert!(!is_open);

        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .activity_bar()
                .clone()
                .update(cx, |activity_bar, cx| {
                    assert!(activity_bar.activate_entry("OutlinePanel", window, cx));
                });
        });
        cx.run_until_parked();

        let (is_open, active_index) = workspace.read_with(cx, |workspace, cx| {
            let dock = workspace.left_dock().read(cx);
            (dock.is_open(), dock.active_panel_index())
        });
        assert!(is_open, "activating an inactive entry opens its dock");
        assert_eq!(active_index, Some(0));

        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .activity_bar()
                .clone()
                .update(cx, |activity_bar, cx| {
                    assert!(activity_bar.activate_entry("ProjectPanel", window, cx));
                });
        });
        cx.run_until_parked();

        let (is_open, active_index) = workspace.read_with(cx, |workspace, cx| {
            let dock = workspace.left_dock().read(cx);
            (dock.is_open(), dock.active_panel_index())
        });
        assert!(is_open);
        assert_eq!(
            active_index,
            Some(1),
            "activating another entry switches panels"
        );

        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .activity_bar()
                .clone()
                .update(cx, |activity_bar, cx| {
                    assert!(activity_bar.activate_entry("ProjectPanel", window, cx));
                });
        });
        cx.run_until_parked();

        let is_open =
            workspace.read_with(cx, |workspace, cx| workspace.left_dock().read(cx).is_open());
        assert!(!is_open, "activating the active entry closes the dock");
    }
}
