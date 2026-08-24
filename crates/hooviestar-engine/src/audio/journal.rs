use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
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
        let temporary = path.with_extension("tmp");
        let mut file = File::create(&temporary)?;
        serde_json::to_writer_pretty(&mut file, self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
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
    fn upsert_preserves_original_session_identity() {
        let mut journal = RestoreJournal::default();
        journal.upsert(SessionRestoreEntry {
            session_instance_id: "session-1".into(),
            process_path: "game.exe".into(),
            original_mute: false,
        });
        journal.upsert(SessionRestoreEntry {
            session_instance_id: "session-1".into(),
            process_path: "game.exe".into(),
            original_mute: true,
        });
        assert_eq!(journal.entries.len(), 1);
        assert!(journal.entries[0].original_mute);
    }
}
