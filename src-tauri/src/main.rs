fn main() {
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    if arguments.next().as_deref() == Some(std::ffi::OsStr::new("--audio-watchdog")) {
        let parent = arguments
            .next()
            .and_then(|value| value.to_string_lossy().parse::<u32>().ok());
        let journal = arguments.next().map(std::path::PathBuf::from);
        if let (Some(parent), Some(journal)) = (parent, journal) {
            hooviestar_lib::run_audio_watchdog(parent, &journal);
        }
        return;
    }
    hooviestar_lib::run();
}
