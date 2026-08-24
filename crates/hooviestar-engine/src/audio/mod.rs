pub mod journal;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod linux_runtime;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub mod windows_runtime;

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use parking_lot::Mutex;
use uuid::Uuid;

pub type MediaAudioBus = Arc<Mutex<HashMap<Uuid, Arc<Mutex<PcmRing>>>>>;

pub fn media_audio_bus() -> MediaAudioBus {
    Arc::new(Mutex::new(HashMap::new()))
}

pub const SAMPLE_RATE: u32 = 48_000;
pub const LIMITER_CEILING: f32 = 0.891_250_9;

#[derive(Debug)]
pub struct PcmRing {
    samples: VecDeque<[f32; 2]>,
    capacity: usize,
    overruns: u64,
    underruns: u64,
    active: bool,
}
impl PcmRing {
    pub fn new(capacity_frames: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity_frames),
            capacity: capacity_frames,
            overruns: 0,
            underruns: 0,
            active: true,
        }
    }
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        if !active {
            self.samples.clear();
        }
    }
    pub fn push(&mut self, frame: [f32; 2]) {
        if !self.active {
            return;
        }
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
            self.overruns += 1
        }
        self.samples.push_back(frame)
    }
    pub fn pop(&mut self) -> [f32; 2] {
        self.samples.pop_front().unwrap_or_else(|| {
            if self.active {
                self.underruns += 1;
            }
            [0.0; 2]
        })
    }
    pub fn overruns(&self) -> u64 {
        self.overruns
    }
    pub fn underruns(&self) -> u64 {
        self.underruns
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GainRamp {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
}
impl GainRamp {
    pub fn new(gain: f32) -> Self {
        Self {
            current: gain.clamp(0.0, 1.0),
            target: gain.clamp(0.0, 1.0),
            step: 0.0,
            remaining: 0,
        }
    }
    pub fn set(&mut self, gain: f32, frames: u32) {
        self.target = gain.clamp(0.0, 1.0);
        self.remaining = frames;
        if frames == 0 {
            self.current = self.target;
            self.step = 0.0
        } else {
            self.step = (self.target - self.current) / frames as f32
        }
    }
    pub fn next_gain(&mut self) -> f32 {
        if self.remaining > 0 {
            self.current += self.step;
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target
            }
        }
        self.current
    }
}

pub fn mix_into(output: &mut [[f32; 2]], inputs: &mut [(&mut PcmRing, &mut GainRamp, bool)]) {
    for frame in output {
        let mut mixed = [0.0f32; 2];
        for (ring, gain, muted) in inputs.iter_mut() {
            let source = ring.pop();
            let g = if *muted { 0.0 } else { gain.next_gain() };
            mixed[0] += source[0] * g;
            mixed[1] += source[1] * g
        }
        let peak = mixed[0].abs().max(mixed[1].abs());
        let limiter = if peak > LIMITER_CEILING {
            LIMITER_CEILING / peak
        } else {
            1.0
        };
        *frame = [mixed[0] * limiter, mixed[1] * limiter]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_ring_reports_overrun() {
        let mut r = PcmRing::new(2);
        r.push([1.0; 2]);
        r.push([2.0; 2]);
        r.push([3.0; 2]);
        assert_eq!(r.overruns(), 1);
        assert_eq!(r.pop(), [2.0; 2])
    }
    #[test]
    fn inactive_ring_returns_silence_without_underrun() {
        let mut ring = PcmRing::new(2);
        ring.set_active(false);
        assert_eq!(ring.pop(), [0.0; 2]);
        assert_eq!(ring.underruns(), 0);
    }
    #[test]
    fn limiter_caps_mix() {
        let mut a = PcmRing::new(1);
        let mut b = PcmRing::new(1);
        a.push([1.0; 2]);
        b.push([1.0; 2]);
        let mut ga = GainRamp::new(1.0);
        let mut gb = GainRamp::new(1.0);
        let mut out = [[0.0; 2]];
        mix_into(
            &mut out,
            &mut [(&mut a, &mut ga, false), (&mut b, &mut gb, false)],
        );
        assert!((out[0][0] - LIMITER_CEILING).abs() < 1e-6)
    }
}
