//! PipeWire audio capture with per-app filtering.
//!
//! System mode (app_name=None): captures from sink monitor (all audio).
//! Per-app mode (app_name=Some("Firefox")): enumerates PipeWire registry to
//! find the app's audio output node, then connects our capture stream directly
//! to that node via AUTOCONNECT.

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
    /// `app_name`: if Some, capture only that app's audio.
    /// If None, capture all system audio from the sink monitor.
    pub fn new(app_name: Option<String>) -> anyhow::Result<Self> {
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let channels: Arc<Mutex<u32>> = Arc::new(Mutex::new(2));
        let sample_rate: Arc<Mutex<u32>> = Arc::new(Mutex::new(48000));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let buf_ref = buffer.clone();
        let ch_ref = channels.clone();
        let sr_ref = sample_rate.clone();
        let stop_ref = stop_flag.clone();

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_audio_loop(app_name, buf_ref, ch_ref, sr_ref, stop_ref) {
                eprintln!("pipecap-audio: error: {e}");
            }
        });

        Ok(AudioCapturer {
            buffer, channels, sample_rate, stop_flag,
            thread: Some(thread),
        })
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

/// Enumerate PipeWire registry to find a node matching `app_name` with
/// media.class = "Stream/Output/Audio". Returns the node ID.
fn find_app_audio_node(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    app_name: &str,
) -> anyhow::Result<u32> {
    let registry = core.get_registry().map_err(|e| anyhow::anyhow!("get_registry: {e}"))?;

    let found_id: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
    let found_ref = found_id.clone();
    let target = app_name.to_string();

    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            if let Some(props) = global.props {
                if global.type_ == ObjectType::Node
                    && props.get("media.class") == Some("Stream/Output/Audio")
                {
                    // Match on application.name or node.name
                    let matches = props.get("application.name") == Some(&target)
                        || props.get("node.name") == Some(&target);
                    if matches {
                        eprintln!("pipecap-audio: found '{}' audio node id={}", target, global.id);
                        found_ref.set(Some(global.id));
                    }
                }
            }
        })
        .register();

    do_roundtrip(mainloop, core);

    found_id.get().ok_or_else(|| {
        anyhow::anyhow!("no audio output node found for app '{app_name}'")
    })
}

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
    eprintln!("pipecap-audio: mode={}", if per_app {
        format!("per-app ({})", app_name.as_deref().unwrap())
    } else {
        "system".to_string()
    });

    // For per-app: discover the target node ID before creating the stream
    let target_node_id = if let Some(ref name) = app_name {
        Some(find_app_audio_node(&mainloop, &core, name)?)
    } else {
        None
    };

    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Music");

    if !per_app {
        // System audio: capture from default sink monitor
        props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    }

    let stream = pw::stream::StreamRc::new(core.clone(), "pipecap-audio", props)?;

    // Process callback — accumulates audio samples
    let buf_ref = buffer;
    let ch_out = channels_out;
    let sr_out = sample_rate_out;
    let frame_count = Arc::new(AtomicU64::new(0));
    let fc = frame_count.clone();

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
            if let Some(mut buffer) = stream_ref.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if let Some(data) = datas.first_mut() {
                    let size = data.chunk().size() as usize;
                    if n < 3 { eprintln!("pipecap-audio: frame #{n} size={size}"); }
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
                                const MAX: usize = 48000 * 2 * 2;
                                if lock.len() > MAX { let d = lock.len() - MAX; lock.drain(..d); }
                            }
                        }
                    }
                }
            }
        })
        .register()?;

    // Audio format params
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
    ).unwrap().0.into_inner();
    let mut params = [Pod::from_bytes(&values).unwrap()];

    // Connect the stream:
    // - System mode: AUTOCONNECT to sink monitor (no target node)
    // - Per-app mode: AUTOCONNECT to the target app's audio output node
    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;

    stream.connect(spa::utils::Direction::Input, target_node_id, flags, &mut params)?;

    eprintln!("pipecap-audio: stream connected{}",
        target_node_id.map_or(String::new(), |id| format!(" to node {id}")));

    // Main loop with stop timer
    let mainloop_weak = mainloop.downgrade();
    let _timer = mainloop.loop_().add_timer(move |_| {
        if stop_flag.load(Ordering::Relaxed) {
            if let Some(ml) = mainloop_weak.upgrade() {
                ml.quit();
            }
        }
    });
    _timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );

    mainloop.run();
    Ok(())
}

fn do_roundtrip(mainloop: &pw::main_loop::MainLoopRc, core: &pw::core::CoreRc) {
    let done = Rc::new(Cell::new(false));
    let done_clone = done.clone();
    let loop_clone = mainloop.clone();
    let pending = core.sync(0).expect("sync failed");
    let _listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                done_clone.set(true);
                loop_clone.quit();
            }
        })
        .register();
    while !done.get() {
        mainloop.run();
    }
}
