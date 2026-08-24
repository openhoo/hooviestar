#[cfg(target_os = "linux")]
fn main() {
    let candidates = hooviestar_engine::discovery::linux::enumerate_audio_nodes()
        .expect("PipeWire audio discovery failed");
    println!(
        "{}",
        serde_json::to_string_pretty(&candidates).expect("serialize audio candidates")
    );
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qualify_pipewire_audio requires Linux");
    std::process::exit(2);
}
