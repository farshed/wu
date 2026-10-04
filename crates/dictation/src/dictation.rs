mod capture;
mod model;
mod resample;

use anyhow::{Context as _, Result, anyhow, bail};
use capture::{Capture, CapturedAudio, Recording};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use model::Recognizer;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
    },
    time::{Duration, Instant},
};

pub use capture::{InputDevice, default_input_device_id, input_devices};
pub use model::{
    DownloadProgress, ModelStatus, download_model, model_directory, model_download_size,
    model_status,
};

pub const MAX_RECORDING_DURATION: Duration = Duration::from_secs(60);

const UNLOAD_IDLE_MODEL_AFTER: Duration = Duration::from_secs(30);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) fn max_recording_samples(sample_rate: u32) -> usize {
    sample_rate as usize * MAX_RECORDING_DURATION.as_secs() as usize
}

#[derive(Debug)]
pub enum DictationEvent {
    Listening,
    Transcribing,
    /// Empty when no speech was heard.
    Transcribed(String),
    Failed(anyhow::Error),
}

/// Dropping the session cancels it.
pub struct DictationSession {
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    input_level: Arc<AtomicU32>,
}

impl DictationSession {
    /// `input_device_id` is an [`InputDevice::id`]; `None` or a disconnected device uses the system default.
    pub fn start(
        data_directory: &Path,
        input_device_id: Option<String>,
    ) -> Result<(Self, UnboundedReceiver<DictationEvent>)> {
        let admission = Admission::acquire()?;
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let input_level = Arc::new(AtomicU32::new(0));
        let (events_sender, events) = unbounded();
        submit_to_worker(WorkerJob {
            job: Job {
                model_directory: model_directory(data_directory),
                stop: stop.clone(),
                cancel: cancel.clone(),
                input_level: input_level.clone(),
                input_device_id,
                events: events_sender,
            },
            _admission: admission,
        })?;
        Ok((
            Self {
                stop,
                cancel,
                input_level,
            },
            events,
        ))
    }

    pub fn take_peak_input_level(&self) -> f32 {
        f32::from_bits(self.input_level.swap(0, Ordering::Relaxed))
    }

    pub fn stop_and_transcribe(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn cancel(self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Drop for DictationSession {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

struct Job {
    model_directory: PathBuf,
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    input_level: Arc<AtomicU32>,
    input_device_id: Option<String>,
    events: UnboundedSender<DictationEvent>,
}

impl Job {
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    fn emit(&self, event: DictationEvent) {
        self.events.unbounded_send(event).ok();
    }
}

fn record<C: Recording>(
    job: &Job,
    start: impl FnOnce() -> Result<C>,
) -> Result<Option<CapturedAudio>> {
    if job.is_cancelled() || job.is_stopped() {
        return Ok(None);
    }
    let capture = start()?;
    if job.is_cancelled() {
        return Ok(None);
    }
    if !job.is_stopped() {
        job.emit(DictationEvent::Listening);
    }
    let started = Instant::now();
    while !job.is_stopped()
        && !job.is_cancelled()
        && !capture.ended()
        && started.elapsed() < MAX_RECORDING_DURATION
    {
        std::thread::sleep(STOP_POLL_INTERVAL);
    }
    if job.is_cancelled() {
        return Ok(None);
    }
    let audio = capture.finish()?;
    job.emit(DictationEvent::Transcribing);
    Ok(Some(audio))
}

fn run_job<C, T>(
    job: &Job,
    start_capture: impl FnOnce() -> Result<C> + Send,
    load_model: impl FnOnce() -> Result<T>,
) -> Result<Option<String>>
where
    C: Recording,
    T: FnOnce(CapturedAudio) -> Result<String>,
{
    if job.is_cancelled() {
        return Ok(None);
    }
    std::thread::scope(|scope| {
        let capture = std::thread::Builder::new()
            .name("dictation-capture".into())
            .spawn_scoped(scope, || record(job, start_capture))?;
        // Closes the microphone before the scope joins it, even if model loading unwinds.
        let _cancel_capture_on_unwind = CancelOnDrop(&job.cancel);
        let transcribe = load_model();
        if transcribe.is_err() {
            job.cancel.store(true, Ordering::Release);
        }
        let audio = capture
            .join()
            .map_err(|_| anyhow!("The microphone thread failed"))?;
        let transcribe = transcribe?;
        if job.is_cancelled() {
            return Ok(None);
        }
        let Some(audio) = audio? else {
            return Ok(Some(String::new()));
        };
        let text = transcribe(audio)?;
        Ok((!job.is_cancelled()).then_some(text))
    })
}

struct CancelOnDrop<'a>(&'a AtomicBool);

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

static DICTATION_ACTIVE: AtomicBool = AtomicBool::new(false);

struct Admission;

impl Admission {
    fn acquire() -> Result<Self> {
        if DICTATION_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            bail!("Dictation is still finishing. Wait a moment, then try again.");
        }
        Ok(Self)
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        DICTATION_ACTIVE.store(false, Ordering::Release);
    }
}

struct WorkerJob {
    job: Job,
    _admission: Admission,
}

static WORKER: Mutex<Option<SyncSender<WorkerJob>>> = Mutex::new(None);

fn submit_to_worker(worker_job: WorkerJob) -> Result<()> {
    let mut worker = WORKER.lock().unwrap_or_else(PoisonError::into_inner);
    let sender = match worker.as_ref() {
        Some(sender) => sender.clone(),
        None => {
            let (sender, receiver) = sync_channel(1);
            std::thread::Builder::new()
                .name("dictation-worker".into())
                .spawn(move || run_worker(receiver))
                .context("Could not start the dictation worker")?;
            worker.insert(sender).clone()
        }
    };
    match sender.try_send(worker_job) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => bail!("Dictation is still finishing"),
        Err(TrySendError::Disconnected(_)) => {
            worker.take();
            bail!("The dictation worker stopped. Try again.")
        }
    }
}

fn run_worker(jobs: Receiver<WorkerJob>) {
    let mut cached_model: Option<(PathBuf, Recognizer)> = None;
    loop {
        let WorkerJob { job, _admission } = match jobs.recv_timeout(UNLOAD_IDLE_MODEL_AFTER) {
            Ok(worker_job) => worker_job,
            Err(RecvTimeoutError::Timeout) => {
                cached_model = None;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        // A native panic must not kill the worker for every later session.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_job(
                &job,
                || Capture::start(job.input_device_id.as_deref(), job.input_level.clone()),
                || {
                    let is_cached = cached_model
                        .as_ref()
                        .is_some_and(|(directory, _)| *directory == job.model_directory);
                    if !is_cached {
                        cached_model = None;
                        cached_model = Some((
                            job.model_directory.clone(),
                            Recognizer::load(&job.model_directory)?,
                        ));
                    }
                    let (_, recognizer) = cached_model
                        .as_mut()
                        .context("The speech model is not loaded")?;
                    Ok(move |audio: CapturedAudio| {
                        recognizer.transcribe(audio.samples, audio.sample_rate)
                    })
                },
            )
        }))
        .unwrap_or_else(|_| {
            cached_model = None;
            Err(anyhow!("Dictation stopped unexpectedly"))
        });
        match result {
            Ok(Some(text)) => job.emit(DictationEvent::Transcribed(text)),
            Ok(None) => {}
            Err(error) => job.emit(DictationEvent::Failed(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::JoinHandle;

    const WAIT: Duration = Duration::from_secs(5);
    const RETAINED_SAMPLES: [f32; 3] = [0.25, 0.5, 0.25];

    struct FakeCapture {
        closed: SyncSender<()>,
        ended: Arc<AtomicBool>,
    }

    impl Recording for FakeCapture {
        fn ended(&self) -> bool {
            self.ended.load(Ordering::Acquire)
        }

        fn finish(self) -> Result<CapturedAudio> {
            Ok(CapturedAudio {
                samples: RETAINED_SAMPLES.to_vec(),
                sample_rate: 16_000,
            })
        }
    }

    impl Drop for FakeCapture {
        fn drop(&mut self) {
            self.closed.try_send(()).unwrap();
        }
    }

    type DeviceOpenGate = (Receiver<()>, SyncSender<()>);

    struct Fixture {
        session: DictationSession,
        events: UnboundedReceiver<DictationEvent>,
        finish_loading: SyncSender<Result<()>>,
        microphone_closed: Receiver<()>,
        capture_ended: Arc<AtomicBool>,
        result: Receiver<Result<Option<String>>>,
        worker: JoinHandle<()>,
    }

    impl Fixture {
        fn new(device_open_gate: Option<DeviceOpenGate>) -> Self {
            Self::with_admission(device_open_gate, None)
        }

        fn with_admission(
            device_open_gate: Option<DeviceOpenGate>,
            admission: Option<Admission>,
        ) -> Self {
            let (events_sender, events) = unbounded();
            let stop = Arc::new(AtomicBool::new(false));
            let cancel = Arc::new(AtomicBool::new(false));
            let input_level = Arc::new(AtomicU32::new(0));
            let job = Job {
                model_directory: PathBuf::new(),
                stop: stop.clone(),
                cancel: cancel.clone(),
                input_level: input_level.clone(),
                input_device_id: None,
                events: events_sender,
            };
            let (finish_loading, loading_finished) = sync_channel::<Result<()>>(1);
            let (loading_started_sender, loading_started) = sync_channel(1);
            let (microphone_closed_sender, microphone_closed) = sync_channel(1);
            let (result_sender, result) = sync_channel(1);
            let capture_ended = Arc::new(AtomicBool::new(false));
            let worker = std::thread::spawn({
                let capture_ended = capture_ended.clone();
                move || {
                    let _admission = admission;
                    let outcome = run_job(
                        &job,
                        || {
                            if let Some((device_opened, device_opening)) = device_open_gate {
                                device_opening.send(()).unwrap();
                                device_opened.recv_timeout(WAIT).unwrap();
                            }
                            Ok(FakeCapture {
                                closed: microphone_closed_sender,
                                ended: capture_ended,
                            })
                        },
                        || {
                            loading_started_sender.send(()).unwrap();
                            loading_finished.recv_timeout(WAIT).unwrap()?;
                            Ok(|audio: CapturedAudio| {
                                assert_eq!(audio.samples, RETAINED_SAMPLES);
                                assert_eq!(audio.sample_rate, 16_000);
                                Ok("retained speech".into())
                            })
                        },
                    );
                    result_sender.send(outcome).unwrap();
                }
            });
            loading_started.recv_timeout(WAIT).unwrap();
            Self {
                session: DictationSession {
                    stop,
                    cancel,
                    input_level,
                },
                events,
                finish_loading,
                microphone_closed,
                capture_ended,
                result,
                worker,
            }
        }

        fn next_event(&mut self) -> DictationEvent {
            let deadline = Instant::now() + WAIT;
            loop {
                match self.events.try_recv() {
                    Ok(event) => return event,
                    Err(error) if error.is_closed() || Instant::now() > deadline => {
                        panic!("no dictation event arrived: {error}")
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(1)),
                }
            }
        }

        fn has_pending_event(&mut self) -> bool {
            self.events.try_recv().is_ok()
        }

        fn expect_listening(&mut self) {
            assert!(matches!(self.next_event(), DictationEvent::Listening));
        }

        fn expect_transcribing(&mut self) {
            assert!(matches!(self.next_event(), DictationEvent::Transcribing));
        }

        fn finish_loading(self, result: Result<()>) -> Result<Option<String>> {
            self.finish_loading.send(result).unwrap();
            let result = self.result.recv_timeout(WAIT).unwrap();
            self.worker.join().unwrap();
            result
        }
    }

    #[test]
    fn listening_and_stop_do_not_wait_for_the_model_and_audio_is_retained() {
        let mut fixture = Fixture::new(None);
        fixture.expect_listening();
        assert!(fixture.result.try_recv().is_err());
        fixture.session.stop_and_transcribe();
        fixture.session.stop_and_transcribe();
        fixture.microphone_closed.recv_timeout(WAIT).unwrap();
        fixture.expect_transcribing();
        assert!(fixture.result.try_recv().is_err());
        assert_eq!(
            fixture.finish_loading(Ok(())).unwrap().as_deref(),
            Some("retained speech")
        );
    }

    #[test]
    fn cancel_during_load_closes_the_microphone_without_waiting_for_the_model() {
        let mut fixture = Fixture::new(None);
        fixture.expect_listening();
        let Fixture {
            session,
            finish_loading,
            microphone_closed,
            result,
            worker,
            ..
        } = fixture;
        session.cancel();
        microphone_closed.recv_timeout(WAIT).unwrap();
        assert!(result.try_recv().is_err());
        finish_loading.send(Ok(())).unwrap();
        assert_eq!(result.recv_timeout(WAIT).unwrap().unwrap(), None);
        worker.join().unwrap();
    }

    #[test]
    fn cancel_after_stop_during_load_discards_retained_audio() {
        let mut fixture = Fixture::new(None);
        fixture.expect_listening();
        fixture.session.stop_and_transcribe();
        fixture.microphone_closed.recv_timeout(WAIT).unwrap();
        fixture.session.cancel.store(true, Ordering::Release);
        assert_eq!(fixture.finish_loading(Ok(())).unwrap(), None);
    }

    #[test]
    fn load_failure_closes_the_live_microphone_before_returning_the_error() {
        let mut fixture = Fixture::new(None);
        fixture.expect_listening();
        fixture
            .finish_loading
            .send(Err(anyhow!("model removed or corrupt")))
            .unwrap();
        assert!(fixture.result.recv_timeout(WAIT).unwrap().is_err());
        fixture.microphone_closed.try_recv().unwrap();
        fixture.worker.join().unwrap();
    }

    #[test]
    fn listening_waits_for_the_device_to_open_and_stop_during_open_is_safe() {
        let (device_opened_sender, device_opened) = sync_channel(1);
        let (device_opening, device_opening_started) = sync_channel(1);
        let mut fixture = Fixture::new(Some((device_opened, device_opening)));
        device_opening_started.recv_timeout(WAIT).unwrap();
        assert!(!fixture.has_pending_event());
        fixture.session.stop_and_transcribe();
        device_opened_sender.send(()).unwrap();
        fixture.microphone_closed.recv_timeout(WAIT).unwrap();
        fixture.expect_transcribing();
        assert!(!fixture.has_pending_event());
        assert_eq!(
            fixture.finish_loading(Ok(())).unwrap().as_deref(),
            Some("retained speech")
        );
    }

    #[test]
    fn recording_limit_closes_capture_while_the_model_is_still_loading() {
        let mut fixture = Fixture::new(None);
        fixture.expect_listening();
        fixture.capture_ended.store(true, Ordering::Release);
        fixture.microphone_closed.recv_timeout(WAIT).unwrap();
        fixture.expect_transcribing();
        assert!(fixture.result.try_recv().is_err());
        assert_eq!(
            fixture.finish_loading(Ok(())).unwrap().as_deref(),
            Some("retained speech")
        );
    }

    #[test]
    fn cancelled_load_keeps_new_sessions_refused_until_native_work_returns() {
        let mut fixture = Fixture::with_admission(None, Some(Admission::acquire().unwrap()));
        fixture.expect_listening();
        fixture.session.cancel.store(true, Ordering::Release);
        fixture.microphone_closed.recv_timeout(WAIT).unwrap();
        assert!(DICTATION_ACTIVE.load(Ordering::Acquire));
        assert!(DictationSession::start(Path::new(""), None).is_err());
        assert_eq!(fixture.finish_loading(Ok(())).unwrap(), None);
        assert!(!DICTATION_ACTIVE.load(Ordering::Acquire));

        let (sender, receiver) = sync_channel(1);
        sender.send(Admission::acquire().unwrap()).unwrap();
        drop(receiver);
        assert!(!DICTATION_ACTIVE.load(Ordering::Acquire));
    }

    #[test]
    fn stop_before_capture_starts_never_opens_the_microphone() {
        let (events, _) = unbounded();
        let job = Job {
            model_directory: PathBuf::new(),
            stop: Arc::new(AtomicBool::new(true)),
            cancel: Arc::new(AtomicBool::new(false)),
            input_level: Arc::new(AtomicU32::new(0)),
            input_device_id: None,
            events,
        };
        let result = run_job::<FakeCapture, _>(
            &job,
            || panic!("the microphone must not open"),
            || Ok(|_: CapturedAudio| -> Result<String> { panic!("there is no audio") }),
        )
        .unwrap();
        assert_eq!(result.as_deref(), Some(""));
    }

    #[test]
    fn cancel_during_device_open_does_not_emit_listening_or_transcribe() {
        let (device_opened_sender, device_opened) = sync_channel(1);
        let (device_opening, device_opening_started) = sync_channel(1);
        let mut fixture = Fixture::new(Some((device_opened, device_opening)));
        device_opening_started.recv_timeout(WAIT).unwrap();
        fixture.session.cancel.store(true, Ordering::Release);
        device_opened_sender.send(()).unwrap();
        fixture.microphone_closed.recv_timeout(WAIT).unwrap();
        assert!(!fixture.has_pending_event());
        assert_eq!(fixture.finish_loading(Ok(())).unwrap(), None);
    }

    #[test]
    fn ready_model_waits_for_stop_and_transcribes_once() {
        let mut fixture = Fixture::new(None);
        fixture.expect_listening();
        fixture.finish_loading.send(Ok(())).unwrap();
        assert!(fixture.result.try_recv().is_err());
        fixture.session.stop_and_transcribe();
        fixture.session.stop_and_transcribe();
        assert_eq!(
            fixture
                .result
                .recv_timeout(WAIT)
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("retained speech")
        );
        fixture.microphone_closed.try_recv().unwrap();
        fixture.worker.join().unwrap();
    }

    #[test]
    fn input_level_is_reset_after_each_read() {
        let session = DictationSession {
            stop: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            input_level: Arc::new(AtomicU32::new(0.5_f32.to_bits())),
        };
        assert_eq!(session.take_peak_input_level(), 0.5);
        assert_eq!(session.take_peak_input_level(), 0.0);
    }
}
