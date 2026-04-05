//! PipeWire audio capture.
//!
//! System mode: captures from sink monitor (all desktop audio).
//! Per-app mode: resolves the app identity from the portal's video node,
//! then watches the registry for matching `Stream/Output/Audio` nodes and
//! connects our capture stream when they appear. The audio node may appear
//! at any time (even minutes after capture starts), so the registry listener
//! stays alive for the entire session.
//!
//! App identity resolution (tried in order):
//!   1. `node.link-group` on the video node — stable session tie (portal v5+)
//!   2. `media.name` = "kwin-screencast-<app>" — KDE/KWin specific
//! Audio node matching:
//!   - By `node.link-group` if we have one
//!   - By `application.name`, `application.process.binary`, `node.name`

use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use spa::pod::Pod;
use std::cell::RefCell;
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
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let channels: Arc<Mutex<u32>> = Arc::new(Mutex::new(2));
        let sample_rate: Arc<Mutex<u32>> = Arc::new(Mutex::new(48000));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let buf = buffer.clone();
        let ch = channels.clone();
        let sr = sample_rate.clone();
        let stop = stop_flag.clone();

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_audio_loop(target, buf, ch, sr, stop) {
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

const STREAM_FLAGS: pw::stream::StreamFlags = pw::stream::StreamFlags::from_bits_truncate(
    pw::stream::StreamFlags::AUTOCONNECT.bits()
        | pw::stream::StreamFlags::MAP_BUFFERS.bits()
        | pw::stream::StreamFlags::RT_PROCESS.bits(),
);

const MAX_SAMPLES: usize = 48000 * 2 * 2;

fn connect_stream_to(stream: &pw::stream::StreamRc, node_id: Option<u32>) {
    let _ = stream.disconnect();
    let values = audio_format_params();
    let mut params = [Pod::from_bytes(&values).unwrap()];
    if let Err(e) = stream.connect(spa::utils::Direction::Input, node_id, STREAM_FLAGS, &mut params) {
        eprintln!("pipecap-audio: connect error: {e}");
    }
}

// ── App identity resolution ────────────────────────

/// How we identify which audio nodes belong to our capture session.
#[derive(Clone, Debug)]
enum SessionMatcher {
    /// Match audio nodes by `node.link-group` (reliable, portal v5+).
    LinkGroup(String),
    /// Match audio nodes by app name extracted from `media.name`
    /// (KWin: "kwin-screencast-<app>"). Case-insensitive matching against
    /// `application.name`, `application.process.binary`, `node.name`.
    AppName(String),
}

/// Query a node's full properties via `pw-cli info`.
/// Registry global.props only has a subset; pw-cli gives us the full info dict
/// which includes media.name, node.link-group, etc.
fn get_node_full_props(node_id: u32) -> std::collections::HashMap<String, String> {
    let mut props = std::collections::HashMap::new();

    let output = match std::process::Command::new("pw-cli")
        .args(["info", &node_id.to_string()])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("pipecap-audio: pw-cli failed: {e}");
            return props;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        // pw-cli info output format: "  key = \"value\"" or "  key = value"
        if let Some((key, val)) = trimmed.split_once(" = ") {
            let key = key.trim().replace('*', "");
            let val = val.trim().trim_matches('"').to_string();
            if !key.is_empty() && !val.is_empty() {
                props.insert(key, val);
            }
        }
    }
    props
}

/// Inspect the video node's properties and figure out how to match audio nodes.
fn resolve_session_matcher(video_node_id: u32) -> Option<SessionMatcher> {
    let props = get_node_full_props(video_node_id);

    let link_group = props.get("node.link-group").map(|s| s.as_str());
    let media_name = props.get("media.name").map(|s| s.as_str()).unwrap_or("");

    eprintln!("pipecap-audio: video node {} full props: link-group={:?} media.name={:?}",
        video_node_id, link_group, media_name);

    // Prefer link-group if available (portal v5+)
    if let Some(lg) = link_group {
        if !lg.is_empty() {
            return Some(SessionMatcher::LinkGroup(lg.to_string()));
        }
    }

    // KWin: media.name = "kwin-screencast-<app>"
    if let Some(app) = media_name.strip_prefix("kwin-screencast-") {
        if !app.is_empty() {
            eprintln!("pipecap-audio: extracted app name {:?} from media.name", app);
            return Some(SessionMatcher::AppName(app.to_string()));
        }
    }

    eprintln!("pipecap-audio: no usable identity on video node {}", video_node_id);
    None
}

/// Check if an audio output node matches our session.
fn audio_node_matches(
    props: &pw::spa::utils::dict::DictRef,
    matcher: &SessionMatcher,
) -> bool {
    match matcher {
        SessionMatcher::LinkGroup(lg) => {
            props.get("node.link-group") == Some(lg.as_str())
        }
        SessionMatcher::AppName(app) => {
            let target = app.to_lowercase();
            let app_name = props.get("application.name").unwrap_or("").to_lowercase();
            let binary = props.get("application.process.binary").unwrap_or("").to_lowercase();
            let node_name = props.get("node.name").unwrap_or("").to_lowercase();

            app_name == target || binary == target || node_name == target
        }
    }
}

// ── Main loop ──────────────────────────────────────

fn run_audio_loop(
    target: AudioTarget,
    buffer: Arc<Mutex<Vec<f32>>>,
    channels_out: Arc<Mutex<u32>>,
    sample_rate_out: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let per_app = matches!(target, AudioTarget::AppFromVideoNode(_));
    eprintln!("pipecap-audio: mode={}", if per_app { "per-app" } else { "system" });

    // Resolve how to match audio nodes for this session
    let matcher = if let AudioTarget::AppFromVideoNode(video_node_id) = &target {
        let m = resolve_session_matcher(*video_node_id);
        match &m {
            Some(m) => eprintln!("pipecap-audio: matcher={:?}", m),
            None => eprintln!("pipecap-audio: no matcher found, will watch all new audio nodes"),
        }
        m
    } else {
        None
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

    // Audio callbacks
    let fc = Arc::new(AtomicU64::new(0));
    let fc2 = fc.clone();
    let ch_out = channels_out;
    let sr_out = sample_rate_out;

    let _stream_listener = stream
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
            let n = fc2.fetch_add(1, Ordering::Relaxed);
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

    // Per-app: watch registry for matching audio nodes.
    // The audio node may not exist yet — keep watching indefinitely.
    // System: connect to sink monitor immediately.
    let _registry;
    let _reg_listener;

    if per_app {
        let registry = core.get_registry().map_err(|e| anyhow::anyhow!("get_registry: {e}"))?;

        let stream_clone = stream.clone();
        let stream_for_remove = stream.clone();
        let connected_to: Rc<RefCell<Option<u32>>> = Rc::new(RefCell::new(None));
        let connected_ref = connected_to.clone();
        let connected_for_remove = connected_to.clone();
        let matcher_for_remove = matcher.clone();

        let listener = registry
            .add_listener_local()
            .global(move |global| {
                let Some(props) = global.props else { return };
                if global.type_ != ObjectType::Node { return; }
                if props.get("media.class") != Some("Stream/Output/Audio") { return; }

                // If we have a matcher, use it. Otherwise accept any new audio node
                // that appears (last one wins — best effort when we can't identify the app).
                if let Some(ref m) = matcher {
                    if !audio_node_matches(props, m) { return; }
                }

                eprintln!("pipecap-audio: matched audio node id={} app={:?} binary={:?}",
                    global.id,
                    props.get("application.name"),
                    props.get("application.process.binary"));

                connect_stream_to(&stream_clone, Some(global.id));
                *connected_ref.borrow_mut() = Some(global.id);
            })
            .global_remove(move |id| {
                let is_ours = connected_for_remove.borrow().map_or(false, |cid| cid == id);
                if is_ours {
                    eprintln!("pipecap-audio: audio node {} removed ({:?}), waiting...",
                        id, matcher_for_remove);
                    let _ = stream_for_remove.disconnect();
                    *connected_for_remove.borrow_mut() = None;
                }
            })
            .register();

        pw_util::do_roundtrip(&mainloop, &core);
        eprintln!("pipecap-audio: watching registry for audio nodes...");

        _registry = Some(registry);
        _reg_listener = Some(listener);
    } else {
        connect_stream_to(&stream, None);
        eprintln!("pipecap-audio: connected to sink monitor");
        _registry = None;
        _reg_listener = None;
    }

    // Poll stop flag
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
