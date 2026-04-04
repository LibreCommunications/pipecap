//! PipeWire audio stream consumer.
//! Captures system audio output as interleaved f32 PCM via PipeWire's
//! sink monitor. Filters out the calling process to prevent feedback.

use pipewire as pw;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

pub struct AudioBuffer {
    pub channels: u32,
    pub sample_rate: u32,
    pub data: Vec<f32>,
}

pub struct AudioCapturer {
    buffer: Arc<Mutex<Vec<f32>>>,
    channels: Arc<Mutex<u32>>,
    sample_rate: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioCapturer {
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

    pub fn is_active(&self) -> bool {
        !self.stop_flag.load(Ordering::Relaxed)
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
    exclude_pid: u32,
    buffer: Arc<Mutex<Vec<f32>>>,
    _channels: Arc<Mutex<u32>>,
    _sample_rate: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Screen");
    props.insert("stream.capture.sink", "true");
    // Exclude our own process audio to prevent feedback loop
    if exclude_pid > 0 {
        props.insert("stream.dont-remix", "true");
        props.insert(
            "node.exclude-from-capture.pids",
            &*exclude_pid.to_string(),
        );
    }
    let stream = pw::stream::StreamBox::new(&core, "pipecap-audio", props)?;

    let buf_ref = buffer;

    let _listener = stream
        .add_local_listener_with_user_data(())
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
                            let samples: &[f32] = unsafe {
                                std::slice::from_raw_parts(
                                    audio_bytes.as_ptr() as *const f32,
                                    audio_bytes.len() / std::mem::size_of::<f32>(),
                                )
                            };
                            if let Ok(mut lock) = buf_ref.lock() {
                                lock.extend_from_slice(samples);
                                // Cap at ~2 seconds of stereo 48kHz
                                const MAX_SAMPLES: usize = 48000 * 2 * 2;
                                if lock.len() > MAX_SAMPLES {
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

    stream.connect(
        libspa::utils::Direction::Input,
        None,
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
