use super::{ChatView, ui};
use crate::{ToggleDictation, chat_style::Chip};
use dictation::{DictationEvent, DictationSession, ModelStatus};
use futures::StreamExt as _;
use gpui::{
    AnyElement, Context, IntoElement, KeyUpEvent, ParentElement, SharedString, Styled, Window,
    div, px,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};
use settings::Settings as _;
use ui::{ContextMenu, Icon, IconName, IconSize, PopoverMenu, prelude::*};
use util::ResultExt as _;

const LEVEL_SAMPLES: usize = 48;
const LEVEL_INTERVAL: Duration = Duration::from_millis(60);
const DOWNLOAD_POLL: Duration = Duration::from_millis(250);
const HOLD_TO_TALK: Duration = Duration::from_millis(400);

pub(super) enum DictationState {
    Idle,
    Downloading(f32),
    Recording {
        session: DictationSession,
        levels: VecDeque<f32>,
        started: Instant,
    },
    Transcribing,
    Failed(SharedString),
}

pub(super) fn data_directory() -> PathBuf {
    paths::data_dir().join("dictation")
}

impl ChatView {
    pub(super) fn toggle_dictation(
        &mut self,
        _: &ToggleDictation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &self.dictation {
            DictationState::Recording { session, .. } => session.stop_and_transcribe(),
            DictationState::Downloading(_) | DictationState::Transcribing => {}
            DictationState::Idle | DictationState::Failed(_) => self.start_dictation(window, cx),
        }
        cx.notify();
    }

    pub(super) fn dictation_key_up(&mut self, event: &KeyUpEvent, cx: &mut Context<Self>) {
        if event.keystroke.key != "d" {
            return;
        }
        if let DictationState::Recording { session, started, .. } = &self.dictation
            && started.elapsed() >= HOLD_TO_TALK
        {
            session.stop_and_transcribe();
            cx.notify();
        }
    }

    fn start_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = data_directory();
        match dictation::model_status(&directory) {
            ModelStatus::Ready => {}
            ModelStatus::Downloading(progress) => {
                self.dictation = DictationState::Downloading(progress.fraction());
                return;
            }
            ModelStatus::Missing => {
                self.download_dictation_model(cx);
                return;
            }
        }
        let device = self.store.read(cx).list_prefs().dictation_device.clone();
        let (session, mut events) = match DictationSession::start(&directory, device) {
            Ok(started) => started,
            Err(error) => {
                self.dictation = DictationState::Failed(error.to_string().into());
                return;
            }
        };
        self.dictation = DictationState::Recording {
            session,
            levels: VecDeque::with_capacity(LEVEL_SAMPLES),
            started: Instant::now(),
        };
        self._dictation_levels = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(LEVEL_INTERVAL).await;
                let recording = this.update(cx, |this, cx| {
                    let DictationState::Recording { session, levels, .. } = &mut this.dictation
                    else {
                        return false;
                    };
                    if levels.len() == LEVEL_SAMPLES {
                        levels.pop_front();
                    }
                    levels.push_back(session.take_peak_input_level());
                    cx.notify();
                    true
                });
                if !recording.unwrap_or(false) {
                    return;
                }
            }
        });
        self._dictation_events = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                let finished = this
                    .update_in(cx, |this, window, cx| {
                        let finished = match event {
                            DictationEvent::Listening => false,
                            DictationEvent::Transcribing => {
                                this.dictation = DictationState::Transcribing;
                                false
                            }
                            DictationEvent::Transcribed(text) => {
                                this.dictation = DictationState::Idle;
                                let text = text.trim();
                                if !text.is_empty() {
                                    this.composer.update(cx, |editor, cx| {
                                        editor.insert(&format!("{text} "), window, cx)
                                    });
                                }
                                true
                            }
                            DictationEvent::Failed(error) => {
                                this.dictation = DictationState::Failed(error.to_string().into());
                                true
                            }
                        };
                        cx.notify();
                        finished
                    })
                    .unwrap_or(true);
                if finished {
                    return;
                }
            }
        });
    }

    fn download_dictation_model(&mut self, cx: &mut Context<Self>) {
        let directory = data_directory();
        let http = cx.http_client();
        self.dictation = DictationState::Downloading(0.);
        let download = cx.background_spawn({
            let directory = directory.clone();
            async move { dictation::download_model(http, &directory).await }
        });
        self._dictation_download = cx.spawn(async move |this, cx| {
            let poll = {
                let this = this.clone();
                let directory = directory.clone();
                let mut cx = cx.clone();
                async move {
                    loop {
                        cx.background_executor().timer(DOWNLOAD_POLL).await;
                        if let ModelStatus::Downloading(progress) = dictation::model_status(&directory)
                        {
                            let updated = this.update(&mut cx, |this, cx| {
                                this.dictation = DictationState::Downloading(progress.fraction());
                                cx.notify();
                            });
                            if updated.is_err() {
                                return;
                            }
                        }
                    }
                }
            };
            let result = futures::future::select(Box::pin(download), Box::pin(poll)).await;
            let result = match result {
                futures::future::Either::Left((result, _)) => result,
                futures::future::Either::Right(_) => return,
            };
            this.update(cx, |this, cx| {
                this.dictation_model_ready.set(None);
                this.dictation = match result {
                    Ok(()) => DictationState::Idle,
                    Err(error) => DictationState::Failed(
                        format!("Couldn't download the speech model: {error}").into(),
                    ),
                };
                cx.notify();
            })
            .log_err();
        });
    }

    fn dictation_model_ready(&self) -> bool {
        if let Some(ready) = self.dictation_model_ready.get() {
            return ready;
        }
        let ready = dictation::model_status(&data_directory()) == ModelStatus::Ready;
        self.dictation_model_ready.set(Some(ready));
        ready
    }

    pub(super) fn render_dictation_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !crate::AgentChatSettings::get_global(cx).dictation {
            return None;
        }
        let recording = matches!(self.dictation, DictationState::Recording { .. });
        let busy = matches!(
            self.dictation,
            DictationState::Transcribing | DictationState::Downloading(_)
        );
        let tooltip: SharedString = match &self.dictation {
            DictationState::Recording { .. } => "Stop and transcribe".into(),
            DictationState::Transcribing => "Transcribing…".into(),
            DictationState::Downloading(progress) => {
                format!("Downloading speech model… {:.0}%", progress * 100.).into()
            }
            DictationState::Failed(error) => error.clone(),
            DictationState::Idle => match self.dictation_model_ready() {
                true => "Dictate (hold Cmd-D)".into(),
                false => format!(
                    "Download the on-device speech model ({} MB) to dictate",
                    dictation::model_download_size() / 1_000_000
                )
                .into(),
            },
        };
        let store = self.store.clone();
        let devices_menu = PopoverMenu::new("agent-dictation-devices")
            .trigger(
                Chip::new(
                    "agent-dictation",
                    8.,
                    div()
                        .size(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(IconName::AgentMicrophone)
                                .size(IconSize::Small)
                                .color(if recording {
                                    Color::Error
                                } else if busy {
                                    Color::Accent
                                } else {
                                    Color::Muted
                                }),
                        ),
                )
                .toggle_state(recording)
                .tooltip(tooltip)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_dictation(&ToggleDictation, window, cx)
                })),
            )
            .menu(move |window, cx| {
                let store = store.clone();
                let devices = dictation::input_devices().log_err().unwrap_or_default();
                let chosen = store.read(cx).list_prefs().dictation_device.clone();
                Some(ContextMenu::build(window, cx, move |menu, _, _| {
                    let mut menu = menu.header("Microphone").toggleable_entry(
                        "System Default",
                        chosen.is_none(),
                        ui::IconPosition::Start,
                        None,
                        {
                            let store = store.clone();
                            move |_, cx| {
                                store.update(cx, |store, cx| {
                                    store.update_list_prefs(|prefs| prefs.dictation_device = None, cx)
                                })
                            }
                        },
                    );
                    for device in &devices {
                        let store = store.clone();
                        let id = device.id.clone();
                        menu = menu.toggleable_entry(
                            device.name.clone(),
                            chosen.as_deref() == Some(device.id.as_str()),
                            ui::IconPosition::Start,
                            None,
                            move |_, cx| {
                                let id = id.clone();
                                store.update(cx, |store, cx| {
                                    store.update_list_prefs(
                                        |prefs| prefs.dictation_device = Some(id),
                                        cx,
                                    )
                                })
                            },
                        );
                    }
                    menu
                }))
            })
            .anchor(gpui::Anchor::BottomLeft);
        Some(devices_menu.into_any_element())
    }

    pub(super) fn render_dictation_strip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let colors = cx.theme().colors();
        let (label, levels, elapsed): (SharedString, Vec<f32>, Option<u64>) = match &self.dictation {
            DictationState::Recording {
                levels, started, ..
            } => (
                "Listening…".into(),
                levels.iter().copied().collect(),
                Some(started.elapsed().as_secs()),
            ),
            DictationState::Transcribing => ("Transcribing…".into(), Vec::new(), None),
            DictationState::Downloading(progress) => (
                format!("Downloading speech model… {:.0}%", progress * 100.).into(),
                Vec::new(),
                None,
            ),
            DictationState::Failed(error) => (error.clone(), Vec::new(), None),
            DictationState::Idle => return None,
        };
        Some(
            h_flex()
                .px(px(16.))
                .pt(px(10.))
                .gap(px(8.))
                .text_size(ui(12.))
                .text_color(if matches!(self.dictation, DictationState::Failed(_)) {
                    cx.theme().status().error
                } else {
                    colors.text_muted
                })
                .child(label)
                .child(
                    h_flex()
                        .flex_1()
                        .h(px(18.))
                        .gap(px(2.))
                        .items_center()
                        .children(levels.into_iter().map(|level| {
                            let height = (level.clamp(0., 1.).sqrt() * 18.).max(2.);
                            div()
                                .w(px(2.))
                                .h(px(height))
                                .rounded_full()
                                .bg(colors.text_muted)
                        })),
                )
                .when_some(elapsed, |this, elapsed| {
                    this.child(div().child(format!("{elapsed}s / 60s")))
                })
                .into_any_element(),
        )
    }
}
