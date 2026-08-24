#[cfg(target_os = "windows")]
fn main() {
    use hooviestar_engine::video::windows::{D3d11Device, SceneTextureDescriptor};

    let device = D3d11Device::create_hardware().expect("hardware D3D11 device creation failed");
    let _scene = device
        .create_scene_texture(SceneTextureDescriptor::float16(1920, 1080))
        .expect("Float16 scene texture creation failed");
    println!(
        "D3D11 hardware device and 1920x1080 Float16 scene texture ready: {:?}",
        device.feature_level
    );
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("qualify_d3d11 requires Windows");
    std::process::exit(2);
}
