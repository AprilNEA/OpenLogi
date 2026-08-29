//! Loopback RMS and coarse bands for the audio visualizer.
//!
//! The tile is offered only when a loopback/monitor device exists. Default
//! input (mic) is never opened, so a platform without loopback hides the
//! effect instead of prompting for a microphone.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static HAS_LOOPBACK: OnceLock<bool> = OnceLock::new();
static START: std::sync::Once = std::sync::Once::new();
static OK: AtomicBool = AtomicBool::new(false);
static RMS: AtomicU32 = AtomicU32::new(0);
static BANDS: [AtomicU32; 8] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

/// Whether a loopback capture device is present (does not start the stream).
#[must_use]
pub fn available() -> bool {
    *HAS_LOOPBACK.get_or_init(|| pick_loopback(&cpal::default_host()).is_some())
}

/// Latest RMS level in `0.0..=1.0`.
#[must_use]
pub fn rms() -> f32 {
    ensure();
    f32::from_bits(RMS.load(Ordering::Relaxed))
}

/// Eight coarse spectral bands in `0.0..=1.0`.
#[must_use]
pub fn bands() -> [f32; 8] {
    ensure();
    core::array::from_fn(|index| f32::from_bits(BANDS[index].load(Ordering::Relaxed)))
}

fn ensure() {
    if !available() {
        return;
    }
    START.call_once(|| {
        if let Err(error) = spawn_stream() {
            tracing::debug!(error = %error, "audio visualizer backend unavailable");
            OK.store(false, Ordering::Relaxed);
        }
    });
}

fn spawn_stream() -> Result<(), String> {
    use cpal::SampleFormat;
    use cpal::traits::{DeviceTrait, StreamTrait};

    let host = cpal::default_host();
    let device = pick_loopback(&host).ok_or_else(|| "no loopback device".to_string())?;
    let config = device
        .default_input_config()
        .map_err(|error| error.to_string())?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let stream = match config.sample_format() {
        SampleFormat::F32 => build_stream::<f32>(&device, &config.config(), channels, sample_rate)?,
        SampleFormat::I16 => build_stream::<i16>(&device, &config.config(), channels, sample_rate)?,
        SampleFormat::U16 => build_stream::<u16>(&device, &config.config(), channels, sample_rate)?,
        other => return Err(format!("unsupported capture format {other}")),
    };
    stream.play().map_err(|error| error.to_string())?;
    std::mem::forget(stream);
    OK.store(true, Ordering::Relaxed);
    Ok(())
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: u16,
    sample_rate: u32,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + Send + 'static,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait;
    device
        .build_input_stream(
            *config,
            move |data: &[T], _| ingest(data, channels, sample_rate),
            |error| tracing::debug!(error = %error, "audio visualizer stream error"),
            None,
        )
        .map_err(|error| error.to_string())
}

fn pick_loopback(host: &cpal::Host) -> Option<cpal::Device> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let devices = host.input_devices().ok()?;
    devices.into_iter().find(|device| {
        let name = device
            .description()
            .map(|description| description.name().to_lowercase())
            .unwrap_or_default();
        name.contains("monitor") || name.contains("loopback") || name.contains("stereo mix")
    })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "audio RMS is averaged over a few hundred PCM samples"
)]
fn ingest<T>(data: &[T], channels: u16, sample_rate: u32)
where
    T: cpal::Sample,
    f32: cpal::FromSample<T>,
{
    if data.is_empty() {
        return;
    }
    let ch = usize::from(channels.max(1));
    let ch_f = f32::from(channels.max(1));
    let mut sum = 0.0f32;
    let mut n = 0u32;
    let mut bands = [0.0f32; 8];
    let mut counts = [0u32; 8];
    for frame in data.chunks(ch) {
        let sample = frame
            .iter()
            .map(|value| value.to_sample::<f32>())
            .sum::<f32>()
            / ch_f;
        sum += sample * sample;
        n += 1;
        let band = band_for(n, sample_rate);
        bands[band] += sample.abs();
        counts[band] += 1;
    }
    let rms = if n == 0 {
        0.0
    } else {
        (sum / n as f32).sqrt().clamp(0.0, 1.0)
    };
    RMS.store(rms.to_bits(), Ordering::Relaxed);
    for i in 0..8 {
        let value = if counts[i] == 0 {
            0.0
        } else {
            (bands[i] / counts[i] as f32).clamp(0.0, 1.0)
        };
        BANDS[i].store(value.to_bits(), Ordering::Relaxed);
    }
}

fn band_for(sample_index: u32, sample_rate: u32) -> usize {
    let hz = sample_index % sample_rate.max(1);
    usize::try_from(hz / 500).unwrap_or(7).min(7)
}
