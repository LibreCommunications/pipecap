//! Per-app audio capture via PipeWire registry watching.
//!
//! Creates a fresh capture stream for each matched audio node.

use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use super::MAX_SAMPLES;
use super::resolve;
use crate::pw_util;

struct LiveStream {
    _stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<spa::param::audio::AudioInfoRaw>,
}

fn create_stream(
    core: &pw::core::CoreRc,
    node_id: u32,
    buffer: &Arc<Mutex<Vec<f32>>>,
    ch_out: &Arc<Mutex<u32>>,
    sr_out: &Arc<Mutex<u32>>,
) -> anyhow::Result<LiveStream> {
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Music");

    let stream = pw::stream::StreamRc::new(core.clone(), "pipecap-audio", props)?;

    let fc = Arc::new(AtomicU64::new(0));
    let buf = buffer.clone();
    let ch = ch_out.clone();
    let sr = sr_out.clone();

    let listener = stream
        .add_local_listener_with_user_data(spa::param::audio::AudioInfoRaw::default())
        .param_changed(move |_, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() { return; }
            if ud.parse(param).is_ok() {
                eprintln!("pipecap-audio: negotiated {}ch {}Hz", ud.channels(), ud.rate());
                if let Ok(mut c) = ch.lock() { *c = ud.channels(); }
                if let Ok(mut r) = sr.lock() { *r = ud.rate(); }
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
                std::slice::from_raw_parts(samples.as_ptr() as *const f32, size / 4)
            };
            if let Ok(mut lock) = buf.lock() {
                lock.extend_from_slice(f32_slice);
                if lock.len() > MAX_SAMPLES {
                    let excess = lock.len() - MAX_SAMPLES;
                    lock.drain(..excess);
                }
            }
        })
        .register()?;

    let bytes = super::audio_format_params();
    let mut params = [spa::pod::Pod::from_bytes(&bytes).unwrap()];
    stream.connect(
        spa::utils::Direction::Input, Some(node_id),
        super::STREAM_FLAGS, &mut params,
    )?;

    eprintln!("pipecap-audio: stream connected to node {node_id}");
    Ok(LiveStream { _stream: stream, _listener: listener })
}

pub fn run_by_name(
    app_name: String,
    buffer: Arc<Mutex<Vec<f32>>>,
    channels_out: Arc<Mutex<u32>>,
    sample_rate_out: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    eprintln!("pipecap-audio: per-app mode (app={app_name})");

    let live: Rc<RefCell<Option<LiveStream>>> = Rc::new(RefCell::new(None));
    let registry = core.get_registry().map_err(|e| anyhow::anyhow!("get_registry: {e}"))?;

    let live_g = live.clone();
    let live_r = live.clone();
    let core_g = core.clone();
    let buf_g = buffer.clone();
    let ch_g = channels_out.clone();
    let sr_g = sample_rate_out.clone();
    let connected: Rc<RefCell<Option<u32>>> = Rc::new(RefCell::new(None));
    let conn_g = connected.clone();
    let conn_r = connected.clone();
    let name_g = app_name.clone();
    let name_r = app_name.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };
            if global.type_ != ObjectType::Node { return; }
            if props.get("media.class") != Some("Stream/Output/Audio") { return; }
            if !resolve::app_name_matches(props, &name_g) { return; }

            eprintln!("pipecap-audio: matched node {} (app={:?})",
                global.id, props.get("application.name"));

            *live_g.borrow_mut() = None;
            match create_stream(&core_g, global.id, &buf_g, &ch_g, &sr_g) {
                Ok(s) => {
                    *live_g.borrow_mut() = Some(s);
                    *conn_g.borrow_mut() = Some(global.id);
                }
                Err(e) => eprintln!("pipecap-audio: stream create error: {e}"),
            }
        })
        .global_remove(move |id| {
            if conn_r.borrow().map_or(false, |c| c == id) {
                eprintln!("pipecap-audio: node {id} removed (app={name_r}), waiting...");
                *live_r.borrow_mut() = None;
                *conn_r.borrow_mut() = None;
            }
        })
        .register();

    pw_util::do_roundtrip(&mainloop, &core);
    eprintln!("pipecap-audio: watching for audio nodes...");

    let ml = mainloop.downgrade();
    let _timer = mainloop.loop_().add_timer(move |_| {
        if stop_flag.load(Ordering::Relaxed) {
            if let Some(m) = ml.upgrade() { m.quit(); }
        }
    });
    _timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );

    mainloop.run();
    Ok(())
}
