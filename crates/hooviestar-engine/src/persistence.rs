use std::{
    env,
    ffi::OsString,
    fs::{self},
    io::{self, Write},
    path::{Path, PathBuf},
    process,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;

use crate::project::ProjectV1;

const SAVE_DEBOUNCE: Duration = Duration::from_millis(250);
static SIBLING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
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
    let (config_dir, home) = (env::var_os("APPDATA"), None::<OsString>);
    #[cfg(not(target_os = "windows"))]
    let (config_dir, home) = (env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"));
    Ok(project_path_from(project_base_from(config_dir, home)?))
}

// Rein logischer Teil: festes Relativlayout unter dem Basisverzeichnis.
// Von der Umgebung entkoppelt, damit die Pfadlogik ohne
// env-Manipulation testbar bleibt.
fn project_path_from(base: PathBuf) -> PathBuf {
    #[cfg(target_os = "windows")]
    let path = base.join("Hooviestar").join("project.json");
    #[cfg(not(target_os = "windows"))]
    let path = base.join("hooviestar").join("project.json");
    path
}

// Reine Vorauswahl des Basisverzeichnisses: explizites
// Konfigurationsverzeichnis gewinnt vor dem HOME-Fallback (.config),
// fehlt beides -> NotFound. Im Produkt stammen beide Werte aus var_os,
// Tests uebergeben sie direkt als Werte.
fn project_base_from(
    config_dir: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, PersistenceError> {
    config_dir
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| {
            PersistenceError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "configuration directory unavailable",
            ))
        })
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
    let temporary = unique_sibling(path, "tmp", "");
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
    // Debouncete Saves verschlucken ihren Fehler sonst fuer immer; der
    // Streak meldet den ersten Fehlschlag nach Erfolg und die Erholung.
    let mut autosave_failing = false;
    while let Ok(message) = receiver.recv() {
        match message {
            WorkerMessage::Save => {
                // Feste Deadline statt neues Fenster je Nachricht: das erste
                // Save oeffnet das Fenster, weitere Saves waehrend des
                // Fensters koaleszieren ohne die Frist zu verschieben -
                // sonst verhungern Schreibvorgaenge bei dauerhaften
                // Aenderungen (Drag, Volumen) bis zum naechsten Stillstand.
                let deadline = Instant::now() + SAVE_DEBOUNCE;
                loop {
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .unwrap_or(Duration::ZERO);
                    match receiver.recv_timeout(remaining) {
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
                            match save_latest(&path, &latest) {
                                Ok(()) => {
                                    if autosave_failing {
                                        eprintln!("autosave recovered: {}", path.display());
                                        autosave_failing = false;
                                    }
                                }
                                Err(error) => {
                                    if !autosave_failing {
                                        eprintln!("autosave failed: {error}");
                                        autosave_failing = true;
                                    }
                                }
                            }
                            break;
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            // Sender weg ohne Shutdown oder Flush: Pending
                            // trotzdem best-effort sichern.
                            let _ = save_latest(&path, &latest);
                            return;
                        }
                    }
                }
            }
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
    // Pending unter kurzem Guard entnehmen: Die IO-Phase laeuft ohne Lock,
    // damit Submit und Transition nicht hinter Dateisystem-Latenz warten.
    let Some(project) = latest.lock().take() else {
        return Ok(());
    };
    // Fehlschlag behaelt das Pending, sofern zwischenzeitlich kein neueres
    // eingetroffen ist; Flush/Shutdown melden den Fehler, statt Erfolg bei
    // nie geschriebenem Projekt zu luegen.
    match save_atomic(path, &project).map_err(|error| error.to_string()) {
        Ok(()) => Ok(()),
        Err(error) => {
            let mut pending = latest.lock();
            if pending.is_none() {
                *pending = Some(project);
            }
            Err(error)
        }
    }
}

fn receive_worker_result(receiver: Receiver<Result<(), String>>) -> Result<(), PersistenceError> {
    receiver
        .recv()
        .map_err(|_| PersistenceError::WriterStopped)?
        .map_err(|message| PersistenceError::Io(io::Error::other(message)))
}

/// Generates a collision-resistant sibling name for files created by this
/// process. Nanoseconds make names useful during diagnosis; PID plus a
/// process-local sequence keep concurrent saves and rapid corruptions apart
/// even on filesystems or clocks with coarse resolution.
fn unique_sibling(path: &Path, label: &str, suffix: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = SIBLING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!(
        "{label}.{stamp}.{}.{}{suffix}",
        process::id(),
        sequence,
    ))
}

fn backup_corrupt(path: &Path) -> Result<(ProjectV1, Option<PathBuf>), PersistenceError> {
    let backup = unique_sibling(path, "corrupt", ".json");
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

    fn temp_project_path() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("project.json");
        (directory, path)
    }

    #[test]
    fn second_save_replaces_existing_file() {
        let (_directory, path) = temp_project_path();
        let mut project = ProjectV1::empty();
        save_atomic(&path, &project).unwrap();
        project.output.background = "#203040".into();
        save_atomic(&path, &project).unwrap();
        assert_eq!(load_or_default(&path).unwrap().0, project);
    }

    #[test]
    fn debounce_flushes_latest_project() {
        let (_directory, path) = temp_project_path();
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

    #[test]
    fn drop_without_shutdown_persists_pending() {
        let (_directory, path) = temp_project_path();
        let (store, _, _) = ProjectStore::start(path.clone()).unwrap();
        let mut submitted = ProjectV1::empty();
        submitted.output.background = "#424242".into();
        store.submit(submitted.clone()).unwrap();
        // Absturz waehrend des Debounce-Fensters simulieren: kein flush(),
        // kein shutdown() - der Writer muss das Pending im
        // Disconnected-Zweig trotzdem best-effort sichern.
        drop(store);
        let deadline = Instant::now() + SAVE_DEBOUNCE + Duration::from_millis(2000);
        loop {
            if let Ok((persisted, _)) = load_or_default(&path)
                && persisted.output.background == submitted.output.background
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "pending project not persisted after drop without shutdown"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn shutdown_persists_pending_without_flush() {
        let (_directory, path) = temp_project_path();
        let (store, _, _) = ProjectStore::start(path.clone()).unwrap();
        let mut submitted = ProjectV1::empty();
        submitted.output.background = "#333642".into();
        store.submit(submitted.clone()).unwrap();
        store.shutdown().unwrap();
        assert_eq!(load_or_default(&path).unwrap().0, submitted);
    }

    #[test]
    fn project_path_appends_fixed_layout() {
        // Reine Join-Logik, keine Umgebung noetig.
        #[cfg(target_os = "windows")]
        assert_eq!(
            project_path_from(PathBuf::from("C:\\Config")),
            PathBuf::from("C:\\Config\\Hooviestar\\project.json")
        );
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            project_path_from(PathBuf::from("/etc")),
            PathBuf::from("/etc/hooviestar/project.json")
        );
    }

    #[test]
    fn project_base_prefers_explicit_config_dir_over_home() {
        let base = project_base_from(Some("/xdg".into()), Some("/home/nutzer".into())).unwrap();
        assert_eq!(base, PathBuf::from("/xdg"));
    }

    #[test]
    fn project_base_falls_back_to_home_config() {
        let base = project_base_from(None, Some("/home/nutzer".into())).unwrap();
        assert_eq!(base, PathBuf::from("/home/nutzer/.config"));
    }

    #[test]
    fn project_base_missing_both_is_not_found() {
        let error = project_base_from(None, None).expect_err("beide fehlen muss fehlschlagen");
        match error {
            PersistenceError::Io(error) => {
                assert_eq!(error.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("unerwarteter Fehler: {other:?}"),
        }
    }

    #[test]
    fn project_base_empty_value_counts_as_set() {
        // var_os liefert Some("") fuer leere Variablen - genau so wie
        // bisher behandeln: als gesetzt, nicht als fehlend.
        let base = project_base_from(Some("".into()), Some("/home/nutzer".into())).unwrap();
        assert_eq!(base, PathBuf::from(""));
    }

    #[test]
    fn corrupt_project_is_backed_up_and_reset() {
        let (_directory, path) = temp_project_path();
        // empty() erzeugt frische UUIDs - Struktur statt Wertgleichheit.
        let first_corruption = b"{ keine gueltigen Projektdaten";
        fs::write(&path, first_corruption).unwrap();
        let (project, backup) = load_or_default(&path).unwrap();
        assert!(project.validate().is_ok());
        assert!(project.sources.is_empty());
        let first_backup = backup.expect("korrupte Datei muss gesichert werden");
        assert!(first_backup.is_file());
        assert_eq!(fs::read(&first_backup).unwrap(), first_corruption);
        assert!(!path.exists());

        // Auch ProjectStore::start muss ueber einer korrupten Datei sauber
        // auf das Default-Projekt umschalten und die Reste sichern.
        let second_corruption = b"{ immer noch kein Projekt";
        fs::write(&path, second_corruption).unwrap();
        let (store, started, backup) = ProjectStore::start(path.clone()).unwrap();
        assert!(started.validate().is_ok());
        assert!(started.sources.is_empty());
        let second_backup = backup.expect("zweiter Korruptionsfall muss gesichert werden");
        assert!(second_backup.is_file());
        assert_ne!(first_backup, second_backup);
        assert_eq!(fs::read(&first_backup).unwrap(), first_corruption);
        assert_eq!(fs::read(&second_backup).unwrap(), second_corruption);
        store.shutdown().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn readonly_parent_leaves_no_temporary_behind() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, path) = temp_project_path();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let result = save_atomic(&path, &ProjectV1::empty());
        // Rechte sofort zuruecksetzen, damit das Tempdir aufgeraeumt
        // werden kann, egal wie die Assertions ausgehen.
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let error = result.expect_err("schreibgeschuetztes Verzeichnis muss fehlschlagen");
        assert!(matches!(error, PersistenceError::Io(_)));
        let leftovers: Vec<String> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.is_empty(),
            "Tempdateien nach Fehler uebrig: {leftovers:?}"
        );
    }

    #[test]
    fn failed_rename_cleans_up_temporary() {
        let (directory, path) = temp_project_path();
        // Ziel existiert als Verzeichnis: rename(Datei -> Verzeichnis)
        // schlaegt mit EISDIR fehl, die Tempdatei muss entfernt werden.
        fs::create_dir(&path).unwrap();
        let error = save_atomic(&path, &ProjectV1::empty())
            .expect_err("rename auf Verzeichnis muss fehlschlagen");
        assert!(matches!(error, PersistenceError::Io(_)));
        let leftovers: Vec<String> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "Tempdateien nach Fehler uebrig: {leftovers:?}"
        );
    }

    #[test]
    fn shutdown_twice_and_submit_after_shutdown_are_noops() {
        let (_directory, path) = temp_project_path();
        let (store, _, _) = ProjectStore::start(path.clone()).unwrap();
        let mut submitted = ProjectV1::empty();
        submitted.output.background = "#050505".into();
        store.submit(submitted.clone()).unwrap();
        store.shutdown().unwrap();
        // Zweites shutdown: Worker bereits aufgeraeumt -> fruehes Ok.
        store.shutdown().unwrap();
        // Nach geschlossenem Store wird der Submit still verworfen,
        // die Datei bleibt beim zuletzt gesicherten Stand.
        let mut late = ProjectV1::empty();
        late.output.background = "#999999".into();
        store.submit(late).unwrap();
        assert_eq!(load_or_default(&path).unwrap().0, submitted);
    }
}
