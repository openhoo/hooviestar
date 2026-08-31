use serde::Serialize;
use std::sync::Mutex;
#[cfg(not(debug_assertions))]
use std::time::Duration;
use tauri::{AppHandle, State};
#[cfg(not(debug_assertions))]
use tauri::{Emitter, Manager};
#[cfg(not(debug_assertions))]
use tauri_plugin_updater::UpdaterExt;

#[cfg(not(debug_assertions))]
use crate::taskbar;

#[cfg(not(debug_assertions))]
const UPDATE_EVENT: &str = "updater-status";
#[cfg(not(debug_assertions))]
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(debug_assertions))]
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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

#[derive(Default)]
pub(crate) struct UpdateState(Mutex<Option<UpdateStatus>>);

impl UpdateState {
    #[cfg(any(not(debug_assertions), test))]
    fn replace_if_changed(&self, status: UpdateStatus) -> bool {
        let mut current = self.0.lock().expect("updater status mutex poisoned");
        if current.as_ref() == Some(&status) {
            return false;
        }
        *current = Some(status);
        true
    }

    fn snapshot(&self) -> Option<UpdateStatus> {
        self.0
            .lock()
            .expect("updater status mutex poisoned")
            .clone()
    }
}

#[tauri::command]
pub(crate) fn updater_status(state: State<'_, UpdateState>) -> Option<UpdateStatus> {
    state.snapshot()
}

#[cfg(not(debug_assertions))]
fn emit(app: &AppHandle, status: UpdateStatus) {
    match &status {
        UpdateStatus::Checking | UpdateStatus::Available { .. } => taskbar::update_checking(app),
        UpdateStatus::Downloading { progress, .. } => taskbar::update_progress(app, *progress),
        UpdateStatus::Installing { .. } => taskbar::update_installing(app),
        UpdateStatus::UpToDate | UpdateStatus::Installed { .. } => taskbar::update_finished(app),
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
    let updater = app.updater_builder().timeout(CHECK_TIMEOUT).build()?;
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
    let install_app = app.clone();
    let install_version = version.clone();
    let mut downloaded = 0_u64;
    update
        .download_and_install(
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
            move || {
                emit(
                    &install_app,
                    UpdateStatus::Installing {
                        version: install_version,
                    },
                );
            },
        )
        .await?;
    emit(&app, UpdateStatus::Installed { version });
    app.restart();
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
