use anyhow::{Context as _, Result, bail};
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
}

/// Can block on the system audio server, so call it off the UI thread.
pub fn input_devices() -> Result<Vec<InputDevice>> {
    let devices = cpal::default_host()
        .input_devices()
        .context("Could not list microphones")?;
    let mut devices: Vec<InputDevice> = devices
        .filter_map(|device| {
            Some(InputDevice {
                id: device.id().ok()?.to_string(),
                name: device.description().ok()?.name().to_owned(),
            })
        })
        .collect();
    devices.dedup_by(|first, second| first.id == second.id);
    Ok(devices)
}

/// Can block on the system audio server, so call it off the UI thread.
pub fn default_input_device_id() -> Option<String> {
    Some(
        cpal::default_host()
            .default_input_device()?
            .id()
            .ok()?
            .to_string(),
    )
}

fn input_device_or_default(id: Option<&str>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    id.and_then(|id| id.parse::<cpal::DeviceId>().ok())
        .and_then(|id| host.device_by_id(&id))
        .or_else(|| host.default_input_device())
}

pub(crate) struct CapturedAudio {
    pub(crate) samples: Vec<f32>,
    pub(crate) sample_rate: u32,
}

pub(crate) struct AudioBuffer {
    samples: Mutex<Vec<f32>>,
    // Kept outside the mutex so polling them never makes the audio callback's try_lock drop a buffer.
    failed: AtomicBool,
    full: AtomicBool,
}

impl AudioBuffer {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            samples: Mutex::new(Vec::with_capacity(capacity)),
            failed: AtomicBool::new(false),
            full: AtomicBool::new(false),
        }
    }

    pub(crate) fn append<T: cpal::Sample>(&self, data: &[T], channels: usize, sample_rate: u32)
    where
        f32: cpal::FromSample<T>,
    {
        let Ok(mut samples) = self.samples.try_lock() else {
            return;
        };
        let max_samples = crate::max_recording_samples(sample_rate);
        for frame in data.chunks_exact(channels.max(1)) {
            if samples.len() >= max_samples {
                self.full.store(true, Ordering::Release);
                break;
            }
            samples.push(mono_sample(frame));
        }
    }

    #[cfg(test)]
    pub(crate) fn samples(&self) -> Vec<f32> {
        self.samples
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn is_full(&self) -> bool {
        self.full.load(Ordering::Acquire)
    }
}

fn mono_sample<T: cpal::Sample>(frame: &[T]) -> f32
where
    f32: cpal::FromSample<T>,
{
    frame
        .iter()
        .map(|sample| sample.to_sample::<f32>())
        .sum::<f32>()
        / frame.len().max(1) as f32
}

/// Non-negative `f32` bit patterns order like their values, so `fetch_max` keeps the peak without a lock.
pub(crate) fn record_peak_level<T: cpal::Sample>(data: &[T], channels: usize, level: &AtomicU32)
where
    f32: cpal::FromSample<T>,
{
    let channels = channels.max(1);
    let frames = data.len() / channels;
    if frames == 0 {
        return;
    }
    let energy = data
        .chunks_exact(channels)
        .map(|frame| {
            let sample = mono_sample(frame);
            sample * sample
        })
        .sum::<f32>();
    let root_mean_square = (energy / frames as f32).sqrt();
    if root_mean_square.is_finite() {
        level.fetch_max(root_mean_square.to_bits(), Ordering::Relaxed);
    }
}

pub(crate) trait Recording {
    fn ended(&self) -> bool;
    fn finish(self) -> Result<CapturedAudio>;
}

pub(crate) struct Capture {
    stream: Option<cpal::Stream>,
    audio: Arc<AudioBuffer>,
    sample_rate: u32,
}

impl Capture {
    pub(crate) fn start(device_id: Option<&str>, input_level: Arc<AtomicU32>) -> Result<Self> {
        let device = input_device_or_default(device_id).context("No microphone is available")?;
        let config = device.default_input_config()?;
        let sample_rate = config.sample_rate();
        let channels = config.channels() as usize;
        let audio = Arc::new(AudioBuffer::with_capacity(crate::max_recording_samples(
            sample_rate,
        )));
        let on_error = {
            let audio = audio.clone();
            move |_| audio.failed.store(true, Ordering::Release)
        };
        macro_rules! build_stream {
            ($sample:ty) => {{
                let audio = audio.clone();
                device.build_input_stream(
                    &config.into(),
                    move |data: &[$sample], _| {
                        record_peak_level(data, channels, &input_level);
                        audio.append(data, channels, sample_rate);
                    },
                    on_error,
                    None,
                )?
            }};
        }
        let stream = match config.sample_format() {
            cpal::SampleFormat::I8 => build_stream!(i8),
            cpal::SampleFormat::I16 => build_stream!(i16),
            cpal::SampleFormat::I24 => build_stream!(cpal::I24),
            cpal::SampleFormat::I32 => build_stream!(i32),
            cpal::SampleFormat::I64 => build_stream!(i64),
            cpal::SampleFormat::U8 => build_stream!(u8),
            cpal::SampleFormat::U16 => build_stream!(u16),
            cpal::SampleFormat::U24 => build_stream!(cpal::U24),
            cpal::SampleFormat::U32 => build_stream!(u32),
            cpal::SampleFormat::U64 => build_stream!(u64),
            cpal::SampleFormat::F32 => build_stream!(f32),
            cpal::SampleFormat::F64 => build_stream!(f64),
            format => bail!("Unsupported microphone sample format {format}"),
        };
        stream.play()?;
        Ok(Self {
            stream: Some(stream),
            audio,
            sample_rate,
        })
    }
}

impl Recording for Capture {
    fn ended(&self) -> bool {
        self.audio.full.load(Ordering::Acquire) || self.audio.failed.load(Ordering::Acquire)
    }

    fn finish(mut self) -> Result<CapturedAudio> {
        self.stream.take();
        if self.audio.failed.load(Ordering::Acquire) {
            bail!("The microphone disconnected");
        }
        let mut samples = self
            .audio
            .samples
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Ok(CapturedAudio {
            samples: std::mem::take(&mut *samples),
            sample_rate: self.sample_rate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::Sample as _;

    #[test]
    fn native_sample_formats_preserve_levels_and_downmix_channels() {
        fn check<T: cpal::Sample + cpal::FromSample<f32>>()
        where
            f32: cpal::FromSample<T>,
        {
            let audio = AudioBuffer::with_capacity(0);
            let input: Vec<T> = [-0.5_f32, 0.5, 0.25, 0.75, 0.0, 0.0]
                .into_iter()
                .map(|sample| sample.to_sample::<T>())
                .collect();
            audio.append(&input, 2, 16_000);
            assert_eq!(audio.samples(), vec![0.0, 0.5, 0.0]);
            let level = AtomicU32::new(0);
            record_peak_level(&input, 2, &level);
            let peak = f32::from_bits(level.load(Ordering::Relaxed));
            assert!((peak - (0.25_f32 / 3.0).sqrt()).abs() < 0.01, "{peak}");
            record_peak_level(&input[4..], 2, &level);
            assert_eq!(f32::from_bits(level.load(Ordering::Relaxed)), peak);
        }
        check::<i8>();
        check::<i16>();
        check::<cpal::I24>();
        check::<i32>();
        check::<i64>();
        check::<u8>();
        check::<u16>();
        check::<cpal::U24>();
        check::<u32>();
        check::<u64>();
        check::<f32>();
        check::<f64>();
    }

    #[test]
    fn audio_buffer_caps_recording_at_the_limit() {
        let audio = AudioBuffer::with_capacity(0);
        let sample_rate = 8_000;
        let max_samples = crate::max_recording_samples(sample_rate);
        audio.append(&vec![0.25_f32; max_samples + 1], 1, sample_rate);
        assert_eq!(audio.samples().len(), sample_rate as usize * 60);
        assert!(audio.is_full());
    }
}
