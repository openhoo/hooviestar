use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use std::env;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRestoreEntry {
    pub session_instance_id: String,
    pub process_path: PathBuf,
    pub original_mute: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreJournal {
    pub entries: Vec<SessionRestoreEntry>,
}

impl RestoreJournal {
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn upsert(&mut self, entry: SessionRestoreEntry) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|existing| existing.session_instance_id == entry.session_instance_id)
        {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
    }

    pub fn remove(&mut self, session_instance_id: &str) {
        self.entries
            .retain(|entry| entry.session_instance_id != session_instance_id);
    }

    pub fn save_atomic(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Eindeutiger Sibling-Temp-Name pro Schreiber: Hauptprozess und
        // Nachlauf-Watchdog können dasselbe Journal gleichzeitig fort-
        // schreiben; ein fixer Name ließ beide dieselbe Temp-Datei
        // truncieren und halb geschriebene JSON-Bytes umbenennen. Jeder
        // Schreiber benennt jetzt seine eigenen vollständigen Bytes um.
        static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let mut temporary_name = path.as_os_str().to_os_string();
        temporary_name.push(format!(
            ".{}.{}.tmp",
            process::id(),
            TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        let temporary = PathBuf::from(temporary_name);
        let result = (|| -> io::Result<()> {
            let mut file = File::create(&temporary)?;
            serde_json::to_writer_pretty(&mut file, self)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            replace_file(&temporary, path)
        })();
        if result.is_err() {
            // Best effort: Kein Temp-Rest bei fehlgeschlagenen Saves.
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

pub fn default_journal_path() -> io::Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        });
    let base =
        base.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "state directory unavailable"))?;
    Ok(base.join("Hooviestar").join("audio-restore.json"))
}
/// Moves an unreadable journal aside as `<path>.corrupt`, replacing any earlier
/// quarantine, so the original bytes stay available for diagnosis.
pub fn quarantine_corrupt(path: &Path) -> io::Result<PathBuf> {
    let mut quarantine = PathBuf::from(path);
    quarantine.as_mut_os_string().push(".corrupt");
    match fs::remove_file(&quarantine) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(path, &quarantine)?;
    Ok(quarantine)
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

    fn restore_entry(
        session_instance_id: &str,
        process_path: &str,
        original_mute: bool,
    ) -> SessionRestoreEntry {
        SessionRestoreEntry {
            session_instance_id: session_instance_id.into(),
            process_path: process_path.into(),
            original_mute,
        }
    }

    #[test]
    fn upsert_preserves_original_session_identity() {
        let mut journal = RestoreJournal::default();
        journal.upsert(restore_entry("session-1", "game.exe", false));
        journal.upsert(restore_entry("session-1", "game.exe", true));
        assert_eq!(journal.entries.len(), 1);
        assert!(journal.entries[0].original_mute);
    }

    #[test]
    fn load_missing_file_returns_default() {
        // Frischer Pfad ohne Datei muss den Default-Journal liefern.
        let directory = tempfile::tempdir().unwrap();
        let journal = RestoreJournal::load(&directory.path().join("journal.json")).unwrap();
        assert_eq!(journal, RestoreJournal::default());
        assert!(journal.entries.is_empty());
    }

    #[test]
    fn load_corrupt_json_returns_invalid_data() {
        // Kaputte Bytes duerfen nicht stillschweigend als leerer Default
        // durchgehen, sondern muessen als InvalidData ankommen.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.json");
        fs::write(&path, b"{ not json at all").unwrap();
        let error = RestoreJournal::load(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn record_remove_round_trip() {
        // Zwei Sessions aufnehmen, eine entfernen - nur die andere bleibt.
        let mut journal = RestoreJournal::default();
        journal.upsert(restore_entry("session-1", "game.exe", true));
        journal.upsert(restore_entry("session-2", "other.exe", false));
        journal.remove("session-1");
        assert_eq!(journal.entries.len(), 1);
        assert_eq!(journal.entries[0].session_instance_id, "session-2");
        assert_eq!(journal.entries[0].process_path, PathBuf::from("other.exe"));
        assert!(!journal.entries[0].original_mute);
    }

    #[test]
    fn save_and_load_round_trip() {
        // Nicht existierender Elternordner muss beim Save angelegt werden;
        // der Zurueckles-Vergleich prueft den vollstaendigen Inhalt.
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("nested")
            .join("deep")
            .join("audio-restore.json");
        let mut journal = RestoreJournal::default();
        journal.upsert(restore_entry("session-7", "/usr/bin/game", true));
        journal.save_atomic(&path).unwrap();
        assert_eq!(RestoreJournal::load(&path).unwrap(), journal);
    }

    #[test]
    fn save_atomic_leaves_no_temp_file_on_success() {
        // Nach erfolgreichem Save liegt genau die Zieldatei im Ordner -
        // kein .tmp-Rest vom eindeutigen Sibling-Namen.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio-restore.json");
        RestoreJournal::default().save_atomic(&path).unwrap();
        let names: Vec<String> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["audio-restore.json".to_string()]);
    }

    #[test]
    fn save_atomic_failure_cleans_temporary_file() {
        // Existiert der Zielpfad als Ordner, scheitert das Umbenennen
        // (Is a directory); der Sibling-Temp muss best effort verschwinden.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio-restore.json");
        fs::create_dir(&path).unwrap();
        assert!(RestoreJournal::default().save_atomic(&path).is_err());
        let leftovers: Vec<String> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "Temp-Rest: {leftovers:?}");
    }
}
