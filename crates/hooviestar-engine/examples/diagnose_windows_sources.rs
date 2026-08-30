#[cfg(target_os = "windows")]
fn main() {
    if let Err(error) = windows_main() {
        eprintln!("Windows source diagnosis failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
fn windows_main() -> Result<(), String> {
    use hooviestar_engine::discovery::windows::{
        enumerate_audio_sessions, enumerate_visible_windows,
    };
    use serde::Serialize;

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Report {
        windows: Vec<hooviestar_engine::discovery::SourceCandidate>,
        audio_sessions: Vec<hooviestar_engine::discovery::SourceCandidate>,
    }

    let report = Report {
        windows: enumerate_visible_windows(&[])?,
        audio_sessions: enumerate_audio_sessions()?,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("diagnose_windows_sources requires Windows");
    std::process::exit(2);
}
