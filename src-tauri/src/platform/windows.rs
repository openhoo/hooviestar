use hooviestar_engine::{NativeSurfaceKind, NativeSurfaces, SourceEnumeration};
use tauri::{WebviewWindow, Window};
use windows::{
    Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_TOP, SWP_NOACTIVATE, SetWindowPos,
            WINDOW_EX_STYLE, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
        },
    },
    core::w,
};

pub struct NativePreview {
    hwnd: usize,
}

impl NativePreview {
    pub fn create(
        studio: &WebviewWindow,
        program: &Window,
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

    pub fn hwnd(&self) -> usize {
        self.hwnd
    }

    pub fn destroy(self) -> Result<(), String> {
        unsafe { DestroyWindow(HWND(self.hwnd as *mut _)) }.map_err(|error| error.to_string())
    }
}

pub fn enumerate_sources(surfaces: NativeSurfaces) -> Result<SourceEnumeration, String> {
    let excluded = [surfaces.studio, surfaces.program, surfaces.preview];
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
        return Err("preview bounds must be positive".into());
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
