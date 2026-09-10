use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewOverlaySelection {
    pub transform: PreviewTransform,
    /// Consumed by the Windows native HUD; retained on Linux for the shared IPC shape.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub locked: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewTransform {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub rotation_degrees: f64,
    pub crop_top: f64,
    pub crop_right: f64,
    pub crop_bottom: f64,
    pub crop_left: f64,
    pub opacity: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewOverlayPayload {
    /// Consumed by the Windows native HUD; retained on Linux for the shared IPC shape.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub visible: bool,
    pub output_width: f64,
    pub output_height: f64,
    pub selection: Option<PreviewOverlaySelection>,
}

impl PreviewOverlayPayload {
    pub fn validate(&self) -> Result<(), String> {
        if !self.output_width.is_finite()
            || !self.output_height.is_finite()
            || self.output_width <= 0.0
            || self.output_height <= 0.0
        {
            return Err("Preview-Ausgabeabmessungen müssen endlich und positiv sein".into());
        }
        if let Some(selection) = &self.selection {
            let transform = &selection.transform;
            if !transform.x.is_finite()
                || !transform.y.is_finite()
                || !transform.width.is_finite()
                || !transform.height.is_finite()
                || !transform.rotation_degrees.is_finite()
                || !transform.crop_top.is_finite()
                || !transform.crop_right.is_finite()
                || !transform.crop_bottom.is_finite()
                || !transform.crop_left.is_finite()
                || !transform.opacity.is_finite()
                || transform.width <= 0.0
                || transform.height <= 0.0
                || transform.crop_top < 0.0
                || transform.crop_right < 0.0
                || transform.crop_bottom < 0.0
                || transform.crop_left < 0.0
                || transform.crop_top + transform.crop_bottom >= transform.height
                || transform.crop_left + transform.crop_right >= transform.width
            {
                return Err("Ungültige Geometrie der Preview-Auswahl".into());
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "windows")]
pub use windows::*;
