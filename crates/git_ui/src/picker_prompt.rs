use futures::channel::oneshot;
use fuzzy::{StringMatch, StringMatchCandidate};

use core::cmp;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Subscription, Task, WeakEntity, Window, rems,
};
use picker::{Picker, PickerDelegate};
use std::sync::Arc;
use ui::{HighlightedLabel, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt;
use workspace::{ModalView, Workspace};

pub struct PickerPrompt<D: PickerDelegate> {
    pub picker: Entity<Picker<D>>,
    _subscription: Subscription,
}

pub fn prompt(
    prompt: &str,
    options: Vec<SharedString>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Option<usize>> {
    if options.is_empty() {
        return Task::ready(None);
    } else if options.len() == 1 {
        return Task::ready(Some(0));
    }
    let prompt = prompt.to_string().into();

    window.spawn(cx, async move |cx| {
        // Modal branch picker has a longer trailoff than a popover one.
        let (tx, rx) = oneshot::channel();
        let delegate = PickerPromptDelegate::new(prompt, options, tx, 70);

        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    PickerPrompt::new(delegate, 34., window, cx)
                })
            })
            .ok();

        (rx.await).ok()
    })
}

pub fn prompt_branch(
    prompt: &str,
    options: Vec<SharedString>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Option<SharedString>> {
    let prompt = prompt.to_string().into();

    window.spawn(cx, async move |cx| {
        let (tx, rx) = oneshot::channel();
        let delegate = BranchPickerPromptDelegate::new(prompt, options, tx, 70);

        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    PickerPrompt::new(delegate, 34., window, cx)
                })
            })
            .ok();

        (rx.await).ok()
    })
}

impl<D: PickerDelegate> PickerPrompt<D> {
    fn new(
        delegate: D,
        rem_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker =
            cx.new(|cx| Picker::uniform_list(delegate, window, cx).initial_width(rems(rem_width)));
        let _subscription = cx.subscribe(&picker, |_, _, _, cx| cx.emit(DismissEvent));
        Self {
            picker,
            _subscription,
        }
    }
}
impl<D: PickerDelegate> ModalView for PickerPrompt<D> {}
impl<D: PickerDelegate> EventEmitter<DismissEvent> for PickerPrompt<D> {}

impl<D: PickerDelegate> Focusable for PickerPrompt<D> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl<D: PickerDelegate> Render for PickerPrompt<D> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .child(self.picker.clone())
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.picker.update(cx, |this, cx| {
                    this.cancel(&Default::default(), window, cx);
                })
            }))
    }
}

pub struct PickerPromptDelegate {
    prompt: Arc<str>,
    matches: Vec<StringMatch>,
    all_options: Vec<SharedString>,
    selected_index: usize,
    max_match_length: usize,
    tx: Option<oneshot::Sender<usize>>,
}

impl PickerPromptDelegate {
    pub fn new(
        prompt: Arc<str>,
        options: Vec<SharedString>,
        tx: oneshot::Sender<usize>,
        max_chars: usize,
    ) -> Self {
        Self {
            prompt,
            all_options: options,
            matches: vec![],
            selected_index: 0,
            max_match_length: max_chars,
            tx: Some(tx),
        }
    }
}

impl PickerDelegate for PickerPromptDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "picker prompt"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        self.prompt.clone()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |picker, cx| {
            let candidates = picker.read_with(cx, |picker, _| {
                picker
                    .delegate
                    .all_options
                    .iter()
                    .enumerate()
                    .map(|(ix, option)| StringMatchCandidate::new(ix, option))
                    .collect::<Vec<StringMatchCandidate>>()
            });
            let Some(candidates) = candidates.log_err() else {
                return;
            };
            let matches: Vec<StringMatch> = if query.is_empty() {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| StringMatch {
                        candidate_id: index,
                        string: candidate.string,
                        positions: Vec::new(),
                        score: 0.0,
                    })
                    .collect()
            } else {
                fuzzy::match_strings(
                    &candidates,
                    &query,
                    true,
                    true,
                    10000,
                    &Default::default(),
                    cx.background_executor().clone(),
                )
                .await
            };
            picker
                .update(cx, |picker, _| {
                    let delegate = &mut picker.delegate;
                    delegate.matches = matches;
                    if delegate.matches.is_empty() {
                        delegate.selected_index = 0;
                    } else {
                        delegate.selected_index =
                            cmp::min(delegate.selected_index, delegate.matches.len() - 1);
                    }
                })
                .log_err();
        })
    }

    fn confirm(&mut self, _: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(option) = self.matches.get(self.selected_index()) else {
            return;
        };

        self.tx.take().map(|tx| tx.send(option.candidate_id));
        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let hit = &self.matches.get(ix)?;
        let shortened_option = util::truncate_and_trailoff(&hit.string, self.max_match_length);

        Some(
            ListItem::new(format!("picker-prompt-menu-{ix}"))
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .map(|el| {
                    let highlights: Vec<_> = hit
                        .positions
                        .iter()
                        .filter(|&&index| index < self.max_match_length)
                        .copied()
                        .collect();

                    el.child(HighlightedLabel::new(shortened_option, highlights))
                }),
        )
    }
}

pub struct BranchPickerPromptDelegate {
    prompt: Arc<str>,
    matches: Vec<BranchPromptMatch>,
    all_options: Vec<SharedString>,
    selected_index: usize,
    max_match_length: usize,
    tx: Option<oneshot::Sender<SharedString>>,
}

enum BranchPromptMatch {
    Existing(StringMatch),
    Custom(String),
}

impl BranchPickerPromptDelegate {
    pub fn new(
        prompt: Arc<str>,
        options: Vec<SharedString>,
        tx: oneshot::Sender<SharedString>,
        max_chars: usize,
    ) -> Self {
        Self {
            prompt,
            all_options: options,
            matches: vec![],
            selected_index: 0,
            max_match_length: max_chars,
            tx: Some(tx),
        }
    }
}

impl PickerDelegate for BranchPickerPromptDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "branch picker prompt"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        self.prompt.clone()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |picker, cx| {
            let candidates = picker.read_with(cx, |picker, _| {
                picker
                    .delegate
                    .all_options
                    .iter()
                    .enumerate()
                    .map(|(ix, option)| StringMatchCandidate::new(ix, option))
                    .collect::<Vec<StringMatchCandidate>>()
            });
            let Some(candidates) = candidates.log_err() else {
                return;
            };
            let mut matches: Vec<BranchPromptMatch> = if query.is_empty() {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| {
                        BranchPromptMatch::Existing(StringMatch {
                            candidate_id: index,
                            string: candidate.string,
                            positions: Vec::new(),
                            score: 0.0,
                        })
                    })
                    .collect()
            } else {
                fuzzy::match_strings(
                    &candidates,
                    &query,
                    true,
                    true,
                    10000,
                    &Default::default(),
                    cx.background_executor().clone(),
                )
                .await
                .into_iter()
                .map(BranchPromptMatch::Existing)
                .collect()
            };

            let trimmed_query = query.trim();
            if !trimmed_query.is_empty() {
                let exact_match = picker
                    .read_with(cx, |picker, _| {
                        picker
                            .delegate
                            .all_options
                            .iter()
                            .any(|opt| opt.as_ref() == trimmed_query)
                    })
                    .unwrap_or(false);

                if !exact_match {
                    matches.push(BranchPromptMatch::Custom(trimmed_query.to_string()));
                }
            }

            picker
                .update(cx, |picker, _| {
                    let delegate = &mut picker.delegate;
                    delegate.matches = matches;
                    if delegate.matches.is_empty() {
                        delegate.selected_index = 0;
                    } else {
                        delegate.selected_index =
                            cmp::min(delegate.selected_index, delegate.matches.len() - 1);
                    }
                })
                .log_err();
        })
    }

    fn confirm(&mut self, _: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(option) = self.matches.get(self.selected_index()) else {
            return;
        };

        let result = match option {
            BranchPromptMatch::Existing(hit) => self.all_options[hit.candidate_id].clone(),
            BranchPromptMatch::Custom(query) => SharedString::from(query.clone()),
        };

        if let Some(tx) = self.tx.take() {
            let _ = tx.send(result);
        }
        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let hit = self.matches.get(ix)?;
        match hit {
            BranchPromptMatch::Existing(hit) => {
                let shortened_option = util::truncate_and_trailoff(&hit.string, self.max_match_length);
                Some(
                    ListItem::new(format!("branch-prompt-item-{ix}"))
                        .inset(true)
                        .spacing(ListItemSpacing::Sparse)
                        .toggle_state(selected)
                        .map(|el| {
                            let highlights: Vec<_> = hit
                                .positions
                                .iter()
                                .filter(|&&index| index < self.max_match_length)
                                .copied()
                                .collect();

                            el.child(HighlightedLabel::new(shortened_option, highlights))
                        }),
                )
            }
            BranchPromptMatch::Custom(custom) => Some(
                ListItem::new(format!("branch-prompt-custom-{ix}"))
                    .inset(true)
                    .spacing(ListItemSpacing::Sparse)
                    .toggle_state(selected)
                    .child(Label::new(format!("Pull '{custom}'"))),
            ),
        }
    }
}

