use anyhow::{Result, ensure};
use rubato::{FftFixedInOut, Resampler};

pub(crate) const MODEL_SAMPLE_RATE: u32 = 16_000;

pub(crate) fn is_supported_sample_rate(sample_rate: u32) -> bool {
    (8_000..=192_000).contains(&sample_rate)
}

pub(crate) fn to_model_sample_rate(samples: Vec<f32>, sample_rate: u32) -> Result<Vec<f32>> {
    ensure!(
        is_supported_sample_rate(sample_rate),
        "Unsupported microphone sample rate"
    );
    if sample_rate == MODEL_SAMPLE_RATE || samples.is_empty() {
        return Ok(samples);
    }
    let length = (samples.len() as u64 * MODEL_SAMPLE_RATE as u64 / sample_rate as u64) as usize;
    let mut resampler =
        FftFixedInOut::<f32>::new(sample_rate as usize, MODEL_SAMPLE_RATE as usize, 1024, 1)?;
    let delay = resampler.output_delay();
    let chunk_length = resampler.input_frames_next();
    let mut input = vec![vec![0.0; chunk_length]];
    let mut output = resampler.output_buffer_allocate(true);
    let mut converted = Vec::with_capacity(length + delay + resampler.output_frames_max());
    let mut offset = 0;
    // Zero padding past the end flushes the filter tail; the delay is trimmed below.
    while converted.len() < length + delay {
        let end = (offset + chunk_length).min(samples.len());
        if let (Some(input_channel), Some(source)) = (input.first_mut(), samples.get(offset..end)) {
            input_channel.fill(0.0);
            input_channel[..source.len()].copy_from_slice(source);
        }
        let (_, written) = resampler.process_into_buffer(&input, &mut output, None)?;
        if let Some(output_channel) = output.first().and_then(|channel| channel.get(..written)) {
            converted.extend_from_slice(output_channel);
        }
        offset = end;
    }
    converted.drain(..delay);
    converted.truncate(length);
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(sample_rate: u32, frequency: f32) -> Vec<f32> {
        (0..sample_rate)
            .map(|index| {
                (std::f32::consts::TAU * frequency * index as f32 / sample_rate as f32).sin()
            })
            .collect()
    }

    fn root_mean_square(samples: &[f32]) -> f32 {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn device_rates_preserve_duration_and_speech_band() {
        for sample_rate in [8_000, 16_000, 24_000, 44_100, 48_000, 96_000] {
            let converted = to_model_sample_rate(tone(sample_rate, 1_000.0), sample_rate).unwrap();
            assert_eq!(converted.len(), MODEL_SAMPLE_RATE as usize, "{sample_rate}");
            // Noninteger ratios shift phase slightly, so phase is not checked.
            let steady = &converted[1000..15000];
            let cycles = steady
                .windows(2)
                .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
                .count();
            assert!((874..=876).contains(&cycles), "{sample_rate}: {cycles}");
            assert!(
                (root_mean_square(steady) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01,
                "{sample_rate}"
            );
        }
    }

    #[test]
    fn downsampling_filters_frequencies_above_model_nyquist() {
        let converted = to_model_sample_rate(tone(48_000, 12_000.0), 48_000).unwrap();
        assert!(root_mean_square(&converted[1000..15000]) < 0.01);
    }

    #[test]
    fn short_capture_and_invalid_rates_are_bounded() {
        assert_eq!(
            to_model_sample_rate(vec![0.0; 147], 44_100).unwrap().len(),
            53
        );
        assert!(to_model_sample_rate(Vec::new(), 48_000).unwrap().is_empty());
        assert!(to_model_sample_rate(vec![1.0], 0).is_err());
    }
}
