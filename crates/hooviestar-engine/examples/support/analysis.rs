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
        self.marker_observation()
            .map(|observation| observation.marker)
    }

    /// Findet den groessten zusammenhaengenden Markerblock. Eine blosse
    /// globale Farbmenge reicht nicht: Ein Video mit wenigen magenta/cyan/
    /// gelb/blauen Pixeln darf keine Szene vortaeuschen.
    pub fn marker_observation(&self) -> Option<MarkerObservation> {
        const STEP: usize = 4;
        const NO_MARKER: u8 = u8::MAX;
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let grid_width = (self.width as usize).div_ceil(STEP);
        let grid_height = (self.height as usize).div_ceil(STEP);
        let sampled_pixels = grid_width.checked_mul(grid_height)?;
        let mut labels = vec![NO_MARKER; sampled_pixels];
        for grid_y in 0..grid_height {
            let y = (grid_y * STEP).min(self.height as usize - 1);
            for grid_x in 0..grid_width {
                let x = (grid_x * STEP).min(self.width as usize - 1);
                let offset = (y * self.width as usize + x) * 4;
                let [b, g, r, _] = self.pixels.get(offset..offset + 4)?.try_into().ok()?;
                labels[grid_y * grid_width + grid_x] =
                    marker_color(r, g, b).map(marker_index).unwrap_or(NO_MARKER);
            }
        }

        let mut visited = vec![false; sampled_pixels];
        let mut best = None;
        for start in 0..sampled_pixels {
            let label = labels[start];
            if label == NO_MARKER || visited[start] {
                continue;
            }
            let mut stack = vec![start];
            visited[start] = true;
            let mut count = 0usize;
            let mut min_x = grid_width;
            let mut min_y = grid_height;
            let mut max_x = 0usize;
            let mut max_y = 0usize;
            while let Some(index) = stack.pop() {
                count += 1;
                let x = index % grid_width;
                let y = index / grid_width;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                for neighbor in [
                    x.checked_sub(1).map(|next| y * grid_width + next),
                    (x + 1 < grid_width).then_some(y * grid_width + x + 1),
                    y.checked_sub(1).map(|next| next * grid_width + x),
                    (y + 1 < grid_height).then_some((y + 1) * grid_width + x),
                ]
                .into_iter()
                .flatten()
                {
                    if !visited[neighbor] && labels[neighbor] == label {
                        visited[neighbor] = true;
                        stack.push(neighbor);
                    }
                }
            }

            let component_width = max_x - min_x + 1;
            let component_height = max_y - min_y + 1;
            let bounding_area = component_width * component_height;
            let component_fraction = count as f64 / sampled_pixels as f64;
            let aspect_ratio = component_width as f64 / component_height as f64;
            let fill_ratio = count as f64 / bounding_area as f64;
            let plausible = (0.00125..=0.125).contains(&component_fraction)
                && component_width >= (grid_width / 40).max(4)
                && component_height >= (grid_height / 80).max(2)
                && (1.8..=7.0).contains(&aspect_ratio)
                && fill_ratio >= 0.45;
            if !plausible {
                continue;
            }
            let observation = MarkerObservation {
                marker: marker_from_index(label),
                component_pixels: count,
                sampled_pixels,
                component_fraction,
                fill_ratio,
                bounds: MarkerBounds {
                    left: (min_x * STEP) as u32,
                    top: (min_y * STEP) as u32,
                    right: ((max_x + 1) * STEP).min(self.width as usize) as u32,
                    bottom: ((max_y + 1) * STEP).min(self.height as usize) as u32,
                },
            };
            if best.as_ref().is_none_or(|current: &MarkerObservation| {
                observation.component_pixels > current.component_pixels
            }) {
                best = Some(observation);
            }
        }
        best
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

    pub fn difference_ratio_in_rect(
        &self,
        other: &Self,
        left: u32,
        top: u32,
        width: u32,
        height: u32,
        threshold: u16,
    ) -> f64 {
        if self.width != other.width
            || self.height != other.height
            || width == 0
            || height == 0
            || left.saturating_add(width) > self.width
            || top.saturating_add(height) > self.height
        {
            return 0.0;
        }
        let mut changed = 0usize;
        let mut compared = 0usize;
        for y in (top..top + height).step_by(2) {
            for x in (left..left + width).step_by(2) {
                let offset = (y as usize * self.width as usize + x as usize) * 4;
                let left_pixel = &self.pixels[offset..offset + 4];
                let right_pixel = &other.pixels[offset..offset + 4];
                let delta = left_pixel[0].abs_diff(right_pixel[0]) as u16
                    + left_pixel[1].abs_diff(right_pixel[1]) as u16
                    + left_pixel[2].abs_diff(right_pixel[2]) as u16;
                compared += 1;
                if delta > threshold {
                    changed += 1;
                }
            }
        }
        changed as f64 / compared.max(1) as f64
    }
}

fn marker_color(red: u8, green: u8, blue: u8) -> Option<Marker> {
    if red > 180 && blue > 180 && green < 110 {
        Some(Marker::Browser)
    } else if green > 180 && blue > 180 && red < 110 {
        Some(Marker::Tone)
    } else if red > 180 && green > 180 && blue < 110 {
        Some(Marker::Mixed)
    } else if blue > 180 && red < 100 && green < 100 {
        Some(Marker::Muted)
    } else {
        None
    }
}

fn marker_index(marker: Marker) -> u8 {
    match marker {
        Marker::Browser => 0,
        Marker::Tone => 1,
        Marker::Mixed => 2,
        Marker::Muted => 3,
    }
}

fn marker_from_index(index: u8) -> Marker {
    [Marker::Browser, Marker::Tone, Marker::Mixed, Marker::Muted][index as usize]
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Marker {
    Browser,
    Tone,
    Mixed,
    Muted,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerBounds {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerObservation {
    pub marker: Marker,
    pub component_pixels: usize,
    pub sampled_pixels: usize,
    pub component_fraction: f64,
    pub fill_ratio: f64,
    pub bounds: MarkerBounds,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionMetrics {
    pub compared_frame_pairs: usize,
    pub moving_frame_pairs: usize,
    pub moving_frame_fraction: f64,
    pub mean_motion_ratio: f64,
    pub median_motion_ratio: f64,
    pub maximum_motion_ratio: f64,
    pub longest_frozen_run: usize,
}

impl MotionMetrics {
    pub fn sustained(
        &self,
        minimum_pairs: usize,
        minimum_moving_fraction: f64,
        minimum_median_ratio: f64,
        maximum_frozen_run: usize,
    ) -> bool {
        self.compared_frame_pairs >= minimum_pairs
            && self.moving_frame_fraction >= minimum_moving_fraction
            && self.median_motion_ratio > minimum_median_ratio
            && self.longest_frozen_run <= maximum_frozen_run
    }
}

pub fn summarize_motion(ratios: &[f64], moving_threshold: f64) -> MotionMetrics {
    if ratios.is_empty() {
        return MotionMetrics::default();
    }
    let mut sorted = ratios.to_vec();
    sorted.sort_by(f64::total_cmp);
    let moving_frame_pairs = ratios
        .iter()
        .filter(|ratio| **ratio > moving_threshold)
        .count();
    let mut longest_frozen_run = 0usize;
    let mut frozen_run = 0usize;
    for ratio in ratios {
        if *ratio > moving_threshold {
            frozen_run = 0;
        } else {
            frozen_run += 1;
            longest_frozen_run = longest_frozen_run.max(frozen_run);
        }
    }
    MotionMetrics {
        compared_frame_pairs: ratios.len(),
        moving_frame_pairs,
        moving_frame_fraction: moving_frame_pairs as f64 / ratios.len() as f64,
        mean_motion_ratio: ratios.iter().sum::<f64>() / ratios.len() as f64,
        median_motion_ratio: if sorted.len().is_multiple_of(2) {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) * 0.5
        } else {
            sorted[sorted.len() / 2]
        },
        maximum_motion_ratio: sorted.last().copied().unwrap_or_default(),
        longest_frozen_run,
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalMetrics {
    pub sample_count: usize,
    pub rms: f64,
    pub peak: f64,
    pub dc_offset: f64,
    pub clipped_sample_ratio: f64,
    pub crest_factor: f64,
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
    let dc_offset = samples.iter().sum::<f64>() / count;
    let rms = (samples.iter().map(|sample| sample * sample).sum::<f64>() / count).sqrt();
    let peak = samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0f64, f64::max);
    let clipped_sample_ratio = samples
        .iter()
        .filter(|sample| sample.abs() >= 0.999)
        .count() as f64
        / count;
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
        sample_count: samples.len(),
        rms,
        peak,
        dc_offset,
        clipped_sample_ratio,
        crest_factor: if rms > 0.0 { peak / rms } else { 0.0 },
        amplitudes,
    }
}

pub fn gain_matches(actual: f64, reference: f64, expected: f64, tolerance: f64) -> bool {
    reference.is_finite()
        && reference > 0.0
        && actual.is_finite()
        && ((actual / reference) - expected).abs() <= tolerance
}

pub fn cadence_scaled_30_to_60(observed_30: f64, observed_60: f64) -> bool {
    observed_30.is_finite()
        && observed_60.is_finite()
        && (22.0..=36.0).contains(&observed_30)
        && (45.0..=72.0).contains(&observed_60)
        && observed_60 >= observed_30 * 1.5
}

pub fn capture_cadence_healthy(observed_30: f64, observed_60: f64) -> bool {
    observed_30.is_finite()
        && observed_60.is_finite()
        && (22.0..=72.0).contains(&observed_30)
        && (22.0..=72.0).contains(&observed_60)
}

pub fn quiet_enough(
    muted: &SignalMetrics,
    active_rms: f64,
    relative_ceiling: f64,
    absolute_ceiling: f64,
) -> bool {
    muted.sample_count > 0
        && active_rms.is_finite()
        && active_rms > 0.0
        && muted.rms < active_rms * relative_ceiling
        && muted.rms < absolute_ceiling
}
