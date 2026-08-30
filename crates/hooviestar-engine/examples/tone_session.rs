#[cfg(target_os = "windows")]
fn main() {
    use std::{
        f32::consts::TAU,
        thread,
        time::{Duration, Instant},
    };
    use windows::Win32::{
        Media::Audio::{
            AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioRenderClient,
            IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eConsole, eRender,
        },
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
        },
    };

    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .expect("COM initialization failed");
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .expect("audio enumerator failed");
    let endpoint = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .expect("default render endpoint failed");
    let client: IAudioClient =
        unsafe { endpoint.Activate(CLSCTX_ALL, None) }.expect("audio client activation failed");
    let format = WAVEFORMATEX {
        wFormatTag: 3,
        nChannels: 2,
        nSamplesPerSec: 48_000,
        nAvgBytesPerSec: 48_000 * 8,
        nBlockAlign: 8,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    let grouping = windows::core::GUID::from_u128(0x48564f4f_5649_4553_5441_52544f4e4501);
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            1_000_000,
            0,
            &format,
            Some(&grouping),
        )
    }
    .expect("audio client initialize failed");
    let buffer_size = unsafe { client.GetBufferSize() }.expect("buffer size failed");
    let render: IAudioRenderClient = unsafe { client.GetService() }.expect("render service failed");
    unsafe { client.Start() }.expect("audio start failed");
    println!("pid={} grouping={grouping:?}", std::process::id());
    let mut phase = 0.0f32;
    let deadline = Instant::now() + Duration::from_secs(300);
    while Instant::now() < deadline {
        let padding = unsafe { client.GetCurrentPadding() }.expect("padding failed");
        let frames = buffer_size.saturating_sub(padding);
        if frames > 0 {
            let buffer = unsafe { render.GetBuffer(frames) }.expect("render buffer failed");
            let samples = unsafe {
                std::slice::from_raw_parts_mut(buffer.cast::<f32>(), frames as usize * 2)
            };
            for frame in samples.as_chunks_mut::<2>().0 {
                let sample = phase.sin() * 0.2;
                frame[0] = sample;
                frame[1] = sample;
                phase += TAU * 440.0 / 48_000.0;
                if phase >= TAU {
                    phase -= TAU;
                }
            }
            unsafe { render.ReleaseBuffer(frames, 0) }.expect("render release failed");
        }
        thread::sleep(Duration::from_millis(5));
    }
    let _ = unsafe { client.Stop() };
    unsafe { CoUninitialize() };
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("tone_session requires Windows");
    std::process::exit(2);
}
