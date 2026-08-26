//! Linux desktop-portal capture and PipeWire DMA-BUF ownership.
//!
//! The portal session is deliberately kept in this module instead of being
//! persisted.  A portal node id is meaningful only for the current portal
//! session; the renderer therefore reports restored sources as requiring a
//! fresh selection until this link receives a new selection.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    io::{Read as _, Write as _},
    os::fd::{BorrowedFd, FromRawFd, IntoRawFd, OwnedFd},
    os::unix::net::UnixStream,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use ashpd::{
    WindowIdentifier,
    desktop::{
        PersistMode, Session,
        screencast::{
            CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
            StartCastOptions,
        },
    },
};
use pipewire as pw;
use pw::properties::properties;
use pw::spa::support::system::IoFlags;
use uuid::Uuid;

/// DRM_FORMAT_MOD_INVALID as defined by drm_fourcc.h.  A range containing this
/// value asks PipeWire to fixate the modifier when the producer can export one.
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// A stream returned by the portal.  `id` is the portal stream id (when the
/// compositor provides one), while `mapping_id` identifies the selected
/// window/monitor across the response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortalStreamInfo {
    pub pipewire_node_id: u32,
    pub source_type: Option<SourceType>,
    pub id: Option<String>,
    pub mapping_id: Option<String>,
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

impl PortalStreamInfo {
    pub fn is_window(&self) -> bool {
        matches!(self.source_type, Some(SourceType::Window))
    }
}

/// A live, non-persisted portal selection. Keeping the proxy and session in
/// this value is required: dropping either invalidates the remote connection.
pub struct PortalSelection {
    pub binding_id: Uuid,
    pub proxy: Screencast,
    pub session: Session<Screencast>,
    pub remote: Option<OwnedFd>,
    pub streams: Vec<PortalStreamInfo>,
}

impl PortalSelection {
    pub async fn select(
        identifier: Option<&WindowIdentifier>,
        multiple: bool,
    ) -> Result<Self, LinuxVideoError> {
        let proxy = Screencast::new()
            .await
            .map_err(|error| LinuxVideoError::Portal(error.to_string()))?;
        let session = proxy
            .create_session(Default::default())
            .await
            .map_err(|error| LinuxVideoError::Portal(error.to_string()))?;
        proxy
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(CursorMode::Embedded)
                    .set_sources(SourceType::Monitor | SourceType::Window)
                    .set_multiple(multiple)
                    // Never persist consent or a portal restore token.
                    .set_persist_mode(PersistMode::DoNot),
            )
            .await
            .map_err(|error| LinuxVideoError::Portal(error.to_string()))?;
        let response = proxy
            .start(&session, identifier, StartCastOptions::default())
            .await
            .map_err(|error| LinuxVideoError::Portal(error.to_string()))?
            .response()
            .map_err(|error| LinuxVideoError::Portal(error.to_string()))?;
        let streams = response
            .streams()
            .iter()
            .map(|stream| PortalStreamInfo {
                pipewire_node_id: stream.pipe_wire_node_id(),
                source_type: stream.source_type(),
                id: stream.id().map(ToOwned::to_owned),
                mapping_id: stream.mapping_id().map(ToOwned::to_owned),
                position: stream.position(),
                size: stream.size(),
            })
            .collect();
        let remote = proxy
            .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
            .await
            .map_err(|error| LinuxVideoError::Portal(error.to_string()))?;
        Ok(Self {
            binding_id: Uuid::new_v4(),
            proxy,
            session,
            remote: Some(remote),
            streams,
        })
    }
}

/// The tauri side publishes one live selection; the render side consumes the
/// remote fd once to establish one PipeWire core and observes stream metadata
/// on every generation.  Replacing a selection drops the old fd and thereby
/// closes the old portal transport without persisting user consent.
pub struct PipeWirePortalLink {
    selection: Mutex<Option<PortalSelection>>,
    generation: std::sync::atomic::AtomicU64,
}

impl PipeWirePortalLink {
    pub fn new() -> Self {
        Self {
            selection: Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn publish(&self, selection: PortalSelection) {
        if let Ok(mut slot) = self.selection.lock() {
            *slot = Some(selection);
            self.generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn binding_marker(&self) -> Option<String> {
        self.selection.lock().ok().and_then(|slot| {
            slot.as_ref()
                .map(|selection| format!("portal:{}", selection.binding_id))
        })
    }
    pub fn streams(&self) -> Vec<PortalStreamInfo> {
        self.selection
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|selection| selection.streams.clone()))
            .unwrap_or_default()
    }

    /// Takes ownership of the remote fd exactly once. The portal proxy/session
    /// stays in the link while the PipeWire core is alive.
    pub fn take_remote(&self) -> Option<OwnedFd> {
        self.selection
            .lock()
            .ok()
            .and_then(|mut slot| slot.as_mut().and_then(|selection| selection.remote.take()))
    }
}

impl Default for PipeWirePortalLink {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LinuxVideoError {
    #[error("desktop portal denied capture permission")]
    PortalDenied,
    #[error("PipeWire source is offline")]
    SourceOffline,
    #[error("DMA-BUF import is unavailable")]
    DmaBufUnavailable,
    #[error("invalid DMA-BUF metadata")]
    InvalidDmaBuf,
    #[error("Vulkan device lost")]
    DeviceLost,
    #[error("desktop portal failed: {0}")]
    Portal(String),
    #[error("PipeWire failed: {0}")]
    PipeWire(String),
}

#[derive(Debug)]
pub struct DmaBufPlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

#[derive(Debug)]
pub enum FrameMemory {
    DmaBuf { planes: Vec<DmaBufPlane> },
}

#[derive(Debug)]
pub struct CapturedFrame {
    pub source_id: Uuid,
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub width: u32,
    pub height: u32,
    pub drm_format: u32,
    pub modifier: u64,
    pub memory: FrameMemory,
    pub buffer_token: SendBufferToken,
}

/// Opaque PipeWire buffer pointer transported only for return to the same
/// PipeWire loop thread.  The renderer never dereferences this value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SendBufferToken(pub usize);
unsafe impl Send for SendBufferToken {}

#[derive(Debug)]
pub enum FrameMessage {
    Frame(CapturedFrame),
    SourceError { source_id: Uuid, reason: String },
}

#[derive(Debug)]
enum CaptureCommand {
    Start {
        source_id: Uuid,
        node_id: u32,
        remote: Option<OwnedFd>,
    },
    Stop {
        source_id: Uuid,
    },
    Return {
        source_id: Uuid,
        token: SendBufferToken,
    },
    Shutdown,
}

pub struct CaptureHandle {
    commands: mpsc::Sender<CaptureCommand>,
    /// Schreibende der Selbst-Pipe; je Befehl ein Byte weckt die
    /// Mainloop-Quelle des Capture-Threads.
    wake: UnixStream,
    frames: mpsc::Receiver<FrameMessage>,
    thread: Option<thread::JoinHandle<()>>,
}

impl CaptureHandle {
    pub fn spawn() -> Result<Self, LinuxVideoError> {
        let (command_tx, command_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::channel();
        let (wake_read, wake_write) =
            UnixStream::pair().map_err(|error| LinuxVideoError::PipeWire(error.to_string()))?;
        // Volle Pipe darf den Sender nicht blockieren; ein verlorener
        // Weck-Schreibversuch ist harmlos, es liegen dann bereits Bytes an.
        let _ = wake_write.set_nonblocking(true);
        let thread = thread::Builder::new()
            .name("pipewire-video".into())
            .spawn(move || capture_thread(command_rx, frame_tx, wake_read))
            .map_err(|error| LinuxVideoError::PipeWire(error.to_string()))?;
        Ok(Self {
            commands: command_tx,
            wake: wake_write,
            frames: frame_rx,
            thread: Some(thread),
        })
    }

    pub fn start(&self, source_id: Uuid, node_id: u32, remote: Option<OwnedFd>) {
        let _ = self.commands.send(CaptureCommand::Start {
            source_id,
            node_id,
            remote,
        });
        // Je Befehl ein Weck-Byte; die Mainloop erwacht nur bei Bedarf.
        let _ = (&self.wake).write(&[1]);
    }

    pub fn stop(&self, source_id: Uuid) {
        let _ = self.commands.send(CaptureCommand::Stop { source_id });
        // Je Befehl ein Weck-Byte; die Mainloop erwacht nur bei Bedarf.
        let _ = (&self.wake).write(&[1]);
    }

    pub fn return_buffer(&self, source_id: Uuid, token: SendBufferToken) {
        let _ = self
            .commands
            .send(CaptureCommand::Return { source_id, token });
        // Je Befehl ein Weck-Byte; die Mainloop erwacht nur bei Bedarf.
        let _ = (&self.wake).write(&[1]);
    }

    pub fn try_recv(&self) -> Result<FrameMessage, mpsc::TryRecvError> {
        self.frames.try_recv()
    }

    pub fn shutdown(&mut self) {
        let _ = self.commands.send(CaptureCommand::Shutdown);
        // Vor dem Join wecken: ohne Byte würde die idle Mainloop den
        // Shutdown nie sehen und thread::join blockiert für immer.
        let _ = (&self.wake).write(&[1]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct StreamData {
    source_id: Uuid,
    width: u32,
    height: u32,
    drm_format: u32,
    modifier: u64,
    sequence: u64,
    pending: Arc<Mutex<HashSet<SendBufferToken>>>,
    frames: mpsc::Sender<FrameMessage>,
}

struct ActiveStream {
    stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<StreamData>,
    pending: Arc<Mutex<HashSet<SendBufferToken>>>,
}

struct PipeWireState {
    core: Option<pw::core::CoreRc>,
    streams: HashMap<Uuid, ActiveStream>,
    frames: mpsc::Sender<FrameMessage>,
}

fn capture_thread(
    commands: mpsc::Receiver<CaptureCommand>,
    frames: mpsc::Sender<FrameMessage>,
    wake: UnixStream,
) {
    pw::init();
    let mainloop = match pw::main_loop::MainLoopRc::new(None) {
        Ok(value) => value,
        Err(_) => return,
    };
    let context = match pw::context::ContextRc::new(&mainloop, None) {
        Ok(value) => value,
        Err(_) => return,
    };
    let state = Rc::new(RefCell::new(PipeWireState {
        core: None,
        streams: HashMap::new(),
        frames: frames.clone(),
    }));
    // Selbst-Pipe als Mainloop-Quelle statt eines 5-ms-Timers: die Loop
    // erwacht nur, wenn CaptureHandle je gesendetem Befehl ein Byte schreibt
    // — kein Leerlauf-Polling mehr, Befehle bleiben sofort bedienbar.
    let _ = wake.set_nonblocking(true);
    let loop_weak = mainloop.downgrade();
    // Bindung hält die IO-Quelle am Leben; Drop entfernt sie aus der Loop.
    let _command_pump = mainloop.loop_().add_io(wake, IoFlags::IN, move |pipe| {
        // Verwerfbare Bytes lesen, sonst bleibt die Quelle lesbar und die
        // Loop dreht im Dauerkreis.
        let mut discard = [0u8; 256];
        loop {
            match pipe.read(&mut discard) {
                Ok(n) if n == discard.len() => continue,
                Ok(_) => break,
                Err(_) => break,
            }
        }
        let mut state = state.borrow_mut();
        while let Ok(command) = commands.try_recv() {
            match command {
                CaptureCommand::Start {
                    source_id,
                    node_id,
                    remote,
                } => {
                    if let Some(remote) = remote
                        && state.core.is_none()
                    {
                        // connect_fd_rc consumes the fd but does not close it
                        // when pw_context_connect_fd fails; reclaim and drop
                        // the OwnedFd on the error path.
                        let fd = remote.into_raw_fd();
                        match pw::context::ContextRc::connect_fd_rc(
                            &context,
                            unsafe { OwnedFd::from_raw_fd(fd) },
                            None,
                        ) {
                            Ok(core) => state.core = Some(core),
                            Err(error) => {
                                drop(unsafe { OwnedFd::from_raw_fd(fd) });
                                let _ = state.frames.send(FrameMessage::SourceError {
                                    source_id,
                                    reason: format!(
                                        "PipeWire-Portalverbindung fehlgeschlagen: {error}"
                                    ),
                                });
                                continue;
                            }
                        }
                    }
                    if state.streams.contains_key(&source_id) {
                        continue;
                    }
                    let Some(core) = state.core.clone() else {
                        let _ = state.frames.send(FrameMessage::SourceError {
                            source_id,
                            reason: "PipeWire-Portalverbindung nicht verfügbar".into(),
                        });
                        continue;
                    };
                    match create_stream(core, source_id, node_id, state.frames.clone()) {
                        Ok(active) => {
                            state.streams.insert(source_id, active);
                        }
                        Err(error) => {
                            let _ = state.frames.send(FrameMessage::SourceError {
                                source_id,
                                reason: error,
                            });
                        }
                    }
                }
                CaptureCommand::Stop { source_id } => {
                    state.streams.remove(&source_id);
                }
                CaptureCommand::Return { source_id, token } => {
                    if let Some(active) = state.streams.get_mut(&source_id) {
                        let matches = active
                            .pending
                            .lock()
                            .ok()
                            .is_some_and(|mut pending| pending.remove(&token));
                        if matches {
                            unsafe {
                                active
                                    .stream
                                    .queue_raw_buffer(token.0 as *mut pw::sys::pw_buffer);
                            }
                        }
                    }
                }
                CaptureCommand::Shutdown => {
                    state.streams.clear();
                    state.core.take();
                    if let Some(mainloop) = loop_weak.upgrade() {
                        mainloop.quit();
                    }
                    return;
                }
            }
        }
    });
    mainloop.run();
}

fn create_stream(
    core: pw::core::CoreRc,
    source_id: Uuid,
    node_id: u32,
    frames: mpsc::Sender<FrameMessage>,
) -> Result<ActiveStream, String> {
    let stream = pw::stream::StreamRc::new(
        core,
        &format!("hooviestar-portal-{source_id}"),
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => "hooviestar-portal-capture",
        },
    )
    .map_err(|error| error.to_string())?;
    let pending = Arc::new(Mutex::new(HashSet::new()));
    let data = StreamData {
        source_id,
        width: 0,
        height: 0,
        drm_format: drm_xrgb8888(),
        modifier: DRM_FORMAT_MOD_INVALID,
        sequence: 0,
        pending: pending.clone(),
        frames,
    };
    let listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_, data, _, new| {
            if let pw::stream::StreamState::Error(reason) = new {
                let _ = data.frames.send(FrameMessage::SourceError {
                    source_id: data.source_id,
                    reason,
                });
                if let Ok(mut pending) = data.pending.lock() {
                    pending.clear();
                }
            }
        })
        .param_changed(|stream, data, id, param| {
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                return;
            };
            let mut format = pw::spa::param::video::VideoInfoRaw::default();
            if format.parse(param).is_err() {
                let _ = data.frames.send(FrameMessage::SourceError {
                    source_id: data.source_id,
                    reason: "PipeWire-Videoformat konnte nicht ausgehandelt werden".into(),
                });
                return;
            }
            let size = format.size();
            data.width = size.width;
            data.height = size.height;
            data.drm_format = spa_format_to_drm(format.format().as_raw());
            data.modifier = format.modifier();
            match dma_buf_params(size.width, size.height) {
                Ok(values) => {
                    let Some(pod) = pw::spa::pod::Pod::from_bytes(&values) else {
                        return;
                    };
                    if let Err(error) = stream.update_params(&mut [pod]) {
                        let _ = data.frames.send(FrameMessage::SourceError {
                            source_id: data.source_id,
                            reason: format!("PipeWire-DMA-BUF-Aushandlung: {error}"),
                        });
                    }
                }
                Err(reason) => {
                    let _ = data.frames.send(FrameMessage::SourceError {
                        source_id: data.source_id,
                        reason,
                    });
                }
            }
        })
        .process(|stream, data| {
            // Keep a dequeued raw pw_buffer out of PipeWire until Vulkan has
            // finished sampling it.  Using dequeue_raw_buffer avoids the safe
            // Buffer wrapper's Drop implementation, which queues immediately.
            if data
                .pending
                .lock()
                .map_or(true, |pending| pending.len() >= 2)
            {
                return;
            }
            let raw = unsafe { stream.dequeue_raw_buffer() };
            if raw.is_null() {
                return;
            }
            let Some(frame) = (unsafe { extract_frame(raw, data) }) else {
                unsafe { stream.queue_raw_buffer(raw) };
                return;
            };
            if let Ok(mut pending) = data.pending.lock() {
                pending.insert(frame.buffer_token);
            }
            if data.frames.send(FrameMessage::Frame(frame)).is_err() {
                if let Ok(mut pending) = data.pending.lock() {
                    pending.remove(&SendBufferToken(raw as usize));
                }
                unsafe { stream.queue_raw_buffer(raw) };
            }
        })
        .remove_buffer(|_, data, raw| {
            let token = SendBufferToken(raw as usize);
            if let Ok(mut pending) = data.pending.lock() {
                pending.remove(&token);
            }
        })
        .register()
        .map_err(|error| error.to_string())?;

    let format = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::RGBx,
            pw::spa::param::video::VideoFormat::RGBA,
            pw::spa::param::video::VideoFormat::BGRA
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            pw::spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            pw::spa::utils::Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            pw::spa::utils::Fraction { num: 60, denom: 1 },
            pw::spa::utils::Fraction { num: 1, denom: 1 },
            pw::spa::utils::Fraction { num: 120, denom: 1 }
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoModifier,
            pw::spa::pod::Value::Choice(pw::spa::pod::ChoiceValue::Long(pw::spa::utils::Choice::<
                i64,
            >(
                pw::spa::utils::ChoiceFlags::empty(),
                pw::spa::utils::ChoiceEnum::Range {
                    default: DRM_FORMAT_MOD_INVALID as i64,
                    min: DRM_FORMAT_MOD_INVALID as i64,
                    max: DRM_FORMAT_MOD_INVALID as i64,
                },
            ),))
        )
    );
    let values = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(format),
    )
    .map_err(|error| error.to_string())?
    .0
    .into_inner();
    let Some(pod) = pw::spa::pod::Pod::from_bytes(&values) else {
        return Err("PipeWire-Formatpod konnte nicht erstellt werden".into());
    };
    let mut params = [pod];
    stream
        .connect(
            pw::spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|error| error.to_string())?;
    Ok(ActiveStream {
        stream,
        _listener: listener,
        pending,
    })
}

fn dma_buf_params(width: u32, height: u32) -> Result<Vec<u8>, String> {
    let max_size = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| i32::try_from(bytes).ok())
        .ok_or_else(|| "PipeWire-Videopuffer ist zu groß".to_string())?;
    let stride = width
        .checked_mul(4)
        .and_then(|bytes| i32::try_from(bytes).ok())
        .ok_or_else(|| "PipeWire-Videostride ist zu groß".to_string())?;
    let object = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
        id: pw::spa::param::ParamType::Buffers.as_raw(),
        properties: vec![
            pw::spa::pod::Property::new(
                pw::spa::sys::SPA_PARAM_BUFFERS_buffers,
                pw::spa::pod::Value::Choice(pw::spa::pod::ChoiceValue::Int(
                    pw::spa::utils::Choice(
                        pw::spa::utils::ChoiceFlags::empty(),
                        pw::spa::utils::ChoiceEnum::Range {
                            default: 4,
                            min: 2,
                            max: 8,
                        },
                    ),
                )),
            ),
            pw::spa::pod::Property::new(
                pw::spa::sys::SPA_PARAM_BUFFERS_blocks,
                pw::spa::pod::Value::Int(1),
            ),
            pw::spa::pod::Property::new(
                pw::spa::sys::SPA_PARAM_BUFFERS_size,
                pw::spa::pod::Value::Int(max_size),
            ),
            pw::spa::pod::Property::new(
                pw::spa::sys::SPA_PARAM_BUFFERS_stride,
                pw::spa::pod::Value::Int(stride),
            ),
            pw::spa::pod::Property::new(
                pw::spa::sys::SPA_PARAM_BUFFERS_dataType,
                pw::spa::pod::Value::Int(1 << pw::spa::sys::SPA_DATA_DmaBuf),
            ),
        ],
    };
    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(object),
    )
    .map_err(|error| error.to_string())
    .map(|serialized| serialized.0.into_inner())
}

unsafe fn extract_frame(
    raw: *mut pw::sys::pw_buffer,
    data: &mut StreamData,
) -> Option<CapturedFrame> {
    let pw_buffer = unsafe { &*raw };
    if pw_buffer.buffer.is_null() {
        return None;
    }
    let spa_buffer = unsafe { &*pw_buffer.buffer };
    if spa_buffer.n_datas == 0 || spa_buffer.datas.is_null() || data.width == 0 || data.height == 0
    {
        return None;
    }
    let mut planes = Vec::new();
    for index in 0..spa_buffer.n_datas {
        let descriptor = unsafe { &*spa_buffer.datas.add(index as usize) };
        if descriptor.chunk.is_null() || descriptor.type_ != pw::spa::sys::SPA_DATA_DmaBuf {
            continue;
        }
        let chunk = unsafe { &*descriptor.chunk };
        let offset = chunk.offset;
        let stride = chunk.stride.max(0) as u32;
        if stride == 0 || descriptor.fd < 0 {
            return None;
        }
        let fd = unsafe { BorrowedFd::borrow_raw(descriptor.fd as i32) }
            .try_clone_to_owned()
            .ok()?;
        planes.push(DmaBufPlane { fd, offset, stride });
    }
    if planes.is_empty() {
        return None;
    }
    let token = SendBufferToken(raw as usize);
    let sequence = data.sequence;
    data.sequence = data.sequence.wrapping_add(1);
    Some(CapturedFrame {
        source_id: data.source_id,
        sequence,
        timestamp_ns: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
        width: data.width,
        height: data.height,
        drm_format: data.drm_format,
        modifier: data.modifier,
        memory: FrameMemory::DmaBuf { planes },
        buffer_token: token,
    })
}

pub fn drm_xrgb8888() -> u32 {
    fourcc(b'X', b'R', b'2', b'4')
}

fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    u32::from(a) | (u32::from(b) << 8) | (u32::from(c) << 16) | (u32::from(d) << 24)
}

fn spa_format_to_drm(value: u32) -> u32 {
    match value {
        value if value == pw::spa::param::video::VideoFormat::RGBA.as_raw() => {
            fourcc(b'A', b'B', b'2', b'4')
        }
        value if value == pw::spa::param::video::VideoFormat::BGRA.as_raw() => {
            fourcc(b'A', b'R', b'2', b'4')
        }
        value if value == pw::spa::param::video::VideoFormat::RGBx.as_raw() => {
            fourcc(b'X', b'B', b'2', b'4')
        }
        _ => drm_xrgb8888(),
    }
}
