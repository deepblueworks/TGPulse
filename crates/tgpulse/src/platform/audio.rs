//! Audio output: carries the sound board's samples to the host device.
//!
//! The board renders at its own clock (10MHz/224 = 44642.857Hz) and the host
//! device runs at whatever it likes, so the callback walks the ring buffer
//! with a fractional step and linear interpolation. A DC-blocking filter
//! stands in for the amplifier's AC coupling: the MultiPCM's 8-bit looped
//! samples carry real DC offsets that a speaker would never see.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Keep roughly this much audio queued (in board samples). Small enough to
/// stay responsive, large enough to ride out frame-time jitter.
const TARGET_QUEUE: usize = 8_192;

/// Volume is carried as hundredths of a percent so it fits an atomic integer
/// without a lock in the audio callback.
const GAIN_SCALE: f32 = 10_000.0;

pub struct Audio {
    ring: Arc<Mutex<VecDeque<(i16, i16)>>>,
    gain: Arc<std::sync::atomic::AtomicU32>,
    _stream: Option<cpal::Stream>,
}

struct Resampler {
    ring: Arc<Mutex<VecDeque<(i16, i16)>>>,
    step: f64,
    frac: f64,
    cur: (f32, f32),
    next: (f32, f32),
    // DC blocker state, one per channel: y[n] = x[n] - x[n-1] + R*y[n-1]
    dc_x: (f32, f32),
    dc_y: (f32, f32),
    // User volume, a plain digital gain on the mixed output. Shared with the
    // owning `Audio` so the settings panel can change it while the stream runs.
    gain: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl Resampler {
    fn pull(&mut self) -> (f32, f32) {
        let mut ring = self.ring.lock().unwrap();
        match ring.pop_front() {
            Some((l, r)) => (l as f32, r as f32),
            None => self.next, // underrun: hold the last sample
        }
    }

    fn render(&mut self, out: &mut [f32], channels: usize) {
        let g = self.gain.load(std::sync::atomic::Ordering::Relaxed) as f32 / GAIN_SCALE;
        for frame in out.chunks_mut(channels) {
            self.frac += self.step;
            while self.frac >= 1.0 {
                self.frac -= 1.0;
                self.cur = self.next;
                self.next = self.pull();
            }
            let t = self.frac as f32;
            let l = self.cur.0 + (self.next.0 - self.cur.0) * t;
            let r = self.cur.1 + (self.next.1 - self.cur.1) * t;

            const R: f32 = 0.9995;
            let hl = l - self.dc_x.0 + R * self.dc_y.0;
            let hr = r - self.dc_x.1 + R * self.dc_y.1;
            self.dc_x = (l, r);
            self.dc_y = (hl, hr);

            let (l, r) = (
                (hl / 32768.0 * g).clamp(-1.0, 1.0),
                (hr / 32768.0 * g).clamp(-1.0, 1.0),
            );
            frame[0] = l;
            if channels > 1 {
                frame[1] = r;
            }
        }
    }
}

impl Audio {
    pub fn new(board_rate: f32, volume_pct: u32) -> Self {
        let ring: Arc<Mutex<VecDeque<(i16, i16)>>> = Arc::default();
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(
            (volume_pct as f32 / 100.0 * GAIN_SCALE) as u32,
        ));

        let stream = (|| -> Option<cpal::Stream> {
            let host = cpal::default_host();
            let device = host.default_output_device()?;
            let config = device.default_output_config().ok()?;
            let dev_rate = config.sample_rate().0 as f64;
            let channels = config.channels() as usize;
            log::info!(
                target: "audio",
                "{} at {}Hz ({}ch); board at {:.0}Hz",
                device.name().unwrap_or_default(),
                dev_rate,
                channels,
                board_rate
            );
            let mut rs = Resampler {
                ring: ring.clone(),
                step: board_rate as f64 / dev_rate,
                frac: 0.0,
                cur: (0.0, 0.0),
                next: (0.0, 0.0),
                dc_x: (0.0, 0.0),
                dc_y: (0.0, 0.0),
                gain: gain.clone(),
            };
            let stream = device
                .build_output_stream(
                    &config.into(),
                    move |out: &mut [f32], _| rs.render(out, channels),
                    |e| log::error!(target: "audio", "stream error: {e}"),
                    None,
                )
                .ok()?;
            stream.play().ok()?;
            Some(stream)
        })();

        if stream.is_none() {
            log::warn!(target: "audio", "no output device; running silent");
        }
        Self {
            ring,
            gain,
            _stream: stream,
        }
    }

    /// Changes the output volume while the stream is running.
    pub fn set_volume(&self, percent: u32) {
        self.gain.store(
            (percent as f32 / 100.0 * GAIN_SCALE) as u32,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Queues a batch of board samples, shedding the oldest if the queue has
    /// grown past the target (e.g. after a window drag paused presentation).
    pub fn push(&self, samples: impl Iterator<Item = (i16, i16)>) {
        let mut ring = self.ring.lock().unwrap();
        ring.extend(samples);
        let excess = ring.len().saturating_sub(TARGET_QUEUE);
        if excess > 0 {
            ring.drain(..excess);
        }
    }
}
