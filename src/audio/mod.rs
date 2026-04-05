//! PipeWire audio capture.
//!
//! Three strategies:
//!   - `system`: sink monitor (all desktop audio)
//!   - `app`: per-app via registry watching + session matcher
//!   - (future) fallback when app can't be identified

pub mod app;
pub mod resolve;
pub mod system;

use pipewire as pw;
use pw::spa;
use spa::pod::Pod;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use crate::pw_util;

pub const MAX_SAMPLES: usize = 48000 * 2 * 2;

pub const STREAM_FLAGS: pw::stream::StreamFlags = pw::stream::StreamFlags::from_bits_truncate(
    pw::stream::StreamFlags::AUTOCONNECT.bits()
        | pw::stream::StreamFlags::MAP_BUFFERS.bits()
        | pw::stream::StreamFlags::RT_PROCESS.bits(),
);

pub struct AudioBuffer {
    pub channels: u32,
    pub sample_rate: u32,
    pub data: Vec<f32>,
}

pub enum AudioTarget {
    System,
    AppFromVideoNode(u32),
}

pub struct AudioCapturer {
    buffer: Arc<Mutex<Vec<f32>>>,
    channels: Arc<Mutex<u32>>,
    sample_rate: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioCapturer {
    pub fn new(target: AudioTarget) -> anyhow::Result<Self> {
        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let channels = Arc::new(Mutex::new(2u32));
        let sample_rate = Arc::new(Mutex::new(48000u32));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let (buf, ch, sr, stop) = (
            buffer.clone(), channels.clone(), sample_rate.clone(), stop_flag.clone(),
        );

        let thread = std::thread::spawn(move || {
            let result = match target {
                AudioTarget::System => system::run(buf, ch, sr, stop),
                AudioTarget::AppFromVideoNode(id) => app::run(id, buf, ch, sr, stop),
            };
            if let Err(e) = result {
                eprintln!("pipecap-audio: error: {e}");
            }
        });

        Ok(Self { buffer, channels, sample_rate, stop_flag, thread: Some(thread) })
    }

    pub fn read_audio(&self) -> Option<AudioBuffer> {
        let mut lock = self.buffer.lock().ok()?;
        if lock.is_empty() { return None; }
        let data = std::mem::take(&mut *lock);
        Some(AudioBuffer {
            channels: *self.channels.lock().ok()?,
            sample_rate: *self.sample_rate.lock().ok()?,
            data,
        })
    }
}

impl Drop for AudioCapturer {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() { let _ = h.join(); }
    }
}

// ── Shared helpers used by system.rs and app.rs ────

pub fn audio_format_params() -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    pw_util::serialize_pod_object(spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    })
}

pub fn connect_stream_to(stream: &pw::stream::StreamRc, node_id: Option<u32>) {
    let _ = stream.disconnect();
    let values = audio_format_params();
    let mut params = [Pod::from_bytes(&values).unwrap()];
    if let Err(e) = stream.connect(spa::utils::Direction::Input, node_id, STREAM_FLAGS, &mut params) {
        eprintln!("pipecap-audio: connect error: {e}");
    }
}
