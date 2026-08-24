//! Linux local-media playback. Video negotiation is deliberately restricted to
//! hardware DMA-BUF NV12; system-memory samples are rejected, never uploaded.

use super::linux::DmaBufPlane;
use crate::{
    audio::{MediaAudioBus, PcmRing, SAMPLE_RATE},
    engine::{EngineEvent, MediaRuntimeState},
};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_allocators::DmaBufMemory;
use gstreamer_app::{AppSink, AppSinkCallbacks};
use gstreamer_video::VideoInfoDmaDrm;
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    os::fd::BorrowedFd,
    path::Path,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct MediaVideoFrame {
    pub source_id: Uuid,
    pub sequence: u64,
    pub sample: gst::Sample,
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: u64,
    pub drm_format: u32,
    pub modifier: u64,
}
impl MediaVideoFrame {
    /// Duplicate plane fds while the retained sample owns the underlying
    /// GstBuffer. The renderer keeps this sample until its Vulkan fence signals.
    pub fn dma_buf_planes(&self) -> Result<Vec<DmaBufPlane>, String> {
        let caps = self
            .sample
            .caps()
            .ok_or_else(|| "video sample has no caps".to_string())?;
        let info = VideoInfoDmaDrm::from_caps(caps)
            .map_err(|error| format!("invalid DMA-DRM video caps: {error}"))?;
        let buffer = self
            .sample
            .buffer()
            .ok_or_else(|| "video sample has no buffer".to_string())?;
        let mut planes = Vec::with_capacity(info.n_planes() as usize);
        for plane in 0..info.n_planes() as usize {
            let offset = info.offset()[plane];
            let next_offset = info
                .offset()
                .get(plane + 1)
                .copied()
                .unwrap_or_else(|| buffer.size());
            let end = next_offset.max(offset + 1).min(buffer.size());
            let (memory_range, skip) = buffer
                .find_memory(offset..end)
                .ok_or_else(|| format!("DMA-BUF plane {plane} has no backing memory"))?;
            if memory_range.len() != 1 {
                return Err(format!(
                    "DMA-BUF plane {plane} spans multiple memory objects"
                ));
            }
            let memory = buffer.peek_memory(memory_range.start);
            let dma = memory
                .downcast_memory_ref::<DmaBufMemory>()
                .ok_or_else(|| "GStreamer negotiated non-DMA-BUF video memory".to_string())?;
            let fd = unsafe { BorrowedFd::borrow_raw(dma.fd()) }
                .try_clone_to_owned()
                .map_err(|error| format!("duplicate media DMA-BUF fd: {error}"))?;
            let plane_offset = memory
                .offset()
                .checked_add(skip)
                .and_then(|offset| u32::try_from(offset).ok())
                .ok_or_else(|| "media DMA-BUF plane offset overflows u32".to_string())?;
            let stride = u32::try_from(info.stride()[plane])
                .map_err(|_| "media DMA-BUF plane stride is invalid".to_string())?;
            planes.push(DmaBufPlane {
                fd,
                offset: plane_offset,
                stride,
            });
        }
        if planes.is_empty() {
            return Err("DMA-BUF video sample has no planes".into());
        }
        Ok(planes)
    }
}

#[derive(Debug)]
pub enum MediaNotice {
    Video(MediaVideoFrame),
    State {
        source_id: Uuid,
        state: MediaRuntimeState,
    },
    Unsupported {
        source_id: Uuid,
        reason: String,
    },
}
#[derive(Clone, Debug)]
pub enum MediaCommand {
    Play,
    Pause,
    Seek(f64),
    SetLoop(bool),
    Stop,
}

struct MediaWorker {
    commands: mpsc::Sender<MediaCommand>,
    ring: Arc<Mutex<PcmRing>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for MediaWorker {
    fn drop(&mut self) {
        self.ring.lock().set_active(false);
        let _ = self.commands.send(MediaCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
pub struct LinuxMedia {
    commands: Arc<StdMutex<HashMap<Uuid, MediaWorker>>>,
    notice_tx: mpsc::SyncSender<MediaNotice>,
    notices: Arc<StdMutex<mpsc::Receiver<MediaNotice>>>,
}
impl LinuxMedia {
    pub fn start(_events: mpsc::Sender<EngineEvent>) -> Result<Self, String> {
        gst::init().map_err(|e| format!("GStreamer init: {e}"))?;
        let (notice_tx, notice_rx) = mpsc::sync_channel(32);
        Ok(Self {
            commands: Arc::new(StdMutex::new(HashMap::new())),
            notice_tx,
            notices: Arc::new(StdMutex::new(notice_rx)),
        })
    }
    pub fn open(
        &self,
        source_id: Uuid,
        path: &str,
        looped: bool,
        audio: &MediaAudioBus,
    ) -> Result<(), String> {
        let uri = gst::glib::filename_to_uri(Path::new(path), None)
            .map_err(|e| format!("media URI: {e}"))?;
        let (tx, rx) = mpsc::channel();
        let ring = Arc::new(Mutex::new(PcmRing::new(SAMPLE_RATE as usize)));
        let worker_ring = ring.clone();
        let audio_bus = audio.clone();
        let notice_tx = self.notice_tx.clone();
        let worker = thread::Builder::new()
            .name(format!("gstreamer-media-{source_id}"))
            .spawn(move || {
                if let Err(reason) = run_pipeline(
                    source_id,
                    &uri,
                    looped,
                    rx,
                    ring,
                    audio_bus,
                    notice_tx.clone(),
                ) {
                    let _ = notice_tx.try_send(MediaNotice::Unsupported { source_id, reason });
                }
            })
            .map_err(|error| error.to_string())?;
        self.commands
            .lock()
            .map_err(|_| "media command lock poisoned")?
            .insert(
                source_id,
                MediaWorker {
                    commands: tx,
                    ring: worker_ring,
                    thread: Some(worker),
                },
            );
        Ok(())
    }
    pub fn command(&self, source_id: Uuid, command: MediaCommand) {
        if let Ok(commands) = self.commands.lock()
            && let Some(worker) = commands.get(&source_id)
        {
            match &command {
                MediaCommand::Play => worker.ring.lock().set_active(true),
                MediaCommand::Pause | MediaCommand::Stop => {
                    worker.ring.lock().set_active(false);
                }
                MediaCommand::Seek(_) | MediaCommand::SetLoop(_) => {}
            }
            let _ = worker.commands.send(command);
        }
    }
    pub fn remove(&self, source_id: Uuid, audio: &MediaAudioBus) {
        if let Ok(mut commands) = self.commands.lock() {
            commands.remove(&source_id);
        }
        audio.lock().remove(&source_id);
    }
    pub fn shutdown(&self, audio: &MediaAudioBus) {
        if let Ok(mut commands) = self.commands.lock() {
            commands.clear();
        }
        audio.lock().clear();
    }
    pub fn drain_notices(&self) -> Vec<MediaNotice> {
        self.notices
            .lock()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }
}

struct MediaPacer {
    origin: std::time::Instant,
    origin_pts_ns: u64,
    last_pts_ns: u64,
    initialized: bool,
}

impl MediaPacer {
    fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
            origin_pts_ns: 0,
            last_pts_ns: 0,
            initialized: false,
        }
    }

    fn delay(&mut self, pts_ns: u64) -> Option<Duration> {
        let now = std::time::Instant::now();
        if !self.initialized || pts_ns < self.last_pts_ns {
            self.origin = now;
            self.origin_pts_ns = pts_ns;
            self.last_pts_ns = pts_ns;
            self.initialized = true;
            return None;
        }
        self.last_pts_ns = pts_ns;
        let target = Duration::from_nanos(pts_ns.saturating_sub(self.origin_pts_ns));
        target.checked_sub(now.duration_since(self.origin))
    }
}

fn pace(pacer: &Mutex<MediaPacer>, pts_ns: u64) {
    let delay = pacer.lock().delay(pts_ns);
    if let Some(delay) = delay {
        thread::sleep(delay);
    }
}

fn run_pipeline(
    source_id: Uuid,
    uri: &str,
    mut looped: bool,
    commands: mpsc::Receiver<MediaCommand>,
    ring: Arc<Mutex<PcmRing>>,
    audio_bus: MediaAudioBus,
    notices: mpsc::SyncSender<MediaNotice>,
) -> Result<(), String> {
    let audio_only = {
        let lower = uri.to_ascii_lowercase();
        lower.ends_with(".wav") || lower.ends_with(".mp3")
    };
    let launch = if audio_only {
        "uridecodebin name=decode ! queue ! audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate=48000,channels=2,layout=interleaved ! appsink name=audio_sink sync=true async=false max-buffers=16 drop=true emit-signals=true".to_string()
    } else {
        "uridecodebin name=decode ! queue ! capsfilter caps=\"video/x-raw(memory:DMABuf),format=DMA_DRM\" ! appsink name=video_sink sync=true async=false max-buffers=2 drop=false emit-signals=true decode. ! queue ! audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate=48000,channels=2,layout=interleaved ! appsink name=audio_sink sync=true async=false max-buffers=16 drop=true emit-signals=true".to_string()
    };
    let element = gst::parse::launch_full(&launch, None, gst::ParseFlags::empty())
        .map_err(|e| format!("hardware DMA-BUF pipeline: {e}"))?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| "media pipeline is not a GstPipeline".to_string())?;
    pipeline
        .by_name("decode")
        .ok_or_else(|| "uridecodebin missing".to_string())?
        .set_property("uri", uri);
    let video = pipeline
        .by_name("video_sink")
        .map(|element| {
            element
                .downcast::<AppSink>()
                .map_err(|_| "video sink is not appsink".to_string())
        })
        .transpose()?;
    let audio = pipeline
        .by_name("audio_sink")
        .ok_or_else(|| "audio appsink missing".to_string())?
        .downcast::<AppSink>()
        .map_err(|_| "audio sink is not appsink".to_string())?;
    if let Some(video) = video {
        let notice_for_video = notices.clone();
        let video_sequence = Arc::new(AtomicU64::new(0));
        let sequence_for_video = video_sequence.clone();
        let video_pacer = Arc::new(Mutex::new(MediaPacer::new()));
        let pacer_for_video = video_pacer.clone();
        video.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let caps = sample.caps().ok_or(gst::FlowError::NotNegotiated)?;
                    if !caps
                        .features(0)
                        .is_some_and(|features| features.contains("memory:DMABuf"))
                    {
                        let _ = notice_for_video.try_send(MediaNotice::Unsupported {
                        source_id,
                        reason:
                            "Software-/Systemspeicher-Videodecoder abgelehnt; DMA-BUF erforderlich"
                                .into(),
                    });
                        return Err(gst::FlowError::NotNegotiated);
                    }
                    let info = VideoInfoDmaDrm::from_caps(caps)
                        .map_err(|_| gst::FlowError::NotNegotiated)?;
                    let timestamp_ns = sample
                        .buffer()
                        .and_then(|buffer| buffer.pts())
                        .map(|timestamp| timestamp.nseconds())
                        .unwrap_or(0);
                    pace(&pacer_for_video, timestamp_ns);
                    let _ = notice_for_video.try_send(MediaNotice::Video(MediaVideoFrame {
                        source_id,
                        sequence: sequence_for_video.fetch_add(1, Ordering::Relaxed),
                        sample,
                        width: info.width(),
                        height: info.height(),
                        timestamp_ns,
                        drm_format: info.fourcc(),
                        modifier: info.modifier(),
                    }));
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }
    let ring_for_audio = ring.clone();
    let audio_registered = Arc::new(AtomicBool::new(false));
    let registered_for_audio = audio_registered.clone();
    let bus_for_audio = audio_bus.clone();
    let audio_pacer = Arc::new(Mutex::new(MediaPacer::new()));
    let pacer_for_audio = audio_pacer.clone();
    audio.set_callbacks(
        AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let pts_ns = buffer
                    .pts()
                    .map(|timestamp| timestamp.nseconds())
                    .unwrap_or(0);
                pace(&pacer_for_audio, pts_ns);
                let bytes = map.as_slice();
                if !registered_for_audio.swap(true, Ordering::AcqRel) {
                    bus_for_audio
                        .lock()
                        .insert(source_id, ring_for_audio.clone());
                }
                let mut ring = ring_for_audio.lock();
                for chunk in bytes.as_chunks::<8>().0 {
                    ring.push([
                        f32::from_le_bytes(chunk[0..4].try_into().unwrap()),
                        f32::from_le_bytes(chunk[4..8].try_into().unwrap()),
                    ]);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|e| format!("pause media pipeline: {e:?}"))?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("start media pipeline: {e:?}"))?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| "media pipeline has no bus".to_string())?;
    let mut playing = true;
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                MediaCommand::Play => {
                    if !playing {
                        pipeline
                            .seek_simple(
                                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                                gst::ClockTime::ZERO,
                            )
                            .map_err(|error| format!("restart media: {error}"))?;
                    }
                    pipeline
                        .set_state(gst::State::Playing)
                        .map_err(|e| format!("play media: {e:?}"))?;
                    playing = true;
                }
                MediaCommand::Pause => {
                    pipeline
                        .set_state(gst::State::Paused)
                        .map_err(|e| format!("pause media: {e:?}"))?;
                    playing = false;
                }
                MediaCommand::Seek(seconds) => {
                    let nanos = (seconds.max(0.0) * 1_000_000_000.0) as u64;
                    pipeline
                        .seek_simple(
                            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                            gst::ClockTime::from_nseconds(nanos),
                        )
                        .map_err(|error| format!("seek media: {error}"))?;
                }
                MediaCommand::SetLoop(value) => looped = value,
                MediaCommand::Stop => {
                    let _ = pipeline.set_state(gst::State::Null);
                    return Ok(());
                }
            }
        }
        if let Some(message) = bus.timed_pop(Some(gst::ClockTime::from_mseconds(10))) {
            use gst::MessageView;
            match message.view() {
                MessageView::Eos(..) => {
                    if looped {
                        let _ = pipeline.seek_simple(
                            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                            gst::ClockTime::ZERO,
                        );
                    } else {
                        playing = false;
                        ring.lock().set_active(false);
                        let _ = pipeline.set_state(gst::State::Paused);
                    }
                }
                MessageView::Error(error) => {
                    let detail = error.debug().unwrap_or_default();
                    if detail.contains("not-linked")
                        || error
                            .error()
                            .message()
                            .contains("Internal data stream error")
                    {
                        return Err(
                            "Hardware-DMA-BUF-Videodecoder nicht verfügbar; Software- und Systemspeicher-Videodecoder bleiben deaktiviert"
                                .into(),
                        );
                    }
                    return Err(format!("{}: {detail}", error.error()));
                }
                _ => {}
            }
        }
        let position = pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| t.seconds() as f64)
            .unwrap_or(0.0);
        let _ = notices.try_send(MediaNotice::State {
            source_id,
            state: MediaRuntimeState {
                playing,
                position_seconds: position,
                duration_seconds: pipeline
                    .query_duration::<gst::ClockTime>()
                    .map(|t| t.seconds() as f64),
            },
        });
        thread::sleep(Duration::from_millis(40));
    }
}
