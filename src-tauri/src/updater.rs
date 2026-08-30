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
const UPDATE_EVENT: &str = "updater-status";
#[cfg(not(debug_assertions))]
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(debug_assertions))]
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[cfg_attr(debug_assertions, allow(dead_code))]
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum UpdateStatus {
    Checking,
    UpToDate,
    Available { version: String },
    Downloading { version: String },
    Installed { version: String },
    Error { message: String },
}

#[derive(Default)]
pub(crate) struct UpdateState(Mutex<Option<UpdateStatus>>);

impl UpdateState {
    #[cfg(any(not(debug_assertions), test))]
    fn replace(&self, status: UpdateStatus) {
        *self.0.lock().expect("updater status mutex poisoned") = Some(status);
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
    app.state::<UpdateState>().replace(status.clone());
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
        },
    );
    update.download_and_install(|_, _| {}, || {}).await?;
    emit(&app, UpdateStatus::Installed { version });
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::UpdateStatus;
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
                version: "1.2.3".into()
            })
            .unwrap(),
            json!({ "status": "downloading", "version": "1.2.3" })
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
        state.replace(UpdateStatus::Downloading {
            version: "1.2.3".into(),
        });
        assert!(matches!(
            state.snapshot(),
            Some(UpdateStatus::Downloading { version }) if version == "1.2.3"
        ));
    }
}
