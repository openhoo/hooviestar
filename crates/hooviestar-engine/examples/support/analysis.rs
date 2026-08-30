#![allow(dead_code)]

use std::collections::HashMap;

pub const BROWSER_FREQUENCY_HZ: f64 = 660.0;
pub const TONE_FREQUENCY_HZ: f64 = 440.0;
const SAMPLE_RATE: f64 = 48_000.0;

pub fn runtime_audio_process_id(runtime_id: &str) -> Option<u32> {
    runtime_id.split('%').find_map(|part| {
        part.parse::<u32>()
            .ok()
            .or_else(|| part.strip_prefix('b')?.parse::<u32>().ok())
    })
}

#[derive(Clone, Debug)]
pub struct BgraFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl BgraFrame {
    pub fn write_ppm(&self, path: &std::path::Path) -> Result<(), String> {
        let mut bytes = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        bytes.reserve(self.width as usize * self.height as usize * 3);
        for pixel in self.pixels.as_chunks::<4>().0 {
            bytes.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        std::fs::write(path, bytes).map_err(|error| error.to_string())
    }

    pub fn marker(&self) -> Option<Marker> {
        let mut counts = [0usize; 4];
        for pixel in self.pixels.as_chunks::<4>().0.iter().step_by(4) {
            let [b, g, r, _] = *pixel;
            if r > 180 && b > 180 && g < 110 {
                counts[0] += 1;
            } else if g > 180 && b > 180 && r < 110 {
                counts[1] += 1;
            } else if r > 180 && g > 180 && b < 110 {
                counts[2] += 1;
            } else if b > 180 && r < 100 && g < 100 {
                counts[3] += 1;
            }
        }
        let (index, count) = counts
            .iter()
            .copied()
            .enumerate()
            .max_by_key(|(_, count)| *count)?;
        let sampled_pixels = self.pixels.len() / 16;
        if count < sampled_pixels / 500 {
            return None;
        }
        Some([Marker::Browser, Marker::Tone, Marker::Mixed, Marker::Muted][index])
    }

    pub fn motion_ratio(&self, newer: &Self) -> f64 {
        if self.width != newer.width || self.height != newer.height {
            return 1.0;
        }
        let mut changed = 0usize;
        let mut compared = 0usize;
        for (left, right) in self
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .step_by(8)
            .zip(newer.pixels.as_chunks::<4>().0.iter().step_by(8))
        {
            compared += 1;
            let delta = left[0].abs_diff(right[0]) as u16
                + left[1].abs_diff(right[1]) as u16
                + left[2].abs_diff(right[2]) as u16;
            if delta > 36 {
                changed += 1;
            }
        }
        if compared == 0 {
            0.0
        } else {
            changed as f64 / compared as f64
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Marker {
    Browser,
    Tone,
    Mixed,
    Muted,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SignalMetrics {
    pub rms: f64,
    pub peak: f64,
    pub amplitudes: HashMap<String, f64>,
}

impl SignalMetrics {
    pub fn amplitude(&self, frequency: f64) -> f64 {
        self.amplitudes
            .get(&format!("{frequency:.0}Hz"))
            .copied()
            .unwrap_or_default()
    }
}

pub fn analyze_signal(samples: &[f64], frequencies: &[f64]) -> SignalMetrics {
    let count = samples.len().max(1) as f64;
    let rms = (samples.iter().map(|sample| sample * sample).sum::<f64>() / count).sqrt();
    let peak = samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0f64, f64::max);
    let mut amplitudes = HashMap::new();
    for frequency in frequencies {
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for (index, sample) in samples.iter().enumerate() {
            let phase = std::f64::consts::TAU * frequency * index as f64 / SAMPLE_RATE;
            let window = if samples.len() <= 1 {
                1.0
            } else {
                0.5 - 0.5
                    * (std::f64::consts::TAU * index as f64 / (samples.len() - 1) as f64).cos()
            };
            real += sample * window * phase.cos();
            imaginary -= sample * window * phase.sin();
        }
        let amplitude = 4.0 * (real * real + imaginary * imaginary).sqrt() / count;
        amplitudes.insert(format!("{frequency:.0}Hz"), amplitude);
    }
    SignalMetrics {
        rms,
        peak,
        amplitudes,
    }
}
