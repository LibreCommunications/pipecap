//! PipeWire audio capture with per-app filtering.
//!
//! System mode (app_name=None): captures from sink monitor (all audio).
//! Per-app mode (app_name=Some("Firefox")): finds the app's audio output node
//! in the PipeWire registry, then connects our capture stream to it.

use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use spa::pod::Pod;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use crate::pw_util;

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
    pub fn new(app_name: Option<String>) -> anyhow::Result<Self> {
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let channels: Arc<Mutex<u32>> = Arc::new(Mutex::new(2));
        let sample_rate: Arc<Mutex<u32>> = Arc::new(Mutex::new(48000));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let buf = buffer.clone();
        let ch = channels.clone();
        let sr = sample_rate.clone();
        let stop = stop_flag.clone();

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_audio_loop(app_name, buf, ch, sr, stop) {
                eprintln!("pipecap-audio: error: {e}");
            }
        });

        Ok(Self { buffer, channels, sample_rate, stop_flag, thread: Some(thread) })
    }

    pub fn read_audio(&self) -> Option<AudioBuffer> {
        let mut lock = self.buffer.lock().ok()?;
        if lock.is_empty() { return None; }
        let data = std::mem::take(&mut *lock);
        let channels = *self.channels.lock().ok()?;
        let sample_rate = *self.sample_rate.lock().ok()?;
        Some(AudioBuffer { channels, sample_rate, data })
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

/// Search the PipeWire registry for an audio output node matching `app_name`.
fn find_app_audio_node(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    app_name: &str,
) -> anyhow::Result<u32> {
    let registry = core.get_registry().map_err(|e| anyhow::anyhow!("get_registry: {e}"))?;

    let found: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let found_ref = found.clone();
    let target = app_name.to_string();

    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };
            if global.type_ != ObjectType::Node { return; }
            if props.get("media.class") != Some("Stream/Output/Audio") { return; }

            let matches = props.get("application.name") == Some(&target)
                || props.get("node.name") == Some(&target);
            if matches {
                eprintln!("pipecap-audio: found '{}' node id={}", target, global.id);
                found_ref.set(Some(global.id));
            }
        })
        .register();

    pw_util::do_roundtrip(mainloop, core);

    found.get().ok_or_else(|| anyhow::anyhow!("no audio output node for '{app_name}'"))
}

fn audio_format_params() -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    pw_util::serialize_pod_object(obj)
}

/// Max samples kept in the ring buffer (~2 seconds of stereo 48kHz).
const MAX_SAMPLES: usize = 48000 * 2 * 2;

fn run_audio_loop(
    app_name: Option<String>,
    buffer: Arc<Mutex<Vec<f32>>>,
    channels_out: Arc<Mutex<u32>>,
    sample_rate_out: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let per_app = app_name.is_some();
    eprintln!("pipecap-audio: mode={}", match &app_name {
        Some(name) => format!("per-app ({name})"),
        None => "system".into(),
    });

    let target_node_id = match &app_name {
        Some(name) => Some(find_app_audio_node(&mainloop, &core, name)?),
        None => None,
    };

    // Stream properties
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Music");
    if !per_app {
        props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    }

    let stream = pw::stream::StreamRc::new(core.clone(), "pipecap-audio", props)?;
    let values = audio_format_params();
    let mut params = [Pod::from_bytes(&values).unwrap()];

    let frame_count = Arc::new(AtomicU64::new(0));
    let fc = frame_count.clone();
    let ch_out = channels_out;
    let sr_out = sample_rate_out;

    let _listener = stream
        .add_local_listener_with_user_data(spa::param::audio::AudioInfoRaw::default())
        .param_changed(move |_, user_data, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() { return; }
            if user_data.parse(param).is_ok() {
                let ch = user_data.channels();
                let rate = user_data.rate();
                eprintln!("pipecap-audio: negotiated {ch}ch {rate}Hz");
                if let Ok(mut c) = ch_out.lock() { *c = ch; }
                if let Ok(mut r) = sr_out.lock() { *r = rate; }
            }
        })
        .process(move |stream_ref, _| {
            let n = fc.fetch_add(1, Ordering::Relaxed);
            let Some(mut pw_buf) = stream_ref.dequeue_buffer() else { return };
            let Some(data) = pw_buf.datas_mut().first_mut() else { return };

            let size = data.chunk().size() as usize;
            if n < 3 { eprintln!("pipecap-audio: frame #{n} size={size}"); }

            let Some(samples) = data.data() else { return };
            if size == 0 || size > samples.len() { return; }

            let f32_slice: &[f32] = unsafe {
                std::slice::from_raw_parts(
                    samples.as_ptr() as *const f32,
                    size / std::mem::size_of::<f32>(),
                )
            };

            if let Ok(mut lock) = buffer.lock() {
                lock.extend_from_slice(f32_slice);
                if lock.len() > MAX_SAMPLES {
                    let excess = lock.len() - MAX_SAMPLES;
                    lock.drain(..excess);
                }
            }
        })
        .register()?;

    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;
    stream.connect(spa::utils::Direction::Input, target_node_id, flags, &mut params)?;
    eprintln!("pipecap-audio: stream connected{}",
        target_node_id.map_or(String::new(), |id| format!(" to node {id}")));

    // Poll stop flag and quit when signaled
    let mainloop_weak = mainloop.downgrade();
    let _timer = mainloop.loop_().add_timer(move |_| {
        if stop_flag.load(Ordering::Relaxed) {
            if let Some(ml) = mainloop_weak.upgrade() { ml.quit(); }
        }
    });
    _timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );

    mainloop.run();
    Ok(())
}
