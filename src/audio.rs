//! PipeWire audio stream consumer.
//! Captures system audio output as interleaved f32 PCM.
//! Based on pipewire-rs audio-capture.rs example.

use pipewire as pw;
use pw::spa;
use spa::pod::Pod;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
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
    pub fn new(_exclude_pid: u32) -> anyhow::Result<Self> {
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let channels: Arc<Mutex<u32>> = Arc::new(Mutex::new(2));
        let sample_rate: Arc<Mutex<u32>> = Arc::new(Mutex::new(48000));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let buf_ref = buffer.clone();
        let ch_ref = channels.clone();
        let sr_ref = sample_rate.clone();
        let stop_ref = stop_flag.clone();

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_audio_loop(buf_ref, ch_ref, sr_ref, stop_ref) {
                eprintln!("pipecap-audio: capture error: {e}");
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
    buffer: Arc<Mutex<Vec<f32>>>,
    channels_out: Arc<Mutex<u32>>,
    sample_rate_out: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;
    eprintln!("pipecap-audio: connected to PipeWire");

    // Capture from the default audio sink's monitor ports (system audio output)
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Music");
    props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");

    let stream = pw::stream::StreamBox::new(&core, "pipecap-audio", props)?;

    let buf_ref = buffer;
    let ch_out = channels_out;
    let sr_out = sample_rate_out;
    let audio_count = Arc::new(AtomicU64::new(0));
    let ac = audio_count.clone();

    let _listener = stream
        .add_local_listener_with_user_data(spa::param::audio::AudioInfoRaw::default())
        .param_changed(move |_, user_data, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() { return; }

            if let Ok((mt, ms)) = spa::param::format_utils::parse_format(param) {
                if mt != spa::param::format::MediaType::Audio
                    || ms != spa::param::format::MediaSubtype::Raw
                {
                    return;
                }
            }

            if user_data.parse(param).is_ok() {
                let ch = user_data.channels();
                let rate = user_data.rate();
                eprintln!("pipecap-audio: negotiated {ch}ch {rate}Hz");
                if let Ok(mut c) = ch_out.lock() { *c = ch; }
                if let Ok(mut r) = sr_out.lock() { *r = rate; }
            }
        })
        .process(move |stream_ref, _user_data| {
            let n = ac.fetch_add(1, Ordering::Relaxed);
            if let Some(mut buffer) = stream_ref.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if let Some(data) = datas.first_mut() {
                    let chunk = data.chunk();
                    let size = chunk.size() as usize;

                    if n < 3 {
                        eprintln!("pipecap-audio: frame #{n} size={size}");
                    }

                    if let Some(samples) = data.data() {
                        if size > 0 && size <= samples.len() {
                            let f32_slice: &[f32] = unsafe {
                                std::slice::from_raw_parts(
                                    samples.as_ptr() as *const f32,
                                    size / std::mem::size_of::<f32>(),
                                )
                            };
                            if let Ok(mut lock) = buf_ref.lock() {
                                lock.extend_from_slice(f32_slice);
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

    // Build audio format params — request F32LE like the official example
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).unwrap()];

    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    eprintln!("pipecap-audio: stream connected");

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
