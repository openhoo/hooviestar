#[cfg(target_os = "linux")]
pub mod image_cache;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod linux_media;
#[cfg(target_os = "linux")]
pub mod text_raster;
#[cfg(target_os = "linux")]
pub mod vulkan;
#[cfg(target_os = "windows")]
pub mod windows;

use crate::project::Transform;
use parking_lot::RwLock;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct MediaControl {
    pub playing: bool,
    pub seek_seconds: Option<f64>,
    pub epoch: u64,
}

impl Default for MediaControl {
    fn default() -> Self {
        Self {
            playing: true,
            seek_seconds: None,
            epoch: 0,
        }
    }
}

pub type MediaControlBus = Arc<RwLock<HashMap<Uuid, MediaControl>>>;

pub fn media_control_bus() -> MediaControlBus {
    Arc::new(RwLock::new(HashMap::new()))
}
use parking_lot::Mutex;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct GpuFrame {
    pub source_id: Uuid,
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub native_texture: usize,
}

#[derive(Default)]
pub struct LatestFrame {
    slot: Mutex<Option<Arc<GpuFrame>>>,
}
impl LatestFrame {
    pub fn publish(&self, frame: Arc<GpuFrame>) -> Option<Arc<GpuFrame>> {
        self.slot.lock().replace(frame)
    }
    pub fn take(&self) -> Option<Arc<GpuFrame>> {
        self.slot.lock().take()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemVertices {
    pub positions: [[f32; 2]; 4],
    pub opacity: f32,
}
pub fn item_vertices(transform: Transform) -> ItemVertices {
    let radians = transform.rotation_degrees.to_radians();
    let (s, c) = radians.sin_cos();
    let cx = transform.x + transform.width * 0.5;
    let cy = transform.y + transform.height * 0.5;
    let mut positions = [[0.0; 2]; 4];
    for (out, (x, y)) in positions.iter_mut().zip([
        (transform.x, transform.y),
        (transform.x + transform.width, transform.y),
        (
            transform.x + transform.width,
            transform.y + transform.height,
        ),
        (transform.x, transform.y + transform.height),
    ]) {
        let dx = x - cx;
        let dy = y - cy;
        *out = [cx + dx * c - dy * s, cy + dx * s + dy * c]
    }
    ItemVertices {
        positions,
        opacity: transform.opacity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_slot_holds_single_frame() {
        let slot = LatestFrame::default();
        let id = Uuid::new_v4();
        for sequence in 0..10 {
            let previous = slot.publish(Arc::new(GpuFrame {
                source_id: id,
                sequence,
                timestamp_ns: 0,
                native_texture: 1,
            }));
            assert_eq!(previous.is_some(), sequence > 0);
        }
        assert_eq!(slot.take().unwrap().sequence, 9);
        assert!(slot.take().is_none());
    }
    #[test]
    fn default_starts_playing_without_seek() {
        let control = MediaControl::default();
        assert!(control.playing);
        assert!(control.seek_seconds.is_none());
        assert_eq!(control.epoch, 0);
    }

    // Ohne Rotation an der Ursprung: Ecken entsprechen exakt der Rechteckgeometrie (TL, TR, BR, BL).
    #[test]
    fn vertices_identity_corners_exact() {
        let t = Transform {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
            ..Transform::default()
        };
        let v = item_vertices(t);
        assert_eq!(
            v.positions,
            [[0.0, 0.0], [100.0, 0.0], [100.0, 50.0], [0.0, 50.0]]
        );
        assert_eq!(v.opacity, t.opacity);
    }

    // 90-Grad-Drehung um den Mittelpunkt: Bounding tauscht Breite und Hoehe, Ecken drehen vorhersehbar.
    #[test]
    fn vertices_quarter_rotation_about_center() {
        let t = Transform {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
            rotation_degrees: 90.0,
            ..Transform::default()
        };
        let v = item_vertices(t);
        let expected = [[75.0, -25.0], [75.0, 75.0], [25.0, 75.0], [25.0, -25.0]];
        for (actual, exp) in v.positions.iter().zip(expected.iter()) {
            assert!((actual[0] - exp[0]).abs() < 1e-4);
            assert!((actual[1] - exp[1]).abs() < 1e-4);
        }
    }

    // Negative und grosse Koordinaten: Nur der Mittelpunkt verschiebt sich; die Zentrier-Mathematik
    // bleibt konsistent (Eckversatz relativ zum eigenen Mittelpunkt ist unveraendert).
    #[test]
    fn vertices_translation_invariant_for_negative_and_large_coords() {
        let base = Transform {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
            rotation_degrees: 30.0,
            ..Transform::default()
        };
        let shifted = Transform {
            x: -16384.0,
            y: 32768.0,
            ..base
        };
        let vb = item_vertices(base).positions;
        let vs = item_vertices(shifted).positions;
        // Versatz exakt als Mittelpunktdelta erwartet; f32-Rundungsfehler bleibt weit unter 1e-2.
        for i in 0..4 {
            assert!((vs[i][0] - (vb[i][0] - 16384.0)).abs() < 1e-2);
            assert!((vs[i][1] - (vb[i][1] + 32768.0)).abs() < 1e-2);
        }
    }
    /// Rueckschreib-Protokoll der Render-Threads: Eine Bus-Rueckschreibung
    /// mit aelterer Epoche darf ein unter neuer Epoche gesetztes
    /// playing=true nicht ueberschreiben; nur bei passender Epoche greift
    /// das Guard-Muster `if entry.epoch == control.epoch { .. }`.
    #[test]
    fn stale_media_writeback_cannot_overwrite_newer_epoch() {
        let bus = media_control_bus();
        let id = Uuid::new_v4();
        // Render-Thread sichert den Stand VOR dem User-Eingriff.
        let stale_control = *bus.write().entry(id).or_default();
        assert!(stale_control.playing);
        // User-Play wie command(): Epoche erhoehen, dann playing setzen.
        {
            let mut guard = bus.write();
            let entry = guard.get_mut(&id).unwrap();
            entry.epoch = entry.epoch.wrapping_add(1);
            entry.playing = true;
        }
        // Veraltete Rueckschreibung nach fehlgeschlagenem Restart-Seek:
        // Epoche passt nicht -> der Play-Wunsch bleibt erhalten.
        if bus.read().get(&id).unwrap().epoch == stale_control.epoch {
            bus.write().get_mut(&id).unwrap().playing = false;
        }
        assert!(bus.read().get(&id).unwrap().playing);
        // Frischer Snapshot nach dem Play: passende Epoche -> Rueckschreibung greift.
        let fresh_control = *bus.read().get(&id).unwrap();
        assert_ne!(fresh_control.epoch, stale_control.epoch);
        if bus.read().get(&id).unwrap().epoch == fresh_control.epoch {
            bus.write().get_mut(&id).unwrap().playing = false;
        }
        assert!(!bus.read().get(&id).unwrap().playing);
    }
}
