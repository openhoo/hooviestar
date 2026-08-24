use std::{
    env,
    fs::{self},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;

use crate::project::ProjectV1;

const SAVE_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid project: {0}")]
    Invalid(String),
    #[error("project writer stopped")]
    WriterStopped,
}

pub struct ProjectStore {
    closed: AtomicBool,
    latest: Arc<Mutex<Option<ProjectV1>>>,
    sender: SyncSender<WorkerMessage>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

enum WorkerMessage {
    Save,
    Flush(mpsc::Sender<Result<(), String>>),
    Shutdown(mpsc::Sender<Result<(), String>>),
}

impl ProjectStore {
    pub fn start(path: PathBuf) -> Result<(Self, ProjectV1, Option<PathBuf>), PersistenceError> {
        let (project, corrupt_backup) = load_or_default(&path)?;
        let latest = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker_latest = latest.clone();
        let worker = thread::Builder::new()
            .name("project-writer".into())
            .spawn(move || writer_loop(path, worker_latest, receiver))?;
        Ok((
            Self {
                closed: AtomicBool::new(false),
                latest,
                sender,
                worker: Mutex::new(Some(worker)),
            },
            project,
            corrupt_backup,
        ))
    }

    pub fn submit(&self, project: ProjectV1) -> Result<(), PersistenceError> {
        project.validate().map_err(PersistenceError::Invalid)?;
        {
            // Pruefung und Eintrag unter einem Lock: ein gleichzeitiges
            // shutdown darf das Pending weder zwischen Pruefung und Eintrag
            // noch nach dem finalen Save unbemerkt aendern.
            let mut pending = self.latest.lock();
            if self.closed.load(Ordering::Acquire) {
                return Ok(());
            }
            *pending = Some(project);
        }
        match self.sender.try_send(WorkerMessage::Save) {
            Ok(()) | Err(TrySendError::Full(WorkerMessage::Save)) => Ok(()),
            Err(TrySendError::Full(message)) => self
                .sender
                .send(message)
                .map_err(|_| PersistenceError::WriterStopped),
            Err(TrySendError::Disconnected(_)) => Err(PersistenceError::WriterStopped),
        }
    }

    pub fn flush(&self) -> Result<(), PersistenceError> {
        let (sender, receiver) = mpsc::channel();
        self.sender
            .send(WorkerMessage::Flush(sender))
            .map_err(|_| PersistenceError::WriterStopped)?;
        receive_worker_result(receiver)
    }

    pub fn shutdown(&self) -> Result<(), PersistenceError> {
        {
            let _pending = self.latest.lock();
            self.closed.store(true, Ordering::Release);
        }
        let Some(worker) = self.worker.lock().take() else {
            return Ok(());
        };
        let (sender, receiver) = mpsc::channel();
        self.sender
            .send(WorkerMessage::Shutdown(sender))
            .map_err(|_| PersistenceError::WriterStopped)?;
        let result = receive_worker_result(receiver);
        worker.join().map_err(|_| PersistenceError::WriterStopped)?;
        result
    }
}

pub fn default_project_path() -> Result<PathBuf, PersistenceError> {
    #[cfg(target_os = "windows")]
    let base = env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let base = base.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "configuration directory unavailable",
        )
    })?;
    #[cfg(target_os = "windows")]
    let path = base.join("Hooviestar").join("project.json");
    #[cfg(not(target_os = "windows"))]
    let path = base.join("hooviestar").join("project.json");
    Ok(path)
}

pub fn load_or_default(path: &Path) -> Result<(ProjectV1, Option<PathBuf>), PersistenceError> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<ProjectV1>(&bytes) {
            Ok(project) if project.validate().is_ok() => Ok((project, None)),
            _ => backup_corrupt(path),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((ProjectV1::empty(), None)),
        Err(error) => Err(error.into()),
    }
}

pub fn save_atomic(path: &Path, project: &ProjectV1) -> Result<(), PersistenceError> {
    project.validate().map_err(PersistenceError::Invalid)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Eindeutiger Temporaername pro Schreibvorgang: zwei Instanzen oder
    // Threads duerfen sich nie eine tmp-Inode teilen.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("tmp.{stamp}"));
    if let Err(error) = write_exclusive(&temporary, project) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn write_exclusive(temporary: &Path, project: &ProjectV1) -> Result<(), PersistenceError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)?;
    serde_json::to_writer_pretty(&mut file, project)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn writer_loop(
    path: PathBuf,
    latest: Arc<Mutex<Option<ProjectV1>>>,
    receiver: Receiver<WorkerMessage>,
) {
    while let Ok(message) = receiver.recv() {
        match message {
            WorkerMessage::Save => loop {
                match receiver.recv_timeout(SAVE_DEBOUNCE) {
                    Ok(WorkerMessage::Save) => continue,
                    Ok(WorkerMessage::Flush(reply)) => {
                        let _ = reply.send(save_latest(&path, &latest));
                        break;
                    }
                    Ok(WorkerMessage::Shutdown(reply)) => {
                        let _ = reply.send(save_latest(&path, &latest));
                        return;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        let _ = save_latest(&path, &latest);
                        break;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            },
            WorkerMessage::Flush(reply) => {
                let _ = reply.send(save_latest(&path, &latest));
            }
            WorkerMessage::Shutdown(reply) => {
                let _ = reply.send(save_latest(&path, &latest));
                return;
            }
        }
    }
}

fn save_latest(path: &Path, latest: &Mutex<Option<ProjectV1>>) -> Result<(), String> {
    let mut pending = latest.lock();
    let Some(project) = pending.clone() else {
        return Ok(());
    };
    // Fehlschlag behaelt das Pending; Flush/Shutdown melden den Fehler,
    // statt Erfolg bei nie geschriebenem Projekt zu luegen.
    save_atomic(path, &project).map_err(|error| error.to_string())?;
    *pending = None;
    Ok(())
}

fn receive_worker_result(receiver: Receiver<Result<(), String>>) -> Result<(), PersistenceError> {
    receiver
        .recv()
        .map_err(|_| PersistenceError::WriterStopped)?
        .map_err(|message| PersistenceError::Io(io::Error::other(message)))
}

fn backup_corrupt(path: &Path) -> Result<(ProjectV1, Option<PathBuf>), PersistenceError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup = path.with_extension(format!("corrupt-{stamp}.json"));
    fs::rename(path, &backup)?;
    Ok((ProjectV1::empty(), Some(backup)))
}

#[cfg(not(target_os = "windows"))]
fn replace_file(temporary: &Path, path: &Path) -> io::Result<()> {
    fs::rename(temporary, path)
}

#[cfg(target_os = "windows")]
fn replace_file(temporary: &Path, path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let succeeded = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_save_replaces_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("project.json");
        let mut project = ProjectV1::empty();
        save_atomic(&path, &project).unwrap();
        project.output.background = "#203040".into();
        save_atomic(&path, &project).unwrap();
        assert_eq!(load_or_default(&path).unwrap().0, project);
    }

    #[test]
    fn debounce_flushes_latest_project() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("project.json");
        let (store, _, _) = ProjectStore::start(path.clone()).unwrap();
        let mut first = ProjectV1::empty();
        first.output.background = "#111111".into();
        store.submit(first).unwrap();
        let mut latest = ProjectV1::empty();
        latest.output.background = "#222222".into();
        store.submit(latest.clone()).unwrap();
        store.flush().unwrap();
        assert_eq!(load_or_default(&path).unwrap().0, latest);
        store.shutdown().unwrap();
    }
}
