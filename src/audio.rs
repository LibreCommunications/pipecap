//! PipeWire audio stream consumer.
//! Captures system audio from a PipeWire node as interleaved f32 PCM.
//! Uses the same approach as venmic but without creating a virtual mic —
//! we capture directly from PipeWire and return raw audio buffers.

use pipewire as pw;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

/// Raw audio buffer.
pub struct AudioBuffer {
    pub channels: u32,
    pub sample_rate: u32,
    pub data: Vec<f32>,
}

/// PipeWire audio capturer. Runs on a background thread and accumulates
/// audio samples for the caller to read.
pub struct AudioCapturer {
    buffer: Arc<Mutex<Vec<f32>>>,
    channels: Arc<Mutex<u32>>,
    sample_rate: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioCapturer {
    /// Create a new audio capturer linked to the system audio output.
    /// Excludes the given PID (our own process) to prevent feedback.
    pub fn new(exclude_pid: u32) -> anyhow::Result<Self> {
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let channels: Arc<Mutex<u32>> = Arc::new(Mutex::new(2));
        let sample_rate: Arc<Mutex<u32>> = Arc::new(Mutex::new(48000));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let buf_ref = buffer.clone();
        let ch_ref = channels.clone();
        let sr_ref = sample_rate.clone();
        let stop_ref = stop_flag.clone();

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_audio_loop(exclude_pid, buf_ref, ch_ref, sr_ref, stop_ref) {
                eprintln!("pipecap: audio capture error: {e}");
            }
        });

        Ok(AudioCapturer {
            buffer,
            channels,
            sample_rate,
            stop_flag,
            thread: Some(thread),
        })
    }

    /// Drain accumulated audio samples. Returns None if no samples yet.
    pub fn read_audio(&self) -> Option<AudioBuffer> {
        let mut lock = self.buffer.lock().ok()?;
        if lock.is_empty() {
            return None;
        }
        let data = std::mem::take(&mut *lock);
        let channels = *self.channels.lock().ok()?;
        let sample_rate = *self.sample_rate.lock().ok()?;
        Some(AudioBuffer {
            channels,
            sample_rate,
            data,
        })
    }
}

impl Drop for AudioCapturer {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn run_audio_loop(
    _exclude_pid: u32,
    buffer: Arc<Mutex<Vec<f32>>>,
    channels: Arc<Mutex<u32>>,
    sample_rate: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    // Capture system audio output
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Screen");
    // Capture from the default audio sink monitor
    props.insert("stream.capture.sink", "true");
    let stream = pw::stream::StreamBox::new(&core, "pipecap-audio", props)?;

    let buf_ref = buffer;
    let ch_ref = channels;
    let sr_ref = sample_rate;

    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(move |_stream, _user_data, id, _pod| {
            // When format is negotiated, extract channels and sample rate
            // id == SPA_PARAM_Format
            if id == libspa::param::ParamType::Format.as_raw() {
                // Default to stereo 48kHz — PipeWire usually negotiates this
                if let Ok(mut ch) = ch_ref.lock() {
                    *ch = 2;
                }
                if let Ok(mut sr) = sr_ref.lock() {
                    *sr = 48000;
                }
            }
        })
        .process(move |stream_ref, _user_data| {
            if let Some(mut buffer) = stream_ref.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if let Some(data) = datas.first_mut() {
                    let chunk = data.chunk();
                    let size = chunk.size() as usize;
                    let offset = chunk.offset() as usize;

                    if let Some(slice) = data.data() {
                        if size > 0 && offset + size <= slice.len() {
                            let audio_bytes = &slice[offset..offset + size];
                            // PipeWire delivers f32 samples by default
                            let samples: &[f32] = unsafe {
                                std::slice::from_raw_parts(
                                    audio_bytes.as_ptr() as *const f32,
                                    audio_bytes.len() / std::mem::size_of::<f32>(),
                                )
                            };
                            if let Ok(mut lock) = buf_ref.lock() {
                                lock.extend_from_slice(samples);
                                // Cap buffer at ~1 second of stereo 48kHz to prevent unbounded growth
                                const MAX_SAMPLES: usize = 48000 * 2;
                                if lock.len() > MAX_SAMPLES * 2 {
                                    let drain = lock.len() - MAX_SAMPLES;
                                    lock.drain(..drain);
                                }
                            }
                        }
                    }
                }
            }
        })
        .register()?;

    // Connect as a capture stream — PipeWire routes system audio to us
    stream.connect(
        libspa::utils::Direction::Input,
        None, // No specific node — capture default sink monitor
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [],
    )?;

    let mainloop_ptr = mainloop.as_raw_ptr();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop_flag.load(Ordering::Relaxed) {
            unsafe { pipewire_sys::pw_main_loop_quit(mainloop_ptr) };
        }
    });
    timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );

    mainloop.run();

    Ok(())
}
