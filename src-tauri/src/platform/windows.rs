use hooviestar_engine::{NativeSurfaceKind, NativeSurfaces, SourceEnumeration};
use tauri::{WebviewWindow, Window};
use windows::{
    Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GetSystemMetrics, HWND_BOTTOM, HWND_TOP,
            SM_CXVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE,
            SWP_SHOWWINDOW, SetWindowPos, WINDOW_EX_STYLE, WS_CHILD, WS_CLIPCHILDREN,
            WS_CLIPSIBLINGS, WS_VISIBLE,
        },
    },
    core::w,
};

pub struct OutputVisibility;

pub fn configure_graphics_backend() {}

impl OutputVisibility {
    pub fn prepare() -> Result<Self, String> {
        Ok(Self)
    }

    pub fn show_program(&self, window: &Window) -> Result<(), String> {
        let hwnd = window.hwnd().map_err(|error| error.to_string())?;
        let size = window.outer_size().map_err(|error| error.to_string())?;
        let virtual_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let virtual_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let virtual_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        if virtual_width <= 0 {
            return Err("Windows meldet keinen virtuellen Desktop".into());
        }
        let width = i32::try_from(size.width).map_err(|_| "Programmbreite ist zu groß")?;
        let height = i32::try_from(size.height).map_err(|_| "Programmhöhe ist zu groß")?;
        let (x, y) = offscreen_program_position(virtual_left, virtual_top, width);
        // Sichtbar und nicht minimiert lassen: Nur so bietet Discord das
        // Fenster als App an und Windows Graphics Capture liefert weiter
        // Frames. HWND_BOTTOM plus Offscreen-Position verhindert Aktivierung
        // und sichtbares Aufblitzen.
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_BOTTOM),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        }
        .map_err(|error| {
            format!("Program-Ausgabe konnte nicht offscreen platziert werden: {error}")
        })
    }

    pub fn initially_visible(&self) -> bool {
        false
    }

    pub fn cleanup(self) {}
}

/// Places the mapped Program window wholly left of every virtual-desktop
/// monitor. Saturating arithmetic keeps exotic multi-monitor coordinates from
/// wrapping back onto a visible display.
fn offscreen_program_position(virtual_left: i32, virtual_top: i32, width: i32) -> (i32, i32) {
    (
        virtual_left.saturating_sub(width).saturating_sub(128),
        virtual_top,
    )
}

pub struct NativePreview {
    hwnd: usize,
}

impl NativePreview {
    pub fn create(
        studio: &WebviewWindow,
        program: &Window,
        _output_visibility: &OutputVisibility,
    ) -> Result<(Self, NativeSurfaces), String> {
        let studio_hwnd = studio.hwnd().map_err(|error| error.to_string())?;
        let program_hwnd = program.hwnd().map_err(|error| error.to_string())?;
        let program_size = program.inner_size().map_err(|error| error.to_string())?;
        let preview_hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Hooviestar Preview"),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                16,
                9,
                Some(studio_hwnd),
                None,
                None,
                None,
            )
        }
        .map_err(|error| error.to_string())?;
        let preview = Self {
            hwnd: preview_hwnd.0 as usize,
        };
        Ok((
            preview,
            NativeSurfaces {
                studio: studio_hwnd.0 as usize,
                program: program_hwnd.0 as usize,
                preview: preview_hwnd.0 as usize,
                display: 0,
                kind: NativeSurfaceKind::Win32,
                program_width: program_size.width.max(1),
                program_height: program_size.height.max(1),
                preview_width: 16,
                preview_height: 9,
            },
        ))
    }

    pub fn native_handle(&self) -> usize {
        self.hwnd
    }

    pub fn destroy(self) -> Result<(), String> {
        unsafe { DestroyWindow(HWND(self.hwnd as *mut _)) }.map_err(|error| error.to_string())
    }
}

pub async fn enumerate_sources(surfaces: NativeSurfaces) -> Result<SourceEnumeration, String> {
    let excluded = [surfaces.studio, surfaces.program, surfaces.preview];
    let (candidates, message) = tauri::async_runtime::spawn_blocking(move || {
        let mut candidates =
            hooviestar_engine::discovery::windows::enumerate_visible_windows(&excluded)?;
        candidates.extend(hooviestar_engine::discovery::windows::enumerate_displays()?);
        let message = match hooviestar_engine::discovery::windows::enumerate_audio_sessions() {
            Ok(audio) => {
                candidates.extend(audio);
                None
            }
            Err(error) => Some(format!("Anwendungs-Audio nicht verfügbar: {error}")),
        };
        Ok::<_, String>((candidates, message))
    })
    .await
    .map_err(|error| format!("Quellenauflösung fehlgeschlagen: {error}"))??;
    Ok(SourceEnumeration {
        candidates,
        portal_selection_required: false,
        message,
    })
}

pub struct PortalResources;

impl PortalResources {
    pub fn new() -> Self {
        Self
    }

    pub async fn select(&self) -> Result<SourceEnumeration, String> {
        Err("Desktop-Portal-Auswahl ist nur unter Linux verfügbar".into())
    }

    pub fn clear(&self) {}
}

pub fn set_preview_bounds(
    hwnd: usize,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<(), String> {
    if width <= 0 || height <= 0 {
        return Err("Vorschauabmessungen müssen positiv sein".into());
    }
    unsafe {
        SetWindowPos(
            HWND(hwnd as *mut _),
            Some(HWND_TOP),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE,
        )
    }
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::offscreen_program_position;

    #[test]
    fn program_is_placed_left_of_single_monitor_desktop() {
        assert_eq!(offscreen_program_position(0, 0, 1280), (-1408, 0));
    }

    #[test]
    fn program_is_placed_left_of_negative_origin_multi_monitor_desktop() {
        assert_eq!(offscreen_program_position(-1920, -120, 1920), (-3968, -120));
    }

    #[test]
    fn extreme_virtual_desktop_coordinates_never_wrap_visible() {
        assert_eq!(
            offscreen_program_position(i32::MIN + 64, 42, 1920),
            (i32::MIN, 42)
        );
    }
}
