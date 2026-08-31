use std::{os::unix::process::CommandExt, process::Command, sync::Arc};

use hooviestar_engine::{
    NativeSurfaceKind, NativeSurfaces, SourceCandidate, SourceEnumeration,
    project::{DisplayBinding, WindowBinding},
    video::linux::{PipeWirePortalLink, PortalSelection},
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use tauri::{Manager, PhysicalPosition, WebviewWindow, Window, WindowBuilder};

const PROGRAM_TITLE: &str = "Hooviestar – Program";
const PREVIEW_TITLE: &str = "Hooviestar – Preview";
const HYPRLAND_PROGRAM_RULE: &str = "__hooviestar_program_rule";
const HYPRLAND_PREVIEW_RULE: &str = "__hooviestar_preview_rule";
const GRAPHICS_ENV_READY: &str = "HOOVIESTAR_GRAPHICS_ENV_READY";

/// GTK and the Vulkan renderer must not both own commits to the same native
/// Wayland surfaces. Hyprland already provides XWayland and exports XWayland
/// toplevels to its screen-cast portal, so use that stable presentation path.
/// This runs before Tauri/GTK starts any threads.
pub fn configure_graphics_backend() {
    if is_hyprland() {
        let needs_x11 =
            std::env::var_os("GDK_BACKEND").as_deref() != Some(std::ffi::OsStr::new("x11"));
        let needs_webkit_fallback = std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none();
        if (!needs_x11 && !needs_webkit_fallback) || std::env::var_os(GRAPHICS_ENV_READY).is_some()
        {
            return;
        }

        // WebKitGTK reads its DMA-BUF switch while shared libraries load,
        // before Rust's `main`. Re-exec once so both GTK and WebKit see their
        // environment from process start. The software import path keeps the
        // Studio visible; Vulkan still renders Program and Preview.
        let executable = std::env::current_exe()
            .unwrap_or_else(|error| panic!("Hooviestar executable could not be resolved: {error}"));
        let mut command = Command::new(executable);
        command.args(std::env::args_os().skip(1));
        command.env(GRAPHICS_ENV_READY, "1");
        if needs_x11 {
            command.env("GDK_BACKEND", "x11");
        }
        if needs_webkit_fallback {
            command.env("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        let error = command.exec();
        panic!("Hooviestar could not restart with its graphics environment: {error}");
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisibilityMode {
    HyprlandSpecialWorkspace,
    OffscreenX11,
}

/// Haelt die nativen Renderfenster fuer Discord gemappt, ohne sie auf einem
/// physischen Desktop anzuzeigen.
///
/// Hyprland exportiert auch Fenster eines inaktiven Special Workspace ueber
/// sein Toplevel-Capture-Protokoll. `render_unfocused` verhindert, dass der
/// Stream dort stehenbleibt. Unter klassischem X11 bleiben die Fenster
/// stattdessen gemappt und werden ausserhalb des virtuellen Desktops gesetzt.
pub struct OutputVisibility {
    mode: VisibilityMode,
    cleaned: bool,
}

impl OutputVisibility {
    pub fn prepare() -> Result<Self, String> {
        if is_hyprland() {
            run_hyprctl_eval(&hyprland_rule_source())?;
            return Ok(Self {
                mode: VisibilityMode::HyprlandSpecialWorkspace,
                cleaned: false,
            });
        }
        Ok(Self {
            mode: VisibilityMode::OffscreenX11,
            cleaned: false,
        })
    }

    pub fn show_program(&self, window: &Window) -> Result<(), String> {
        self.show(window, PROGRAM_TITLE)
    }

    pub fn initially_visible(&self) -> bool {
        self.mode == VisibilityMode::HyprlandSpecialWorkspace
    }

    fn show_preview(&self, window: &Window) -> Result<(), String> {
        self.show(window, PREVIEW_TITLE)
    }

    fn show(&self, window: &Window, title: &str) -> Result<(), String> {
        match self.mode {
            // Das Fenster wurde sichtbar gebaut: Nur so ist der native
            // Wayland-Surface-Handle bereits im synchronen Tauri-Setup
            // verfügbar. Die zuvor installierte Hyprland-Regel greift beim
            // Mapping, bevor ein physischer Workspace das Fenster zeichnet.
            VisibilityMode::HyprlandSpecialWorkspace => Ok(()),
            VisibilityMode::OffscreenX11 => {
                let raw = window
                    .window_handle()
                    .map_err(|error| error.to_string())?
                    .as_raw();
                if !matches!(raw, RawWindowHandle::Xlib(_)) {
                    return Err(
                        "Unsichtbare Discord-App-Ausgabe wird unter Wayland derzeit nur mit Hyprland unterstützt"
                            .into(),
                    );
                }
                window
                    .set_position(PhysicalPosition::new(-32_768, -32_768))
                    .map_err(|error| {
                        format!("{title} konnte nicht offscreen platziert werden: {error}")
                    })?;
                window
                    .show()
                    .map_err(|error| format!("{title} konnte nicht gestartet werden: {error}"))
            }
        }
    }

    pub fn cleanup(mut self) {
        self.cleanup_inner();
    }

    fn cleanup_inner(&mut self) {
        if !self.cleaned
            && self.mode == VisibilityMode::HyprlandSpecialWorkspace
            && let Err(error) = run_hyprctl_eval(&hyprland_cleanup_source())
        {
            eprintln!("[hooviestar] Hyprland output rule cleanup failed: {error}");
        }
        self.cleaned = true;
    }
}

impl Drop for OutputVisibility {
    fn drop(&mut self) {
        self.cleanup_inner();
    }
}

fn is_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        && std::env::var("XDG_CURRENT_DESKTOP")
            .ok()
            .is_some_and(|desktop| {
                desktop
                    .split([':', ';'])
                    .any(|part| part.eq_ignore_ascii_case("hyprland"))
            })
}

fn run_hyprctl_eval(source: &str) -> Result<(), String> {
    let output = Command::new("hyprctl")
        .args(["eval", source])
        .output()
        .map_err(|error| format!("hyprctl konnte nicht gestartet werden: {error}"))?;
    if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "ok" {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    Err(format!(
        "Hyprland konnte die virtuelle Discord-Ausgabe nicht einrichten: {detail}"
    ))
}

fn hyprland_rule_source() -> String {
    format!(
        r#"
if _G.{PROGRAM_RULE} ~= nil then _G.{PROGRAM_RULE}:set_enabled(false) end
if _G.{PREVIEW_RULE} ~= nil then _G.{PREVIEW_RULE}:set_enabled(false) end
_G.{PROGRAM_RULE} = hl.window_rule({{
  name = "hooviestar-program-output",
  match = {{ title = "^{PROGRAM_TITLE}$" }},
  workspace = "special:hooviestar-output silent",
  float = true,
  size = "1280 720",
  no_initial_focus = true,
  no_anim = true,
  render_unfocused = true
}})
_G.{PREVIEW_RULE} = hl.window_rule({{
  name = "hooviestar-native-preview",
  match = {{ title = "^{PREVIEW_TITLE}$" }},
  workspace = "special:hooviestar-output silent",
  float = true,
  size = "960 540",
  no_initial_focus = true,
  no_anim = true,
  render_unfocused = true
}})
"#,
        PROGRAM_RULE = HYPRLAND_PROGRAM_RULE,
        PREVIEW_RULE = HYPRLAND_PREVIEW_RULE,
    )
}

fn hyprland_cleanup_source() -> String {
    format!(
        r#"
if _G.{PROGRAM_RULE} ~= nil then
  _G.{PROGRAM_RULE}:set_enabled(false)
  _G.{PROGRAM_RULE} = nil
end
if _G.{PREVIEW_RULE} ~= nil then
  _G.{PREVIEW_RULE}:set_enabled(false)
  _G.{PREVIEW_RULE} = nil
end
"#,
        PROGRAM_RULE = HYPRLAND_PROGRAM_RULE,
        PREVIEW_RULE = HYPRLAND_PREVIEW_RULE,
    )
}

pub struct NativePreview {
    window: Window,
    pointer: usize,
}

impl NativePreview {
    pub fn create(
        studio: &WebviewWindow,
        program: &Window,
        output_visibility: &OutputVisibility,
    ) -> Result<(Self, NativeSurfaces), String> {
        let preview = WindowBuilder::new(studio.app_handle(), "preview")
            .title(PREVIEW_TITLE)
            .inner_size(960.0, 540.0)
            .decorations(false)
            .focused(false)
            .focusable(false)
            .visible(output_visibility.initially_visible())
            .build()
            .map_err(|error| error.to_string())?;
        output_visibility.show_preview(&preview)?;
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

pub fn set_preview_visible(_pointer: usize, _visible: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{hyprland_cleanup_source, hyprland_rule_source};

    #[test]
    fn hyprland_rules_keep_output_mapped_but_off_the_physical_desktop() {
        let source = hyprland_rule_source();
        assert!(source.contains("workspace = \"special:hooviestar-output silent\""));
        assert!(source.contains("title = \"^Hooviestar – Program$\""));
        assert!(source.contains("render_unfocused = true"));
        assert!(source.contains("no_initial_focus = true"));
    }

    #[test]
    fn hyprland_cleanup_disables_both_runtime_rules() {
        let source = hyprland_cleanup_source();
        assert_eq!(source.matches(":set_enabled(false)").count(), 2);
        assert!(source.contains("_G.__hooviestar_program_rule = nil"));
        assert!(source.contains("_G.__hooviestar_preview_rule = nil"));
    }
}
