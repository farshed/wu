use super::ChatView;
use crate::{
    ToggleDictation,
    chat_style::{Chip, accent, icon, ink, mix, mono_spinner, text_faint},
};
use dictation::{DictationEvent, DictationSession, ModelStatus};
use futures::StreamExt as _;
use gpui::{
    AnyElement, App, BoxShadow, Context, Hsla, IntoElement, KeyUpEvent, ParentElement,
    SharedString, Styled, Task, Window, div, hsla, linear_color_stop, linear_gradient, point, px,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};
use settings::Settings as _;
use ui::{ContextMenu, IconName, prelude::*, right_click_menu};
use util::ResultExt as _;

const LEVEL_SAMPLES: usize = 48;
const LEVEL_INTERVAL: Duration = Duration::from_millis(60);
const DOWNLOAD_POLL: Duration = Duration::from_millis(250);
const HOLD_TO_TALK: Duration = Duration::from_millis(400);
const DICTATION_BUTTON_SIZE: f32 = 28.;
const STOP_SQUARE_SIZE: f32 = 9.;
const ON_ACCENT: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.985,
    a: 1.,
};
const WAVEFORM_BAR_WIDTH: f32 = 3.;
const WAVEFORM_BAR_GAP: f32 = 3.;
const WAVEFORM_HEIGHT: f32 = 16.;
const VOICE_LIMIT_WARNING_SECS: u64 = 10;
pub(super) const VOICE_TRACK_HEIGHT: f32 = 32.;
pub(super) const VOICE_TRACK_GAP: f32 = 8.;

pub(super) enum DictationState {
    Idle,
    Downloading(f32),
    Recording {
        session: DictationSession,
        levels: VecDeque<f32>,
        started: Instant,
    },
    Transcribing(DictationSession),
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
            DictationState::Downloading(_) | DictationState::Transcribing(_) => {}
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
                    if !crate::AgentChatSettings::get_global(cx).dictation {
                        this.cancel_dictation(cx);
                        return false;
                    }
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
                        this.apply_dictation_event(event, window, cx)
                    })
                    .unwrap_or(true);
                if finished {
                    return;
                }
            }
            this.update(cx, |this, cx| this.end_dictation_stream(cx)).ok();
        });
    }

    fn apply_dictation_event(
        &mut self,
        event: DictationEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let finished = match event {
            DictationEvent::Listening => false,
            DictationEvent::Transcribing => {
                self.dictation = match std::mem::replace(&mut self.dictation, DictationState::Idle)
                {
                    DictationState::Recording { session, .. } => {
                        DictationState::Transcribing(session)
                    }
                    other => other,
                };
                false
            }
            DictationEvent::Transcribed(text) => {
                self.dictation = DictationState::Idle;
                let text = text.trim();
                if !text.is_empty() {
                    self.composer.update(cx, |editor, cx| {
                        editor.insert(&format!("{text} "), window, cx)
                    });
                }
                true
            }
            DictationEvent::Failed(error) => {
                self.dictation = DictationState::Failed(error.to_string().into());
                true
            }
        };
        cx.notify();
        finished
    }

    fn end_dictation_stream(&mut self, cx: &mut Context<Self>) {
        if self.dictation_active() {
            self.dictation = DictationState::Idle;
            cx.notify();
        }
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

    pub(super) fn dictation_active(&self) -> bool {
        matches!(
            self.dictation,
            DictationState::Recording { .. } | DictationState::Transcribing(_)
        )
    }

    pub(super) fn cancel_dictation(&mut self, cx: &mut Context<Self>) {
        match std::mem::replace(&mut self.dictation, DictationState::Idle) {
            DictationState::Recording { session, .. } | DictationState::Transcribing(session) => {
                session.cancel()
            }
            other => {
                self.dictation = other;
                return;
            }
        }
        self._dictation_events = Task::ready(());
        cx.notify();
    }

    pub(super) fn render_dictation_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !crate::AgentChatSettings::get_global(cx).dictation {
            return None;
        }
        let tooltip: SharedString = match &self.dictation {
            DictationState::Recording { .. } => "Stop and transcribe".into(),
            DictationState::Transcribing(_) => "Transcribing…".into(),
            DictationState::Downloading(progress) => {
                format!("Downloading speech model… {:.0}%", progress * 100.).into()
            }
            DictationState::Failed(error) => error.clone(),
            DictationState::Idle => match self.dictation_model_ready() {
                true => "Dictate (hold Cmd-D). Right-click to choose a microphone.".into(),
                false => format!(
                    "Download the on-device speech model ({} MB) to dictate",
                    dictation::model_download_size() / 1_000_000
                )
                .into(),
            },
        };
        let content = div()
            .relative()
            .size(px(DICTATION_BUTTON_SIZE))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center();
        let content = match &self.dictation {
            DictationState::Recording { levels, .. } => {
                let level = levels.back().copied().unwrap_or(0.).clamp(0., 1.);
                let glow = level * level.sqrt();
                let side = STOP_SQUARE_SIZE * (1. + 0.08 * glow);
                glass_accent(content, glow, cx).child(
                    div()
                        .size(px(side))
                        .rounded(px(2.5))
                        .bg(ON_ACCENT),
                )
            }
            DictationState::Transcribing(_) | DictationState::Downloading(_) => {
                glass_accent(content, 0., cx).child(mono_spinner(
                    "agent-dictation-progress".into(),
                    2.,
                    ON_ACCENT,
                ))
            }
            DictationState::Idle | DictationState::Failed(_) => content.child(icon(
                IconName::AgentMicrophone,
                px(18.),
                cx.theme().colors().text_muted,
            )),
        };
        let store = self.store.clone();
        let button = Chip::new("agent-dictation", DICTATION_BUTTON_SIZE / 2., content)
            .hover_background(ink(0.10, cx))
            .tooltip(tooltip)
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_dictation(&ToggleDictation, window, cx)
            }));
        let devices_menu = right_click_menu("agent-dictation-devices")
            .trigger(move |_, _, _| button)
            .menu(move |window, cx| {
                let store = store.clone();
                let devices = dictation::input_devices().log_err().unwrap_or_default();
                let chosen = store.read(cx).list_prefs().dictation_device.clone();
                ContextMenu::build(window, cx, move |menu, _, _| {
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
                })
            })
            .anchor(gpui::Anchor::BottomLeft);
        Some(devices_menu.into_any_element())
    }

    pub(super) fn render_voice_track(
        &self,
        left: f32,
        right: f32,
        top: f32,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let (levels, elapsed, live) = match &self.dictation {
            DictationState::Recording {
                levels, started, ..
            } => (
                levels.iter().copied().collect::<Vec<_>>(),
                Some(started.elapsed().as_secs()),
                true,
            ),
            DictationState::Transcribing(_) => (Vec::new(), None, false),
            _ => return None,
        };
        let colors = cx.theme().colors();
        let faint = text_faint(cx);
        let ink_color = colors.text.opacity(0.8);
        let quiet = faint.opacity(0.5);
        let padding = LEVEL_SAMPLES.saturating_sub(levels.len());
        let bars = std::iter::repeat_n(0., padding)
            .chain(levels)
            .map(move |level: f32| {
                let level = level.clamp(0., 1.);
                let height = (level.sqrt() * WAVEFORM_HEIGHT).max(WAVEFORM_BAR_WIDTH);
                div()
                    .flex_none()
                    .w(px(WAVEFORM_BAR_WIDTH))
                    .h(px(height))
                    .rounded_full()
                    .bg(if height > WAVEFORM_BAR_WIDTH {
                        ink_color
                    } else {
                        quiet
                    })
            });
        let max_seconds = dictation::MAX_RECORDING_DURATION.as_secs();
        let clock = elapsed.map(|seconds| {
            div()
                .flex_none()
                .font_family(super::CODE_FONT)
                .text_size(px(11.))
                .text_color(if seconds + VOICE_LIMIT_WARNING_SECS >= max_seconds {
                    cx.theme().status().warning
                } else if live {
                    colors.text_muted
                } else {
                    faint
                })
                .child(format!("{}:{:02}", seconds / 60, seconds % 60))
        });
        let track = glass_light(
            h_flex()
                .size_full()
                .rounded_full()
                .overflow_hidden()
                .gap(px(10.))
                .pl(px(12.))
                .pr(px(12.)),
            cx,
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .h(px(WAVEFORM_HEIGHT))
                .justify_end()
                .overflow_hidden()
                .gap(px(WAVEFORM_BAR_GAP))
                .children(bars),
        )
        .children(clock);
        Some(
            div()
                .id("agent-dictation-track")
                .absolute()
                .left(px(left))
                .right(px(right))
                .top(px(top))
                .h(px(VOICE_TRACK_HEIGHT))
                .flex()
                .justify_end()
                .occlude()
                .child(track)
                .into_any_element(),
        )
    }

    pub(super) fn render_dictation_status(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (title, detail, failed): (SharedString, SharedString, bool) = match &self.dictation {
            DictationState::Downloading(progress) => (
                "Downloading speech model…".into(),
                format!("{:.0}%", progress * 100.).into(),
                false,
            ),
            DictationState::Failed(error) => ("Dictation stopped".into(), error.clone(), true),
            _ => return None,
        };
        let colors = cx.theme().colors();
        let hover = colors.element_hover;
        Some(
            h_flex()
                .id("agent-dictation-status")
                .items_start()
                .gap(px(8.))
                .px(px(12.))
                .text_size(px(12.))
                .line_height(px(18.))
                .child(
                    div()
                        .flex_none()
                        .mt(px(6.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(if failed {
                            cx.theme().status().warning
                        } else {
                            text_faint(cx)
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .gap_x(px(8.))
                        .child(div().text_color(colors.text).child(title))
                        .child(div().min_w_0().text_color(colors.text_muted).child(detail)),
                )
                .when(failed, |this| {
                    this.child(
                        div()
                            .id("agent-dictation-dismiss")
                            .flex_none()
                            .mt(px(-3.))
                            .h(px(24.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .text_color(colors.text_muted)
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover).text_color(colors.text))
                            .child("Dismiss")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dictation = DictationState::Idle;
                                cx.notify();
                            })),
                    )
                })
                .into_any_element(),
        )
    }
}

fn shadow(color: Hsla, y: f32, blur: f32, spread: f32, inset: bool) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset,
    }
}

fn vertical_gradient(top: Hsla, bottom: Hsla) -> gpui::Background {
    linear_gradient(
        180.,
        linear_color_stop(top, 0.),
        linear_color_stop(bottom, 1.),
    )
}

fn is_dark(cx: &App) -> bool {
    !cx.theme().appearance.is_light()
}

fn glass_light(element: gpui::Div, cx: &App) -> gpui::Div {
    let (top, bottom, rim, highlight, drop) = if is_dark(cx) {
        (
            hsla(0., 0., 1., 0.09),
            hsla(0., 0., 1., 0.04),
            hsla(0., 0., 1., 0.10),
            hsla(0., 0., 1., 0.10),
            hsla(0., 0., 0., 0.35),
        )
    } else {
        (
            hsla(0., 0., 0.985, 1.),
            hsla(0., 0., 0.925, 1.),
            hsla(0., 0., 0., 0.10),
            hsla(0., 0., 1., 0.95),
            hsla(0., 0., 0., 0.08),
        )
    };
    element
        .bg(vertical_gradient(top, bottom))
        .border_1()
        .border_color(rim)
        .shadow(vec![
            shadow(highlight, 1., 0., 0., true),
            shadow(drop, 1., 3., 0., false),
        ])
}

fn glass_accent(element: gpui::Div, glow: f32, cx: &App) -> gpui::Div {
    let dark = is_dark(cx);
    let base = accent(cx);
    let lift = |amount: f32| mix(base, hsla(0., 0., 1., base.a), amount);
    let rim = lift(0.45).opacity(if dark { 0.45 } else { 0.7 });
    let highlight = hsla(0., 0., 1., if dark { 0.22 } else { 0.38 });
    let halo = base.opacity(0.22 + 0.4 * glow);
    element
        .bg(vertical_gradient(lift(if dark { 0.18 } else { 0.32 }), base))
        .border_1()
        .border_color(rim)
        .shadow(vec![
            shadow(highlight, 1., 0., 0., true),
            shadow(hsla(0., 0., 1., 0.12), 0., 0., 1., true),
            shadow(halo, 2. + 2. * glow, 6. + 14. * glow, 0., false),
        ])
}
