pub mod journal;
#[cfg(target_os = "linux")]
pub mod linux_runtime;
#[cfg(target_os = "windows")]
pub mod windows_runtime;

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, mpsc},
};

use parking_lot::Mutex;
use uuid::Uuid;

use crate::engine::EngineEvent;

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
    /// Verwirft alle gepufferten Frames, ohne Über-/Unterlaufzähler anzutasten.
    pub fn clear(&mut self) {
        self.samples.clear();
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
    pub fn filled_frames(&self) -> usize {
        self.samples.len()
    }
    /// Batch-Push: alle Stereo-Frames eines MF-Audio-Samples in einem Zug,
    /// gleiche Semantik wie push (bei Volllauf weichen die aeltesten Frames).
    /// Liefert die Anzahl uebernommener Frames.
    pub fn push_slice(&mut self, interleaved: &[f32]) -> usize {
        if !self.active {
            return 0;
        }
        let mut pushed = 0;
        for frame in interleaved.as_chunks::<2>().0 {
            if self.samples.len() == self.capacity {
                self.samples.pop_front();
                self.overruns += 1;
            }
            self.samples.push_back([frame[0], frame[1]]);
            pushed += 1;
        }
        pushed
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

/// Sendet Verfügbarkeitsereignisse nur bei echten Zustandsübergängen.
pub(crate) fn emit_availability(
    events: &mpsc::Sender<EngineEvent>,
    state: &mut HashMap<Uuid, bool>,
    source_id: Uuid,
    available: bool,
    reason: &str,
) {
    if state.get(&source_id) == Some(&available) {
        return;
    }
    state.insert(source_id, available);
    let event = if available {
        EngineEvent::SourceAvailable { source_id }
    } else {
        EngineEvent::SourceUnavailable {
            source_id,
            reason: reason.to_string(),
        }
    };
    let _ = events.send(event);
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
    fn push_slice_batches_and_wraps_like_push() {
        let mut r = PcmRing::new(4);
        // Ein Batch, interleaved 3 Stereo-Frames.
        assert_eq!(r.push_slice(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]), 3);
        assert_eq!(r.filled_frames(), 3);
        // Overflow um einen Frame: aeltester weicht, Reihenfolge bleibt.
        assert_eq!(r.push_slice(&[0.7, 0.8, 0.9, 1.0, 1.1, 1.2]), 3);
        // 3 Frames auf Fuellstand 3/Kapazitaet 4: zwei aelteste weichen.
        assert_eq!(r.overruns(), 2);
        assert_eq!(r.pop(), [0.5, 0.6]);
        assert_eq!(r.pop(), [0.7, 0.8]);
        assert_eq!(r.pop(), [0.9, 1.0]);
        assert_eq!(r.pop(), [1.1, 1.2]);
        assert_eq!(r.filled_frames(), 0);
    }
    #[test]
    fn push_slice_ignores_inactive_ring() {
        let mut r = PcmRing::new(2);
        r.set_active(false);
        assert_eq!(r.push_slice(&[1.0, 2.0]), 0);
        assert_eq!(r.filled_frames(), 0);
    }
    #[test]
    fn push_slice_counts_partial_trailing_sample() {
        let mut r = PcmRing::new(8);
        // Ungerade f32-Anzahl: haelftloser letzter Wert wird verworfen,
        // genau wie bei chunks_exact(2).
        assert_eq!(r.push_slice(&[1.0, 2.0, 3.0]), 1);
        assert_eq!(r.pop(), [1.0, 2.0]);
    }
    #[test]
    fn media_audio_bus_returns_usable_empty_bus() {
        let bus = media_audio_bus();
        assert!(bus.lock().is_empty());
        // Bus ist keine toende Huelle: Eintrag rein, Ring direkt benutzbar.
        let id = uuid::Uuid::new_v4();
        bus.lock().insert(id, Arc::new(Mutex::new(PcmRing::new(4))));
        let mut rings = bus.lock();
        assert_eq!(rings.len(), 1);
        assert_eq!(
            rings
                .get_mut(&id)
                .unwrap()
                .lock()
                .push_slice(&[0.25, -0.25]),
            1
        );
        assert_eq!(rings[&id].lock().filled_frames(), 1);
    }
    #[test]
    fn clear_drops_samples_but_keeps_counters() {
        // R6/R7-Vertrag: clear() wirft nur gepufferte Frames weg,
        // die Ueber-/Unterlaufstatistik bleibt unangetastet.
        let mut r = PcmRing::new(1);
        r.push([1.0; 2]);
        r.push([2.0; 2]); // Voll -> aeltester weicht: overrun.
        assert_eq!(r.overruns(), 1);
        assert_eq!(r.pop(), [2.0; 2]);
        assert_eq!(r.pop(), [0.0; 2]); // Aktiv und leer: underrun.
        assert_eq!(r.underruns(), 1);
        r.clear();
        assert_eq!(r.filled_frames(), 0);
        assert_eq!(r.overruns(), 1);
        assert_eq!(r.underruns(), 1);
    }
    #[test]
    fn pop_counts_underrun_only_when_active() {
        // Aktiv aber leer: jede Anfrage ist ein echter Underrun.
        let mut aktiv = PcmRing::new(2);
        assert_eq!(aktiv.pop(), [0.0; 2]);
        assert_eq!(aktiv.pop(), [0.0; 2]);
        assert_eq!(aktiv.underruns(), 2);
        // Gegenstick: inaktiver Ring liefert Stille ohne Zaehler.
        let mut inaktiv = PcmRing::new(2);
        inaktiv.set_active(false);
        assert_eq!(inaktiv.pop(), [0.0; 2]);
        assert_eq!(inaktiv.underruns(), 0);
    }
    #[test]
    fn push_discards_frames_on_inactive_ring() {
        let mut r = PcmRing::new(2);
        r.set_active(false);
        r.push([9.0; 2]);
        assert_eq!(r.filled_frames(), 0);
        assert_eq!(r.overruns(), 0);
    }
    #[test]
    fn gain_ramp_progresses_monotonically_to_target() {
        let mut g = GainRamp::new(0.0);
        g.set(0.8, 480);
        let mut prev = 0.0_f32;
        for frame in 0..480 {
            let v = g.next_gain();
            assert!(
                v >= prev && v <= 0.8 + 1.0e-3,
                "Ramp muss aufsteigend zum Ziel laufen, Frame {frame}: {v}"
            );
            prev = v;
        }
        // Letzter Frame snappt exakt aufs Ziel, weitere Aufrufe halten es.
        assert_eq!(prev, 0.8);
        assert_eq!(g.next_gain(), 0.8);
    }
    #[test]
    fn gain_ramp_zero_frames_jumps_instantly() {
        let mut g = GainRamp::new(0.1);
        g.set(0.8, 0);
        // Fast-Pfad: kein Restlauf, sofortiges Ziel ab dem ersten Frame.
        assert_eq!(g.next_gain(), 0.8);
        assert_eq!(g.next_gain(), 0.8);
    }
    #[test]
    fn gain_ramp_clamps_out_of_range_targets() {
        let mut hoch = GainRamp::new(0.5);
        hoch.set(1.5, 0);
        assert_eq!(hoch.next_gain(), 1.0);
        let mut runter = GainRamp::new(0.5);
        runter.set(-0.3, 0);
        assert_eq!(runter.next_gain(), 0.0);
        // Auch der Rampenpfad zielt auf den geklemmten Zielwert.
        let mut rampe = GainRamp::new(0.0);
        rampe.set(2.0, 4);
        for _ in 0..4 {
            rampe.next_gain();
        }
        assert_eq!(rampe.next_gain(), 1.0);
    }
}
