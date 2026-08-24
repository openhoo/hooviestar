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
}

impl Default for MediaControl {
    fn default() -> Self {
        Self {
            playing: true,
            seek_seconds: None,
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
    pub fn depth(&self) -> usize {
        usize::from(self.slot.lock().is_some())
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
    fn latest_depth_never_exceeds_one() {
        let slot = LatestFrame::default();
        let id = Uuid::new_v4();
        for sequence in 0..10 {
            slot.publish(Arc::new(GpuFrame {
                source_id: id,
                sequence,
                timestamp_ns: 0,
                native_texture: 1,
            }));
            assert_eq!(slot.depth(), 1)
        }
        assert_eq!(slot.take().unwrap().sequence, 9);
        assert_eq!(slot.depth(), 0)
    }
}
