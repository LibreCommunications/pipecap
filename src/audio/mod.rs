//! PipeWire audio capture.
//!
//! Strategies:
//!   - `System`: sink monitor (all desktop audio)
//!   - `AppFromVideoNode` / `AppByName`: per-app via registry watching
//!   - `SystemExcludePids`: capture every output stream except those whose
//!     `application.process.id` is in the exclude list, and mix them. Used
//!     to avoid hearing the calling app inside its own screen-share.

pub mod app;
pub mod mix;
pub mod resolve;
pub mod system;
pub mod system_exclude;

use pipewire as pw;
use pw::spa;
use spa::pod::Pod;
use std::sync::Arc;

use crate::pw_util;
use mix::MixBuffer;

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
    AppByName(String),
    SystemExcludePids(Vec<u32>),
}

/// Internal control message sent from the controller thread to a PipeWire
/// loop running in a worker thread. Replaces the old 100ms polling timer.
pub enum AudioCtl {
    Stop,
}

pub struct AudioCapturer {
    mix: Arc<MixBuffer>,
    sender: pw::channel::Sender<AudioCtl>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioCapturer {
    pub fn new(target: AudioTarget) -> anyhow::Result<Self> {
        let mix = Arc::new(MixBuffer::new());
        let (sender, receiver) = pw::channel::channel::<AudioCtl>();

        let mix_t = mix.clone();

        let thread = std::thread::Builder::new()
            .name("pipecap-audio".into())
            .spawn(move || {
                let result = match target {
                    AudioTarget::System => system::run(mix_t, receiver),
                    AudioTarget::AppFromVideoNode(id) => app::run(id, mix_t, receiver),
                    AudioTarget::AppByName(name) => app::run_by_name(name, mix_t, receiver),
                    AudioTarget::SystemExcludePids(pids) => {
                        system_exclude::run(pids, mix_t, receiver)
                    }
                };
                if let Err(e) = result {
                    eprintln!("pipecap-audio: error: {e}");
                }
            })?;

        Ok(Self {
            mix,
            sender,
            thread: Some(thread),
        })
    }

    pub fn read_audio(&self) -> Option<AudioBuffer> {
        self.mix.drain()
    }

    /// Single-source synthetic id for non-mix modes.
    pub const SINGLE_SOURCE: u32 = 0;
}

impl Drop for AudioCapturer {
    fn drop(&mut self) {
        let _ = self.sender.send(AudioCtl::Stop);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
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
    let Some(pod) = Pod::from_bytes(&values) else {
        eprintln!("pipecap-audio: failed to build format pod");
        return;
    };
    let mut params = [pod];
    if let Err(e) = stream.connect(spa::utils::Direction::Input, node_id, STREAM_FLAGS, &mut params)
    {
        eprintln!("pipecap-audio: connect error: {e}");
    }
}

/// Reinterpret a PipeWire byte chunk as `&[f32]` safely. Returns `None` if
/// the bytes are not 4-byte aligned, in which case the chunk is dropped.
/// f32 has no invalid bit patterns so the only soundness requirement is
/// alignment, which `align_to` validates at runtime.
pub fn bytes_as_f32(bytes: &[u8]) -> Option<&[f32]> {
    let (head, mid, tail) = unsafe { bytes.align_to::<f32>() };
    if !head.is_empty() || !tail.is_empty() {
        return None;
    }
    Some(mid)
}
