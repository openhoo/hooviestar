use std::sync::Arc;

use hooviestar_engine::{
    NativeSurfaceKind, NativeSurfaces, SourceCandidate, SourceEnumeration,
    project::{DisplayBinding, WindowBinding},
    video::linux::{PipeWirePortalLink, PortalSelection},
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use tauri::{Manager, WebviewWindow, Window, WindowBuilder};

pub struct NativePreview {
    window: Window,
    pointer: usize,
}

impl NativePreview {
    pub fn create(
        studio: &WebviewWindow,
        program: &Window,
    ) -> Result<(Self, NativeSurfaces), String> {
        let preview = WindowBuilder::new(studio.app_handle(), "preview")
            .title("Hooviestar – Preview")
            .inner_size(960.0, 540.0)
            .visible(true)
            .build()
            .map_err(|error| error.to_string())?;
        let program_size = program.inner_size().map_err(|error| error.to_string())?;
        let preview_size = preview.inner_size().map_err(|error| error.to_string())?;
        let display = program
            .display_handle()
            .map_err(|error| error.to_string())?
            .as_raw();
        let studio_handle = studio
            .window_handle()
            .map_err(|error| error.to_string())?
            .as_raw();
        let program_handle = program
            .window_handle()
            .map_err(|error| error.to_string())?
            .as_raw();
        let preview_handle = preview
            .window_handle()
            .map_err(|error| error.to_string())?
            .as_raw();
        let (studio_pointer, program_pointer, preview_pointer, display_pointer, kind) =
            match (display, studio_handle, program_handle, preview_handle) {
                (
                    RawDisplayHandle::Xlib(display),
                    RawWindowHandle::Xlib(studio),
                    RawWindowHandle::Xlib(program),
                    RawWindowHandle::Xlib(preview),
                ) => (
                    usize::try_from(studio.window)
                        .map_err(|_| "Xlib studio handle exceeds pointer width".to_string())?,
                    usize::try_from(program.window)
                        .map_err(|_| "Xlib program handle exceeds pointer width".to_string())?,
                    usize::try_from(preview.window)
                        .map_err(|_| "Xlib preview handle exceeds pointer width".to_string())?,
                    display
                        .display
                        .map(|pointer| pointer.as_ptr() as usize)
                        .ok_or_else(|| "Xlib display handle is null".to_string())?,
                    NativeSurfaceKind::Xlib,
                ),
                (
                    RawDisplayHandle::Wayland(display),
                    RawWindowHandle::Wayland(studio),
                    RawWindowHandle::Wayland(program),
                    RawWindowHandle::Wayland(preview),
                ) => (
                    studio.surface.as_ptr() as usize,
                    program.surface.as_ptr() as usize,
                    preview.surface.as_ptr() as usize,
                    display.display.as_ptr() as usize,
                    NativeSurfaceKind::Wayland,
                ),
                _ => return Err("unsupported Linux native surface combination".into()),
            };
        Ok((
            Self {
                window: preview,
                pointer: preview_pointer,
            },
            NativeSurfaces {
                studio: studio_pointer,
                program: program_pointer,
                preview: preview_pointer,
                display: display_pointer,
                kind,
                program_width: program_size.width.max(1),
                program_height: program_size.height.max(1),
                preview_width: preview_size.width.max(1),
                preview_height: preview_size.height.max(1),
            },
        ))
    }

    pub fn native_handle(&self) -> usize {
        self.pointer
    }

    pub fn destroy(self) -> Result<(), String> {
        self.window.close().map_err(|error| error.to_string())
    }
}

pub async fn enumerate_sources(_surfaces: NativeSurfaces) -> Result<SourceEnumeration, String> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        hooviestar_engine::discovery::linux::enumerate_audio_nodes()
    })
    .await
    .map_err(|error| format!("enumerate_sources task failed: {error}"))?;
    let (candidates, message) = match result {
        Ok(candidates) => (
            candidates,
            Some("Fenster und Monitore werden über das Desktop-Portal ausgewählt.".into()),
        ),
        Err(error) => (
            Vec::new(),
            Some(format!(
                "PipeWire-Audio nicht verfügbar: {error}. Fenster und Monitore werden über das Desktop-Portal ausgewählt."
            )),
        ),
    };
    Ok(SourceEnumeration {
        candidates,
        portal_selection_required: true,
        message,
    })
}

/// Portal resources are session-scoped.  The link is also owned by the Linux
/// renderer, so selecting a source after engine startup immediately gives the
/// renderer the remote fd and stream metadata without persisting consent.
pub struct PortalResources {
    link: std::sync::Mutex<Option<Arc<PipeWirePortalLink>>>,
}

impl PortalResources {
    pub fn new() -> Self {
        Self {
            link: std::sync::Mutex::new(None),
        }
    }

    pub fn set_link(&self, link: Arc<PipeWirePortalLink>) {
        *self.link.lock().expect("portal mutex poisoned") = Some(link);
    }

    pub async fn select(&self) -> Result<SourceEnumeration, String> {
        let selection = PortalSelection::select(None, true)
            .await
            .map_err(|error| error.to_string())?;
        let candidates = portal_candidates(&selection);
        let link = self
            .link
            .lock()
            .expect("portal mutex poisoned")
            .clone()
            .ok_or_else(|| "Linux-Renderer ist noch nicht initialisiert".to_string())?;
        link.publish(selection);
        Ok(SourceEnumeration {
            candidates,
            portal_selection_required: false,
            message: None,
        })
    }

    pub fn clear(&self) {
        self.link.lock().expect("portal mutex poisoned").take();
    }
}

fn portal_candidates(selection: &PortalSelection) -> Vec<SourceCandidate> {
    let marker = format!("portal:{}", selection.binding_id);
    selection
        .streams
        .iter()
        .enumerate()
        .map(|(index, stream)| {
            let node = stream.pipewire_node_id;
            let name = stream
                .id
                .clone()
                .or_else(|| stream.mapping_id.clone())
                .unwrap_or_else(|| format!("Portal-Quelle {}", index + 1));
            if stream.is_window() {
                SourceCandidate::Window {
                    runtime_id: format!("portal:window:{node}"),
                    name,
                    // The portal does not expose a stable process path.  The
                    // marker plus node id lets the renderer associate the
                    // candidate with this session; a restart requires a new
                    // portal selection instead of a silent rebind.
                    binding: WindowBinding {
                        process_path: marker.clone(),
                        window_title: node.to_string(),
                    },
                }
            } else {
                SourceCandidate::Display {
                    runtime_id: format!("portal:display:{node}"),
                    name,
                    binding: DisplayBinding {
                        adapter_luid: marker.clone(),
                        output_id: node,
                    },
                }
            }
        })
        .collect()
}

pub fn set_preview_bounds(
    _pointer: usize,
    _x: i32,
    _y: i32,
    _width: i32,
    _height: i32,
) -> Result<(), String> {
    Err("Linux verwendet ein separates natives Preview-Fenster".into())
}
