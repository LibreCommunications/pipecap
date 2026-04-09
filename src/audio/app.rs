//! Per-app audio capture via PipeWire registry watching.
//!
//! Creates a fresh capture stream for each matched audio node rather than
//! reusing one — PipeWire streams don't reliably produce audio after
//! disconnect/reconnect to a different node.

use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::resolve::{self, audio_node_matches};
use super::{bytes_as_f32, mix::MixBuffer, AudioCtl};
use crate::pw_util;

/// A live audio stream connected to a specific node.
/// Dropping this disconnects and cleans up.
struct LiveStream {
    _stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<spa::param::audio::AudioInfoRaw>,
}

fn create_stream(
    core: &pw::core::CoreRc,
    node_id: u32,
    mix: &Arc<MixBuffer>,
) -> anyhow::Result<LiveStream> {
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Music");

    let stream = pw::stream::StreamRc::new(core.clone(), "pipecap-audio", props)?;

    let mix_p = mix.clone();
    let mix_f = mix.clone();

    let listener = stream
        .add_local_listener_with_user_data(spa::param::audio::AudioInfoRaw::default())
        .param_changed(move |_, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if ud.parse(param).is_ok() {
                eprintln!(
                    "pipecap-audio: negotiated {}ch {}Hz",
                    ud.channels(),
                    ud.rate()
                );
                mix_f.set_format(ud.channels(), ud.rate());
            }
        })
        .process(move |stream_ref, _| {
            let Some(mut pw_buf) = stream_ref.dequeue_buffer() else { return };
            let Some(data) = pw_buf.datas_mut().first_mut() else { return };

            let size = data.chunk().size() as usize;
            let Some(samples) = data.data() else { return };
            if size == 0 || size > samples.len() {
                return;
            }
            let Some(f32_slice) = bytes_as_f32(&samples[..size]) else {
                eprintln!("pipecap-audio: dropping misaligned f32 chunk ({size} bytes)");
                return;
            };
            mix_p.push(node_id, f32_slice);
        })
        .register()?;

    let bytes = super::audio_format_params();
    let Some(pod) = spa::pod::Pod::from_bytes(&bytes) else {
        anyhow::bail!("invalid audio format pod");
    };
    let mut params = [pod];
    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        super::STREAM_FLAGS,
        &mut params,
    )?;

    eprintln!("pipecap-audio: stream connected to node {node_id}");
    Ok(LiveStream {
        _stream: stream,
        _listener: listener,
    })
}

/// Per-app capture resolved from a portal video node.
pub fn run(
    video_node_id: u32,
    mix: Arc<MixBuffer>,
    receiver: pw::channel::Receiver<AudioCtl>,
) -> anyhow::Result<()> {
    let matcher = resolve::resolve_session_matcher(video_node_id);
    match &matcher {
        Some(m) => eprintln!("pipecap-audio: matcher={:?}", m),
        None => eprintln!("pipecap-audio: no matcher found, will watch all new audio nodes"),
    }
    run_with_matcher(matcher, mix, receiver)
}

/// Per-app capture by explicit app name (from setAudioTarget).
pub fn run_by_name(
    app_name: String,
    mix: Arc<MixBuffer>,
    receiver: pw::channel::Receiver<AudioCtl>,
) -> anyhow::Result<()> {
    let matcher = Some(resolve::SessionMatcher::AppName(app_name));
    eprintln!("pipecap-audio: matcher={:?}", matcher.as_ref().unwrap());
    run_with_matcher(matcher, mix, receiver)
}

fn run_with_matcher(
    matcher: Option<resolve::SessionMatcher>,
    mix: Arc<MixBuffer>,
    receiver: pw::channel::Receiver<AudioCtl>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    // Current live stream — replaced each time a new matching node appears
    let live: Rc<RefCell<Option<LiveStream>>> = Rc::new(RefCell::new(None));

    let registry = core
        .get_registry()
        .map_err(|e| anyhow::anyhow!("get_registry: {e}"))?;

    let live_g = live.clone();
    let live_r = live.clone();
    let core_g = core.clone();
    let mix_g = mix.clone();
    let connected_to: Rc<RefCell<Option<u32>>> = Rc::new(RefCell::new(None));
    let connected_g = connected_to.clone();
    let connected_r = connected_to.clone();
    let matcher_for_remove = matcher.clone();
    let mix_r = mix.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };
            if global.type_ != ObjectType::Node {
                return;
            }
            if props.get("media.class") != Some("Stream/Output/Audio") {
                return;
            }

            if let Some(ref m) = matcher
                && !audio_node_matches(props, m) {
                    return;
                }

            eprintln!(
                "pipecap-audio: matched audio node id={} app={:?}",
                global.id,
                props.get("application.name")
            );

            // Drop old stream and any leftover samples for the old source.
            if let Some(old) = connected_g.borrow().as_ref().copied() {
                mix_g.remove_source(old);
            }
            *live_g.borrow_mut() = None;

            match create_stream(&core_g, global.id, &mix_g) {
                Ok(s) => {
                    *live_g.borrow_mut() = Some(s);
                    *connected_g.borrow_mut() = Some(global.id);
                }
                Err(e) => eprintln!("pipecap-audio: stream create error: {e}"),
            }
        })
        .global_remove(move |id| {
            if connected_r.borrow().is_some_and(|c| c == id) {
                eprintln!(
                    "pipecap-audio: audio node {id} removed ({:?}), waiting...",
                    matcher_for_remove
                );
                *live_r.borrow_mut() = None;
                *connected_r.borrow_mut() = None;
                mix_r.remove_source(id);
            }
        })
        .register();

    pw_util::do_roundtrip(&mainloop, &core);
    eprintln!("pipecap-audio: watching registry for audio nodes...");

    let mainloop_weak = mainloop.downgrade();
    let _recv = receiver.attach(mainloop.loop_(), move |msg| match msg {
        AudioCtl::Stop => {
            if let Some(m) = mainloop_weak.upgrade() {
                m.quit();
            }
        }
    });

    mainloop.run();
    Ok(())
}
