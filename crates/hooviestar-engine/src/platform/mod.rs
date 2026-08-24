#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoBackend {
    Direct3D11,
    Vulkan,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureBackend {
    WindowsGraphicsCapture,
    DesktopDuplication,
    PipeWirePortal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioBackend {
    WasapiProcessLoopback,
    PipeWire,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformCapabilities {
    pub video: VideoBackend,
    pub window_capture: CaptureBackend,
    pub display_capture: CaptureBackend,
    pub audio: AudioBackend,
    pub embedded_preview: bool,
}

#[cfg(target_os = "windows")]
pub const CAPABILITIES: PlatformCapabilities = PlatformCapabilities {
    video: VideoBackend::Direct3D11,
    window_capture: CaptureBackend::WindowsGraphicsCapture,
    display_capture: CaptureBackend::DesktopDuplication,
    audio: AudioBackend::WasapiProcessLoopback,
    embedded_preview: true,
};
#[cfg(target_os = "linux")]
pub const CAPABILITIES: PlatformCapabilities = PlatformCapabilities {
    video: VideoBackend::Vulkan,
    window_capture: CaptureBackend::PipeWirePortal,
    display_capture: CaptureBackend::PipeWirePortal,
    audio: AudioBackend::PipeWire,
    embedded_preview: false,
};

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("capture permission denied by desktop portal")]
    PortalPermissionDenied,
    #[error("required runtime capability unavailable: {0}")]
    CapabilityUnavailable(&'static str),
    #[error("source binding is ambiguous")]
    AmbiguousBinding,
    #[error("source is offline")]
    Offline,
}
