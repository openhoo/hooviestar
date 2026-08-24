//! CPU-side cache for static image sources.
//! Images are decoded once to RGBA8; Vulkan upload is performed by the renderer.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Result, anyhow};
use image::ImageReader;

#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
    pub fingerprint: (u128, u64),
}

#[derive(Default)]
pub struct ImageCache {
    entries: HashMap<PathBuf, DecodedImage>,
}

impl ImageCache {
    pub fn get_or_decode(&mut self, path: &str) -> Result<&DecodedImage> {
        let canonical = fs::canonicalize(path).map_err(|e| anyhow!("image path {}: {e}", path))?;
        let metadata = fs::metadata(&canonical)
            .map_err(|e| anyhow!("image metadata {}: {e}", canonical.display()))?;
        let modified = metadata
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let fingerprint = (modified, metadata.len());
        let rebuild = self
            .entries
            .get(&canonical)
            .map(|image| image.fingerprint != fingerprint)
            .unwrap_or(true);
        if rebuild {
            let decoded = decode(&canonical, fingerprint)?;
            self.entries.insert(canonical.clone(), decoded);
        }
        self.entries
            .get(&canonical)
            .ok_or_else(|| anyhow!("image cache insert failed"))
    }

    pub fn remove(&mut self, path: &Path) {
        self.entries.remove(path);
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn retain(&mut self, keep: impl Fn(&PathBuf) -> bool) {
        self.entries.retain(|path, _| keep(path));
    }
}

fn decode(path: &Path, fingerprint: (u128, u64)) -> Result<DecodedImage> {
    let image = ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?
        .to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(anyhow!("image has zero dimensions"));
    }
    Ok(DecodedImage {
        path: path.to_path_buf(),
        width,
        height,
        rgba8: image.into_raw(),
        fingerprint,
    })
}
