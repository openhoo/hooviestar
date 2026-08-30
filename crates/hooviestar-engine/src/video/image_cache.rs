//! CPU-side cache for static image sources.
//! Images are decoded once to RGBA8; Vulkan upload is performed by the renderer.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Result, anyhow};
use image::{GenericImageView, ImageReader};

#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
    pub fingerprint: (u128, u64),
}

/// Failed decodes are retried at most once per cooldown window: a broken or
/// truncated image must not stall the render thread with per-frame decode
/// attempts. The record is keyed by fingerprint, so a changed file retries
/// immediately.
const DECODE_RETRY_COOLDOWN: Duration = Duration::from_secs(5);
/// Bound both one decoded RGBA image and the retained CPU cache. Vulkan keeps
/// its own aggregate budget for uploaded static textures.
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGE_CACHE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Default)]
pub struct ImageCache {
    entries: HashMap<PathBuf, DecodedImage>,
    failures: HashMap<PathBuf, ((u128, u64), Instant)>,
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
            let cooling_down = self
                .failures
                .get(&canonical)
                .is_some_and(|(failed, attempted)| {
                    *failed == fingerprint && attempted.elapsed() < DECODE_RETRY_COOLDOWN
                });
            if cooling_down {
                return Err(anyhow!(
                    "image decode recently failed, retry after cooldown: {}",
                    canonical.display()
                ));
            }
            self.failures.remove(&canonical);
            match decode(&canonical, fingerprint) {
                Ok(decoded) => {
                    self.insert_bounded(canonical.clone(), decoded)?;
                }
                Err(error) => {
                    self.failures
                        .insert(canonical.clone(), (fingerprint, Instant::now()));
                    return Err(error);
                }
            };
        }
        self.entries
            .get(&canonical)
            .ok_or_else(|| anyhow!("image cache insert failed"))
    }

    pub fn remove(&mut self, path: &Path) {
        self.entries.remove(path);
        self.failures.remove(path);
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.failures.clear();
    }
    pub fn retain(&mut self, keep: impl Fn(&PathBuf) -> bool) {
        self.entries.retain(|path, _| keep(path));
        self.failures.retain(|path, _| keep(path));
    }

    fn insert_bounded(&mut self, path: PathBuf, decoded: DecodedImage) -> Result<()> {
        let incoming = decoded.rgba8.len();
        if incoming > MAX_IMAGE_BYTES {
            return Err(anyhow!("decoded image exceeds 64 MiB"));
        }
        let replaced = self.entries.get(&path).map_or(0, |entry| entry.rgba8.len());
        let mut used = self
            .entries
            .values()
            .map(|entry| entry.rgba8.len())
            .sum::<usize>()
            .saturating_sub(replaced);
        while used.saturating_add(incoming) > MAX_IMAGE_CACHE_BYTES {
            let Some(evicted) = self.entries.keys().find(|key| **key != path).cloned() else {
                return Err(anyhow!("image cache budget exhausted"));
            };
            if let Some(entry) = self.entries.remove(&evicted) {
                used = used.saturating_sub(entry.rgba8.len());
                self.failures.remove(&evicted);
            }
        }
        self.entries.insert(path, decoded);
        Ok(())
    }
}

fn decode(path: &Path, fingerprint: (u128, u64)) -> Result<DecodedImage> {
    // Strict dimension limits: oversized images must fail decode and flow
    // into the regular failure/cooldown path above instead of pinning
    // gigabytes of RGBA8 and stalling the render thread. The default
    // allocation limit stays in force.
    // Limits is #[non_exhaustive]: field assignment, not a struct literal.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(MAX_IMAGE_BYTES as u64);
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    reader.limits(limits);
    let image = reader.decode()?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(anyhow!("image has zero dimensions"));
    }
    let expected_rgba_bytes = rgba_byte_len(width, height)?;
    let image = image.to_rgba8();
    debug_assert_eq!(image.len(), expected_rgba_bytes);
    Ok(DecodedImage {
        path: path.to_path_buf(),
        width,
        height,
        rgba8: image.into_raw(),
        fingerprint,
    })
}

fn rgba_byte_len(width: u32, height: u32) -> Result<usize> {
    let rgba_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_IMAGE_BYTES as u64)
        .ok_or_else(|| anyhow!("decoded image {width}x{height} exceeds 64 MiB limit"))?;
    usize::try_from(rgba_bytes)
        .map_err(|_| anyhow!("decoded image size does not fit this platform"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_size_limit_rejects_oversized_decodes_before_conversion() {
        assert_eq!(rgba_byte_len(1, 1).unwrap(), 4);
        assert_eq!(rgba_byte_len(4096, 4096).unwrap(), MAX_IMAGE_BYTES);
        assert!(rgba_byte_len(4097, 4096).is_err());
        assert!(rgba_byte_len(u32::MAX, u32::MAX).is_err());
    }
}
