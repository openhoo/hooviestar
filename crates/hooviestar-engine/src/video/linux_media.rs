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
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
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
    SeekFailed {
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
        // Windows-Paritaet: existiert fuer die Quelle bereits ein
        // Bus-Ring (Sitzung ohne remove() neu eroeffnet), wird er
        // wiederverwendet statt einen neuen zu praegen; der Mixer
        // behaelt seine Ring-Identitaet. Stale PCM wird beim Binden
        // verworfen, der Ring startet stumm.
        let ring = audio
            .lock()
            .get(&source_id)
            .cloned()
            .unwrap_or_else(|| Arc::new(Mutex::new(PcmRing::new(SAMPLE_RATE as usize))));
        {
            let mut bound = ring.lock();
            bound.clear();
            bound.set_active(false);
        }
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
                    send_control(&notice_tx, MediaNotice::Unsupported { source_id, reason });
                }
            })
            .map_err(|error| error.to_string())?;
        match self.commands.lock() {
            Ok(mut commands) => {
                commands.insert(
                    source_id,
                    MediaWorker {
                        commands: tx,
                        ring: worker_ring,
                        thread: Some(worker),
                    },
                );
            }
            Err(_) => {
                // The worker thread already runs; hand it a Stop so the
                // pipeline tears down via PipelineNullGuard instead of being
                // orphaned by this early return.
                let _ = tx.send(MediaCommand::Stop);
                return Err("media command lock poisoned".to_string());
            }
        }
        Ok(())
    }
    pub fn command(&self, source_id: Uuid, command: MediaCommand) {
        if let Ok(commands) = self.commands.lock()
            && let Some(worker) = commands.get(&source_id)
        {
            match &command {
                // Play does NOT pre-arm the ring here: the worker arms it
                // only after the pipeline actually reached Playing, so a
                // reopen/backoff window never exposes a re-armed-but-stale
                // ring. Pause/Stop mute immediately.
                MediaCommand::Play => {}
                MediaCommand::Pause | MediaCommand::Stop => {
                    worker.ring.lock().set_active(false);
                }
                MediaCommand::Seek(_) | MediaCommand::SetLoop(_) => {}
            }
            let _ = worker.commands.send(command);
        }
    }
    /// Ring-Identitaet des aktiven Workers fuer Paritaetspruefungen:
    /// die Audio-Seite kann so pruefen, ob ihr zwischengespeicherter
    /// Ring noch der aktuelle ist (Linux praegt pro Sitzung neu,
    /// remove() loescht den Bus-Eintrag).
    pub fn audio_ring_handle(&self, source_id: Uuid) -> Option<Arc<Mutex<PcmRing>>> {
        let commands = self.commands.lock().ok()?;
        commands.get(&source_id).map(|worker| worker.ring.clone())
    }
    pub fn remove(&self, source_id: Uuid, audio: &MediaAudioBus) {
        // Join the worker AFTER releasing the map lock: MediaWorker::drop
        // waits for the media thread, which must never stall the render
        // thread while it holds the command map.
        let worker = self
            .commands
            .lock()
            .ok()
            .and_then(|mut commands| commands.remove(&source_id));
        drop(worker);
        audio.lock().remove(&source_id);
    }
    pub fn shutdown(&self, audio: &MediaAudioBus) {
        // Same as remove(): take every worker out, release the lock, then
        // let the workers join.
        let workers = self
            .commands
            .lock()
            .map(|mut commands| std::mem::take(&mut *commands))
            .unwrap_or_default();
        drop(workers);
        audio.lock().clear();
    }
    pub fn drain_notices(&self) -> Vec<MediaNotice> {
        self.notices
            .lock()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }
}

/// Forward PTS delta above this is a seek-style discontinuity rather than a
/// pacing target; legitimate inter-frame intervals stay far below it.
const PACE_DISCONT_JUMP_NS: u64 = 500_000_000;
/// Clamp for one pacing sleep so a missed transition can never park a
/// streaming thread for the length of the jump.
const PACE_MAX_SLEEP: Duration = Duration::from_millis(100);

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

    fn delay(&mut self, pts_ns: u64, discont: bool) -> Option<Duration> {
        let now = std::time::Instant::now();
        // Backward jumps (EOS restart, rewind seeks) and forward jumps beyond
        // any legal inter-frame interval (flush seeks) are discontinuities:
        // re-anchor instead of sleeping until wall time catches up.
        let forward_jump_ns = pts_ns.saturating_sub(self.last_pts_ns);
        if discont
            || !self.initialized
            || pts_ns < self.last_pts_ns
            || forward_jump_ns > PACE_DISCONT_JUMP_NS
        {
            self.origin = now;
            self.origin_pts_ns = pts_ns;
            self.last_pts_ns = pts_ns;
            self.initialized = true;
            return None;
        }
        self.last_pts_ns = pts_ns;
        let target = Duration::from_nanos(pts_ns.saturating_sub(self.origin_pts_ns));
        target
            .checked_sub(now.duration_since(self.origin))
            .map(|delay| delay.min(PACE_MAX_SLEEP))
    }
}

fn pace(pacer: &Mutex<MediaPacer>, pts_ns: u64, discont: bool) {
    let delay = pacer.lock().delay(pts_ns, discont);
    if let Some(delay) = delay {
        thread::sleep(delay);
    }
}

/// Unsupported verdicts are latched per playback session: one root cause must
/// produce one notice, not one per rejected sample or bus message.
fn send_unsupported_once(
    latch: &AtomicBool,
    notices: &mpsc::SyncSender<MediaNotice>,
    source_id: Uuid,
    reason: String,
) {
    // Terminal verdict: retried through a bounded window so a burst of
    // video-frame try_sends cannot deadlock teardown, yet survives a full
    // channel long enough for the drain to catch up.
    if !latch.swap(true, Ordering::AcqRel) {
        send_control(notices, MediaNotice::Unsupported { source_id, reason });
    }
}

/// Bounded delivery for control notices: video-frame try_send bursts can
/// fill the whole channel during a render stall, and the render thread may
/// be the very thread joining this worker on teardown — a blocking send
/// there would deadlock MediaWorker::drop forever. Retry through transient
/// bursts, then give up silently instead of hanging.
fn send_control(notices: &mpsc::SyncSender<MediaNotice>, mut notice: MediaNotice) {
    for _ in 0..200 {
        match notices.try_send(notice) {
            Ok(()) => return,
            Err(mpsc::TrySendError::Full(returned)) => notice = returned,
            Err(mpsc::TrySendError::Disconnected(_)) => return,
        }
        thread::sleep(Duration::from_millis(10));
    }
}

struct PipelineNullGuard<'a>(&'a gst::Pipeline);

impl Drop for PipelineNullGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

/// Bounded reopen policy for transient GStreamer failures: instead of
/// latching a session-wide Unsupported verdict on the first hiccup, tear
/// the pipeline down and retry opening with a fixed backoff. The budget is
/// cumulative across attempts and only refills after CLEAN_STRETCH of
/// genuinely delivered playback within one attempt, so a source that emits
/// one sample per cycle and then fails cannot livelock the rebuild loop.
/// MAX_REOPEN_CYCLES charges without such a clean stretch end the retries
/// with a latched Unsupported verdict.
const MAX_REOPEN_CYCLES: u32 = 6;
const RETRY_BACKOFF: Duration = Duration::from_millis(250);
const CLEAN_STRETCH: Duration = Duration::from_secs(5);

/// Cumulative transient-failure budget shared by all pipeline attempts.
struct ReopenBudget {
    /// Transient results charged since the last clean stretch.
    cumulative: AtomicU32,
    /// First delivered-sample instant of the current attempt; a full
    /// CLEAN_STRETCH of delivered playback resets the cumulative charge.
    playback_start: Mutex<Option<Instant>>,
    /// Most recent delivered-sample instant of the current attempt; lets
    /// charge() distinguish sustained playback from a lone stale sample.
    last_sample: Mutex<Option<Instant>>,
}

impl ReopenBudget {
    fn new() -> Self {
        Self {
            cumulative: AtomicU32::new(0),
            playback_start: Mutex::new(None),
            last_sample: Mutex::new(None),
        }
    }

    /// Record the first delivered sample push of the current attempt;
    /// later pushes keep the original instant.
    fn note_playback_start(&self) {
        let mut start = self.playback_start.lock();
        if start.is_none() {
            *start = Some(Instant::now());
        }
    }

    /// Record one delivered sample of the current attempt: starts the
    /// clean-stretch clock on the first sample (set-if-none) and refreshes
    /// the most-recent-delivery instant on every sample, audio and video.
    fn note_sample(&self) {
        *self.last_sample.lock() = Some(Instant::now());
        self.note_playback_start();
    }

    /// Clear per-attempt delivery tracking; called before every pipeline
    /// attempt so partial delivery cannot accumulate clean-stretch time
    /// across rebuilds.
    fn begin_attempt(&self) {
        *self.playback_start.lock() = None;
        *self.last_sample.lock() = None;
    }

    /// Charge one Transient result and return the cumulative count. The
    /// counter resets only when the previous attempt both started delivery
    /// a full CLEAN_STRETCH ago AND kept delivering until recently; one
    /// sample followed by a long stall is not a clean stretch, so flappers
    /// cannot reset their charge every cycle.
    fn charge(&self) -> u32 {
        // Scoped sequentially, never nested: the sample callbacks take
        // these mutexes in the opposite order.
        let started_long_ago = self
            .playback_start
            .lock()
            .is_some_and(|start| start.elapsed() >= CLEAN_STRETCH);
        let delivered_recently = self
            .last_sample
            .lock()
            .is_some_and(|last| last.elapsed() < CLEAN_STRETCH);
        if started_long_ago && delivered_recently {
            self.cumulative.store(0, Ordering::Relaxed);
        }
        self.cumulative.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Outcome of one pipeline attempt: `Finished` ended playback gracefully,
/// `Transient` carries the reason a reopen should be tried before the
/// existing fatal-Err path latches the session.
enum MediaAttempt {
    Finished,
    Transient(String),
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
    // One Unsupported verdict per playback session (fresh latch per open());
    // the latch survives pipeline rebuilds so a genuine verdict is never
    // retried away.
    let unsupported_latch = Arc::new(AtomicBool::new(false));
    // Desired playback state shared with the pipeline attempts; `true`
    // preserves the cold-open autoplay behavior.
    let desired_playing = Arc::new(AtomicBool::new(true));
    let budget = Arc::new(ReopenBudget::new());
    loop {
        budget.begin_attempt();
        let outcome = run_pipeline_once(
            source_id,
            uri,
            &mut looped,
            &commands,
            &ring,
            &audio_bus,
            &notices,
            &unsupported_latch,
            &desired_playing,
            &budget,
        )?;
        match outcome {
            MediaAttempt::Finished => return Ok(()),
            MediaAttempt::Transient(reason) => {
                let used = budget.charge();
                if used >= MAX_REOPEN_CYCLES {
                    // Sustained flapping without a clean stretch: stop the
                    // rebuild loop and surface a truthful verdict instead of
                    // retrying forever.
                    send_unsupported_once(
                        &unsupported_latch,
                        &notices,
                        source_id,
                        "Medium wiederholt fehlgeschlagen (wiederholtes Nachladen)".to_string(),
                    );
                    // Terminal verdict mirror of run_pipeline_once's tail:
                    // consumers must observe the final paused state even
                    // when the lone Unsupported verdict above was lost on a
                    // saturated channel. The pipeline is already torn down,
                    // so position/duration are unknown.
                    send_control(
                        &notices,
                        MediaNotice::State {
                            source_id,
                            state: MediaRuntimeState {
                                playing: false,
                                position_seconds: 0.0,
                                duration_seconds: None,
                            },
                        },
                    );
                    return Ok(());
                }
                eprintln!(
                    "media pipeline retry {used}/{MAX_REOPEN_CYCLES} in {RETRY_BACKOFF:?}: {reason}"
                );
                thread::sleep(RETRY_BACKOFF);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_pipeline_once(
    source_id: Uuid,
    uri: &str,
    looped: &mut bool,
    commands: &mpsc::Receiver<MediaCommand>,
    ring: &Arc<Mutex<PcmRing>>,
    audio_bus: &MediaAudioBus,
    notices: &mpsc::SyncSender<MediaNotice>,
    unsupported_latch: &Arc<AtomicBool>,
    desired_playing: &Arc<AtomicBool>,
    budget: &Arc<ReopenBudget>,
) -> Result<MediaAttempt, String> {
    let audio_ext: Option<&'static str> = {
        let lower = uri.to_ascii_lowercase();
        [".flac", ".ogg", ".opus", ".m4a", ".aac", ".wav", ".mp3"]
            .into_iter()
            .find(|extension| lower.ends_with(extension))
            .map(|extension| extension.trim_start_matches('.'))
    };
    let audio_only = audio_ext.is_some();
    let launch = if audio_only {
        "uridecodebin name=decode ! queue ! audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate=48000,channels=2,layout=interleaved ! appsink name=audio_sink sync=true async=false max-buffers=16 drop=true emit-signals=true".to_string()
    } else {
        "uridecodebin name=decode ! queue ! capsfilter caps=\"video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format=NV12\" ! appsink name=video_sink sync=true async=false max-buffers=2 drop=false emit-signals=true decode. ! queue ! audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate=48000,channels=2,layout=interleaved ! appsink name=audio_sink sync=true async=false max-buffers=16 drop=true emit-signals=true".to_string()
    };
    let element = gst::parse::launch_full(&launch, None, gst::ParseFlags::empty())
        .map_err(|e| format!("hardware DMA-BUF pipeline: {e}"))?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| "media pipeline is not a GstPipeline".to_string())?;
    // Every error exit after this point must leave the pipeline in NULL;
    // GStreamer only disposes elements and streaming threads safely from a
    // stopped state. The Stop arm sets NULL itself; the guard's extra
    // transition is a harmless no-op.
    let _pipeline_guard = PipelineNullGuard(&pipeline);
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
        let unsupported_for_video = unsupported_latch.clone();
        let notice_for_video = notices.clone();
        let video_sequence = Arc::new(AtomicU64::new(0));
        let sequence_for_video = video_sequence.clone();
        let video_pacer = Arc::new(Mutex::new(MediaPacer::new()));
        let pacer_for_video = video_pacer.clone();
        let budget_for_video = budget.clone();
        video.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let caps = sample.caps().ok_or(gst::FlowError::NotNegotiated)?;
                    if !caps
                        .features(0)
                        .is_some_and(|features| features.contains("memory:DMABuf"))
                    {
                        send_unsupported_once(
                            &unsupported_for_video,
                            &notice_for_video,
                            source_id,
                            "Software-/Systemspeicher-Videodecoder abgelehnt; DMA-BUF erforderlich"
                                .into(),
                        );
                        return Err(gst::FlowError::NotNegotiated);
                    }
                    let info = VideoInfoDmaDrm::from_caps(caps)
                        .map_err(|_| gst::FlowError::NotNegotiated)?;
                    let timestamp_ns = sample
                        .buffer()
                        .and_then(|buffer| buffer.pts())
                        .map(|timestamp| timestamp.nseconds())
                        .unwrap_or(0);
                    let discont = sample
                        .buffer()
                        .is_some_and(|buffer| buffer.flags().contains(gst::BufferFlags::DISCONT));
                    pace(&pacer_for_video, timestamp_ns, discont);
                    // First delivered DMA-BUF frame of this attempt: start
                    // the clean-stretch clock even for audio-less sources.
                    budget_for_video.note_sample();
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
    let budget_for_audio = budget.clone();
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
                let discont = buffer.flags().contains(gst::BufferFlags::DISCONT);
                pace(&pacer_for_audio, pts_ns, discont);
                let bytes = map.as_slice();
                if !registered_for_audio.swap(true, Ordering::AcqRel) {
                    bus_for_audio
                        .lock()
                        .insert(source_id, ring_for_audio.clone());
                    // First delivered sample of this attempt: start the
                    // clean-stretch clock that can refill the reopen budget.
                    budget_for_audio.note_playback_start();
                }
                let mut ring = ring_for_audio.lock();
                for chunk in bytes.as_chunks::<8>().0 {
                    ring.push([
                        f32::from_le_bytes(chunk[0..4].try_into().unwrap()),
                        f32::from_le_bytes(chunk[4..8].try_into().unwrap()),
                    ]);
                }
                // Refresh per-attempt delivery progress on EVERY sample so
                // charge() sees recent delivery, not an ancient first one.
                budget_for_audio.note_sample();
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|e| format!("pause media pipeline: {e:?}"))?;
    // Honor the desired playback state across reopens instead of
    // force-resuming: an attempt that died while paused must come back
    // paused, with the ring left inactive until a real transition to
    // Playing.
    let want_playing = desired_playing.load(Ordering::Relaxed);
    if want_playing {
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("start media pipeline: {e:?}"))?;
        // A retry teardown set the ring inactive (and cleared it); re-arm so
        // audio flows again without waiting for a Play command.
        ring.lock().set_active(true);
    }
    let bus = pipeline
        .bus()
        .ok_or_else(|| "media pipeline has no bus".to_string())?;
    let mut playing = want_playing;
    let mut ended = false;
    // Letzter gesendeter Stand (playing, position, duration) für das
    // State-Dedup über exakte Gleichheit — anders als der Windows-Pfad
    // (send_media_state), der Positionsfortschritt auf 0,25 s quantisiert.
    let mut last_state: Option<(bool, f64, Option<f64>)> = None;
    let mut retry_reason: Option<String> = None;
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                MediaCommand::Play => {
                    // Only a natural end-of-playback restarts from zero; a
                    // plain Play after Pause resumes at the paused position.
                    let mut resumed = !ended;
                    if ended
                        && pipeline
                            .seek_simple(
                                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                                gst::ClockTime::ZERO,
                            )
                            .inspect_err(|error| eprintln!("restart media failed: {error:?}"))
                            .is_ok()
                    {
                        ended = false;
                        resumed = true;
                        // The FLUSH seek invalidated pre-jump PCM: drop
                        // stale audio instead of replaying it over the new
                        // position.
                        ring.lock().clear();
                    }
                    // A failed restart seek leaves playback honestly
                    // paused/inactive; the next Play retries the restart.
                    // A failed state change is a transient hiccup, not a
                    // codec verdict: log it and keep the pipeline running so
                    // Stop stays serviceable and a later Play can retry.
                    if resumed {
                        match pipeline.set_state(gst::State::Playing) {
                            Ok(_) => {
                                playing = true;
                                ring.lock().set_active(true);
                                desired_playing.store(true, Ordering::Relaxed);
                            }
                            Err(_) => {
                                eprintln!("play media: set_state(Playing) fehlgeschlagen");
                            }
                        }
                    }
                    if !playing {
                        // Fehlgeschlagener Neustart/Resume ehrlich melden:
                        // der Renderer schreibt den Bus auf playing=false
                        // zurueck und retryt nicht pro Frame (Windows-
                        // Paritaet). Der naechste Nutzer-Play startet neu.
                        let position = pipeline
                            .query_position::<gst::ClockTime>()
                            .map(|t| t.seconds() as f64)
                            .unwrap_or(0.0);
                        let duration = pipeline
                            .query_duration::<gst::ClockTime>()
                            .map(|t| t.seconds() as f64);
                        send_control(
                            notices,
                            MediaNotice::State {
                                source_id,
                                state: MediaRuntimeState {
                                    playing: false,
                                    position_seconds: position,
                                    duration_seconds: duration,
                                },
                            },
                        );
                    }
                }
                MediaCommand::Pause => {
                    if pipeline.set_state(gst::State::Paused).is_ok() {
                        playing = false;
                        desired_playing.store(false, Ordering::Relaxed);
                    } else {
                        eprintln!("pause media: set_state(Paused) fehlgeschlagen");
                    }
                }
                MediaCommand::Seek(seconds) => {
                    let nanos = (seconds.max(0.0) * 1_000_000_000.0) as u64;
                    match pipeline.seek_simple(
                        gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                        gst::ClockTime::from_nseconds(nanos),
                    ) {
                        Ok(()) => {
                            ended = false;
                            // The FLUSH seek invalidated decoded PCM: drop
                            // stale audio so the ring never replays samples
                            // from before the jump.
                            ring.lock().clear();
                        }
                        Err(error) => {
                            eprintln!("seek media failed: {error:?}");
                            send_control(
                                notices,
                                MediaNotice::SeekFailed {
                                    source_id,
                                    reason: format!("Seek fehlgeschlagen: {error:?}"),
                                },
                            );
                        }
                    }
                }
                MediaCommand::SetLoop(value) => *looped = value,
                MediaCommand::Stop => {
                    let _ = pipeline.set_state(gst::State::Null);
                    return Ok(MediaAttempt::Finished);
                }
            }
        }
        if let Some(message) = bus.timed_pop(Some(gst::ClockTime::from_mseconds(10))) {
            use gst::MessageView;
            match message.view() {
                MessageView::Eos(..) => {
                    if *looped {
                        // A failed loop seek silently wedges playback (no
                        // further frames, ended unreachable); log it, mark
                        // the attempt ended and mute the ring so the next
                        // user Play can take the ended-based restart path.
                        if let Err(error) = pipeline.seek_simple(
                            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                            gst::ClockTime::ZERO,
                        ) {
                            send_control(
                                notices,
                                MediaNotice::SeekFailed {
                                    source_id,
                                    reason: format!("Wiederholungs-Seek fehlgeschlagen: {error:?}"),
                                },
                            );
                            eprintln!("loop media seek failed: {error:?}");
                            ended = true;
                            ring.lock().set_active(false);
                        } else {
                            // The FLUSH loop seek invalidated decoded PCM
                            // from the previous pass: drop it instead of
                            // replaying stale audio at the restart.
                            ring.lock().clear();
                        }
                    } else {
                        playing = false;
                        ended = true;
                        // Natural end: persist the paused/ended desire so a
                        // later transient error reopens paused instead of
                        // spontaneously replaying finished media.
                        desired_playing.store(false, Ordering::Relaxed);
                        ring.lock().set_active(false);
                        let _ = pipeline.set_state(gst::State::Paused);
                    }
                }
                MessageView::Error(error) => {
                    let detail = error.debug().unwrap_or_default();
                    let glib_error = error.error();
                    // Stream-domain errors (not-linked, internal data stream
                    // errors, mid-stream decode failures) and Resource/Core
                    // errors (busy file, IO hiccup, device loss) also fire
                    // for transient playback problems; reopen the pipeline
                    // instead of blaming the hardware decoder stack and
                    // permanently removing the source.
                    let transient = glib_error.kind::<gst::StreamError>().is_some()
                        || detail.contains("not-linked")
                        || glib_error.message().contains("Internal data stream error")
                        || glib_error.kind::<gst::ResourceError>().is_some()
                        || glib_error.kind::<gst::CoreError>().is_some();
                    // A negotiation failure here means no decoder could
                    // produce the DMA-BUF NV12 the capsfilter mandates;
                    // report why immediately instead of retrying or ending
                    // playback silently.
                    let indication = format!("{} {detail}", glib_error.message()).to_lowercase();
                    if transient && indication.contains("negotiat") {
                        // Audio-classified files have no DMA-BUF video
                        // expectation; blame the audio format truthfully.
                        let reason = match audio_ext {
                            Some(extension) => {
                                format!("Audioformat nicht unterstützt: {extension}")
                            }
                            None => {
                                format!("DMA-BUF-Videounterhandlung fehlgeschlagen: {detail}")
                            }
                        };
                        send_unsupported_once(unsupported_latch, notices, source_id, reason);
                        ring.lock().set_active(false);
                        break;
                    }
                    if !transient {
                        // Terminal exit without any rebuild attempt: mute
                        // the ring like every sibling exit, otherwise the
                        // mixer keeps popping stale audio forever.
                        ring.lock().set_active(false);
                        return Err(format!("{}: {detail}", glib_error));
                    }
                    eprintln!("media transient error, reopening pipeline: {detail}");
                    ring.lock().set_active(false);
                    retry_reason = Some(format!("transient media error: {detail}"));
                    break;
                }
                _ => {}
            }
        }
        let position = pipeline
            .query_position::<gst::ClockTime>()
            .map(|t| t.seconds() as f64)
            .unwrap_or(0.0);
        let duration = pipeline
            .query_duration::<gst::ClockTime>()
            .map(|t| t.seconds() as f64);
        let state = (playing, position, duration);
        // MediaState nur bei Aenderung senden; pausierte oder beendete
        // Pipelines erzeugen sonst 25 Events/s ohne jeden Informationsgewinn.
        if last_state != Some(state) {
            let notice = MediaNotice::State {
                source_id,
                state: MediaRuntimeState {
                    playing,
                    position_seconds: position,
                    duration_seconds: duration,
                },
            };
            // Dedup-Gedaechtnis erst nach erfolgreicher Zustellung committen;
            // ein voller Kanal degradiert zu verzoegerter statt verlorener Uebertragung.
            if notices.try_send(notice).is_ok() {
                last_state = Some(state);
            }
        }
        thread::sleep(Duration::from_millis(40));
    }
    if let Some(reason) = retry_reason {
        return Ok(MediaAttempt::Transient(reason));
    }
    // Terminal verdict: this final paused State must reach the consumer
    // even on a full channel; the worker exits right after.
    send_control(
        notices,
        MediaNotice::State {
            source_id,
            state: MediaRuntimeState {
                playing: false,
                position_seconds: pipeline
                    .query_position::<gst::ClockTime>()
                    .map(|t| t.seconds() as f64)
                    .unwrap_or(0.0),
                duration_seconds: pipeline
                    .query_duration::<gst::ClockTime>()
                    .map(|t| t.seconds() as f64),
            },
        },
    );
    Ok(MediaAttempt::Finished)
}
