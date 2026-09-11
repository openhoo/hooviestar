use serde::Serialize;
#[cfg(not(debug_assertions))]
use sha2::{Digest, Sha256};
use std::sync::Mutex;
#[cfg(not(debug_assertions))]
use std::time::Duration;
#[cfg(not(debug_assertions))]
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
};
use tauri::{AppHandle, Emitter, Manager, State};
#[cfg(not(debug_assertions))]
use tauri_plugin_updater::UpdaterExt;
#[cfg(not(debug_assertions))]
use tempfile::tempfile_in;

#[cfg(not(debug_assertions))]
use crate::taskbar;

const UPDATE_EVENT: &str = "updater-status";
#[cfg(not(debug_assertions))]
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(debug_assertions))]
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
#[cfg(not(debug_assertions))]
enum PreparedBytesError {
    Read(String),
    Integrity(String),
}

#[cfg(not(debug_assertions))]
impl PreparedBytesError {
    fn message(self) -> String {
        match self {
            Self::Read(message) | Self::Integrity(message) => message,
        }
    }

    fn is_integrity(&self) -> bool {
        matches!(self, Self::Integrity(_))
    }
}

#[cfg(not(debug_assertions))]
struct PreparedUpdate {
    update: Box<tauri_plugin_updater::Update>,
    artifact: File,
    length: u64,
    digest: [u8; 32],
}

#[cfg(not(debug_assertions))]
impl PreparedUpdate {
    fn read_verified_bytes(&mut self) -> Result<Vec<u8>, PreparedBytesError> {
        self.artifact.seek(SeekFrom::Start(0)).map_err(|error| {
            PreparedBytesError::Read(format!(
                "Vorbereitete Aktualisierung konnte nicht gelesen werden: {error}"
            ))
        })?;
        let limit = self.length.checked_add(1).ok_or_else(|| {
            PreparedBytesError::Integrity("Vorbereitete Aktualisierung ist zu groß".to_string())
        })?;
        let capacity = usize::try_from(self.length).map_err(|_| {
            PreparedBytesError::Integrity("Vorbereitete Aktualisierung ist zu groß".to_string())
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        Read::by_ref(&mut self.artifact)
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                PreparedBytesError::Read(format!(
                    "Vorbereitete Aktualisierung konnte nicht gelesen werden: {error}"
                ))
            })?;
        if bytes.len() as u64 != self.length {
            return Err(PreparedBytesError::Integrity(
                "Vorbereitete Aktualisierung ist unvollständig".to_string(),
            ));
        }
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if digest != self.digest {
            return Err(PreparedBytesError::Integrity(
                "Vorbereitete Aktualisierung hat die Integritätsprüfung nicht bestanden"
                    .to_string(),
            ));
        }
        Ok(bytes)
    }
}

#[cfg_attr(debug_assertions, allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum UpdateStatus {
    Checking,
    UpToDate,
    Available {
        version: String,
    },
    Downloading {
        version: String,
        progress: Option<u8>,
    },
    Ready {
        version: String,
    },
    Installing {
        version: String,
    },
    Installed {
        version: String,
    },
    Error {
        message: String,
    },
}

#[cfg(not(debug_assertions))]
#[derive(Default)]
enum PendingUpdate {
    #[default]
    Empty,
    Ready(PreparedUpdate),
    Installing,
}

#[derive(Default)]
pub(crate) struct UpdateState {
    status: Mutex<Option<UpdateStatus>>,
    #[cfg(not(debug_assertions))]
    pending: Mutex<PendingUpdate>,
}

#[cfg(not(debug_assertions))]
enum ClaimError {
    Empty,
    Busy,
}

impl UpdateState {
    fn replace_if_changed(&self, status: UpdateStatus) -> bool {
        let mut current = self.status.lock().expect("updater status mutex poisoned");
        if current.as_ref() == Some(&status) {
            return false;
        }
        *current = Some(status);
        true
    }

    fn snapshot(&self) -> Option<UpdateStatus> {
        self.status
            .lock()
            .expect("updater status mutex poisoned")
            .clone()
    }

    #[cfg(not(debug_assertions))]
    fn store_pending(&self, prepared: PreparedUpdate) -> Result<(), PreparedUpdate> {
        let mut pending = self.pending.lock().expect("updater state mutex poisoned");
        if !matches!(&*pending, PendingUpdate::Empty) {
            return Err(prepared);
        }
        *pending = PendingUpdate::Ready(prepared);
        Ok(())
    }

    #[cfg(not(debug_assertions))]
    fn claim_pending(&self) -> Result<PreparedUpdate, ClaimError> {
        let mut pending = self.pending.lock().expect("updater state mutex poisoned");
        match std::mem::replace(&mut *pending, PendingUpdate::Installing) {
            PendingUpdate::Ready(prepared) => Ok(prepared),
            PendingUpdate::Empty => {
                *pending = PendingUpdate::Empty;
                Err(ClaimError::Empty)
            }
            PendingUpdate::Installing => {
                *pending = PendingUpdate::Installing;
                Err(ClaimError::Busy)
            }
        }
    }

    #[cfg(not(debug_assertions))]
    fn restore_claim(&self, prepared: PreparedUpdate) {
        let mut pending = self.pending.lock().expect("updater state mutex poisoned");
        if matches!(&*pending, PendingUpdate::Installing) {
            *pending = PendingUpdate::Ready(prepared);
        }
    }

    #[cfg(not(debug_assertions))]
    fn finish_claim(&self) {
        let mut pending = self.pending.lock().expect("updater state mutex poisoned");
        if matches!(&*pending, PendingUpdate::Installing) {
            *pending = PendingUpdate::Empty;
        }
    }
}

#[tauri::command]
pub(crate) fn updater_status(state: State<'_, UpdateState>) -> Option<UpdateStatus> {
    state.snapshot()
}

fn emit(app: &AppHandle, status: UpdateStatus) {
    #[cfg(not(debug_assertions))]
    match &status {
        UpdateStatus::Checking | UpdateStatus::Available { .. } => taskbar::update_checking(app),
        UpdateStatus::Downloading { progress, .. } => taskbar::update_progress(app, *progress),
        UpdateStatus::Ready { .. } | UpdateStatus::UpToDate | UpdateStatus::Installed { .. } => {
            taskbar::update_finished(app)
        }
        UpdateStatus::Installing { .. } => taskbar::update_installing(app),
        UpdateStatus::Error { .. } => taskbar::update_failed(app),
    }
    if !app
        .state::<UpdateState>()
        .replace_if_changed(status.clone())
    {
        return;
    }
    if let Err(error) = app.emit(UPDATE_EVENT, status) {
        eprintln!("[hooviestar] failed to emit updater status: {error}");
    }
}

#[tauri::command]
pub(crate) async fn install_update(
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        let _ = state;
        let message = "Keine vorbereitete Aktualisierung verfügbar".to_string();
        emit(
            &app,
            UpdateStatus::Error {
                message: message.clone(),
            },
        );
        return Err(message);
    }

    #[cfg(not(debug_assertions))]
    {
        let mut prepared = match state.claim_pending() {
            Ok(prepared) => prepared,
            Err(ClaimError::Empty) => {
                let message = "Keine vorbereitete Aktualisierung verfügbar".to_string();
                emit(
                    &app,
                    UpdateStatus::Error {
                        message: message.clone(),
                    },
                );
                return Err(message);
            }
            Err(ClaimError::Busy) => {
                return Err("Aktualisierung wird bereits installiert".to_string());
            }
        };
        let version = prepared.update.version.clone();
        let bytes = match prepared.read_verified_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                let integrity_failure = error.is_integrity();
                let message = error.message();
                if integrity_failure {
                    state.finish_claim();
                    emit(
                        &app,
                        UpdateStatus::Error {
                            message: message.clone(),
                        },
                    );
                } else {
                    state.restore_claim(prepared);
                    emit(
                        &app,
                        UpdateStatus::Ready {
                            version: version.clone(),
                        },
                    );
                }
                return Err(message);
            }
        };
        let resources = match app.try_state::<std::sync::Arc<crate::RuntimeResources>>() {
            Some(resources) => resources,
            None => {
                let message = "Updater-Laufzeitstatus ist nicht verfügbar".to_string();
                state.restore_claim(prepared);
                emit(
                    &app,
                    UpdateStatus::Ready {
                        version: version.clone(),
                    },
                );
                return Err(message);
            }
        };
        if let Err(error) = resources.inner().flush_for_updater() {
            let message =
                format!("Projekt konnte vor der Installation nicht gespeichert werden: {error}");
            state.restore_claim(prepared);
            emit(
                &app,
                UpdateStatus::Ready {
                    version: version.clone(),
                },
            );
            return Err(message);
        }
        emit(
            &app,
            UpdateStatus::Installing {
                version: version.clone(),
            },
        );
        match prepared.update.install(&bytes) {
            Ok(()) => {
                state.finish_claim();
                drop(prepared);
                emit(&app, UpdateStatus::Installed { version });
                app.restart()
            }
            Err(error) => {
                let message = format!("Aktualisierung konnte nicht gestartet werden: {error}");
                state.restore_claim(prepared);
                emit(
                    &app,
                    UpdateStatus::Ready {
                        version: version.clone(),
                    },
                );
                Err(message)
            }
        }
    }
}

#[cfg(not(debug_assertions))]
fn prepare_verified_update(
    app: &AppHandle,
    update: tauri_plugin_updater::Update,
    bytes: Vec<u8>,
) -> tauri_plugin_updater::Result<PreparedUpdate> {
    let cache_dir = app.path().app_cache_dir()?;
    fs::create_dir_all(&cache_dir)?;
    let mut artifact = tempfile_in(cache_dir)?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    artifact.write_all(&bytes)?;
    artifact.sync_all()?;
    artifact.seek(SeekFrom::Start(0))?;
    Ok(PreparedUpdate {
        update: Box::new(update),
        artifact,
        length: bytes.len() as u64,
        digest,
    })
}

#[cfg(not(debug_assertions))]
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = update(app.clone()).await {
            eprintln!("[hooviestar] automatic update failed: {error}");
            emit(
                &app,
                UpdateStatus::Error {
                    message: error.to_string(),
                },
            );
        }
    });
}

#[cfg(debug_assertions)]
pub fn spawn(_app: AppHandle) {}

#[cfg(not(debug_assertions))]
async fn update(app: AppHandle) -> tauri_plugin_updater::Result<()> {
    emit(&app, UpdateStatus::Checking);
    let hook_app = app.clone();
    let updater = app
        .updater_builder()
        .timeout(CHECK_TIMEOUT)
        // Windows installers can terminate the process directly, before
        // Tauri emits RunEvent::Exit. Flush only: this hook runs before the
        // installer is launched and an installation error must leave the
        // running engine usable.
        .on_before_exit(move || {
            let resources = hook_app.state::<std::sync::Arc<crate::RuntimeResources>>();
            if let Err(error) = resources.flush_for_updater() {
                eprintln!("[hooviestar] updater pre-exit project flush failed: {error}");
            }
        })
        .build()?;
    let Some(mut update) = updater.check().await? else {
        emit(&app, UpdateStatus::UpToDate);
        return Ok(());
    };

    update.timeout = Some(DOWNLOAD_TIMEOUT);
    let version = update.version.clone();
    emit(
        &app,
        UpdateStatus::Available {
            version: version.clone(),
        },
    );
    emit(
        &app,
        UpdateStatus::Downloading {
            version: version.clone(),
            progress: None,
        },
    );
    let download_app = app.clone();
    let download_version = version.clone();
    let mut downloaded = 0_u64;
    let bytes = update
        .download(
            move |chunk_length, content_length| {
                downloaded = downloaded.saturating_add(chunk_length as u64);
                emit(
                    &download_app,
                    UpdateStatus::Downloading {
                        version: download_version.clone(),
                        progress: download_percentage(downloaded, content_length),
                    },
                );
            },
            || {},
        )
        .await?;

    let prepared = prepare_verified_update(&app, update, bytes)?;
    if let Err(prepared) = app.state::<UpdateState>().store_pending(prepared) {
        drop(prepared);
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Es ist bereits eine Aktualisierung vorbereitet",
        )
        .into());
    }
    emit(&app, UpdateStatus::Ready { version });
    Ok(())
}

#[cfg(any(not(debug_assertions), test))]
fn download_percentage(downloaded: u64, content_length: Option<u64>) -> Option<u8> {
    let total = content_length.filter(|total| *total > 0)?;
    Some((u128::from(downloaded.min(total)) * 100 / u128::from(total)) as u8)
}

#[cfg(test)]
mod tests {
    use super::{UpdateStatus, download_percentage};
    use serde_json::json;

    #[test]
    fn status_contract_uses_stable_tagged_payloads() {
        assert_eq!(
            serde_json::to_value(UpdateStatus::Checking).unwrap(),
            json!({ "status": "checking" })
        );
        assert_eq!(
            serde_json::to_value(UpdateStatus::Available {
                version: "1.2.3".into()
            })
            .unwrap(),
            json!({ "status": "available", "version": "1.2.3" })
        );
        assert_eq!(
            serde_json::to_value(UpdateStatus::Error {
                message: "offline".into()
            })
            .unwrap(),
            json!({ "status": "error", "message": "offline" })
        );
        assert_eq!(
            serde_json::to_value(UpdateStatus::UpToDate).unwrap(),
            json!({ "status": "up_to_date" })
        );
        assert_eq!(
            serde_json::to_value(UpdateStatus::Downloading {
                version: "1.2.3".into(),
                progress: Some(42),
            })
            .unwrap(),
            json!({ "status": "downloading", "version": "1.2.3", "progress": 42 })
        );
        assert_eq!(
            serde_json::to_value(UpdateStatus::Installing {
                version: "1.2.3".into()
            })
            .unwrap(),
            json!({ "status": "installing", "version": "1.2.3" })
        );
        assert_eq!(
            serde_json::to_value(UpdateStatus::Installed {
                version: "1.2.3".into()
            })
            .unwrap(),
            json!({ "status": "installed", "version": "1.2.3" })
        );

        let state = super::UpdateState::default();
        assert!(state.snapshot().is_none());
        assert!(state.replace_if_changed(UpdateStatus::Downloading {
            version: "1.2.3".into(),
            progress: Some(10),
        }));
        assert!(!state.replace_if_changed(UpdateStatus::Downloading {
            version: "1.2.3".into(),
            progress: Some(10),
        }));
        assert!(matches!(
            state.snapshot(),
            Some(UpdateStatus::Downloading { version, progress: Some(10) }) if version == "1.2.3"
        ));
    }

    #[test]
    fn download_percentage_handles_unknown_zero_and_overrun_lengths() {
        assert_eq!(download_percentage(5, None), None);
        assert_eq!(download_percentage(5, Some(0)), None);
        assert_eq!(download_percentage(50, Some(200)), Some(25));
        assert_eq!(download_percentage(250, Some(200)), Some(100));
        assert_eq!(download_percentage(u64::MAX, Some(1)), Some(100));
        assert_eq!(download_percentage(u64::MAX, Some(u64::MAX)), Some(100));
    }
}
