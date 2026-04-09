//! System audio capture with per-process exclusion. Used so a
//! screen-recording app doesn't hear itself in the share.
//!
//! Per-app pids aren't on the registry global event for audio nodes —
//! only on the full info dict you get from binding the node. So for every
//! `Stream/Output/Audio` node we see, we bind a `pw::node::Node` proxy
//! and attach an info listener; when the info fires we read the real pid
//! and decide whether to capture or skip. Same path `pw-cli info` /
//! `pw-dump` use.

use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use super::{bytes_as_f32, mix::MixBuffer, AudioCtl};
use crate::pw_util;

struct LiveStream {
    _stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<spa::param::audio::AudioInfoRaw>,
}

/// Bound Node proxy + its info listener. Both must stay alive — dropping
/// either tears down the binding and the info event never arrives.
struct NodeBinding {
    _node: pw::node::Node,
    _listener: pw::node::NodeListener,
}

struct State {
    /// node id -> active capture stream
    live: HashMap<u32, LiveStream>,
    /// Nodes we've already made a final capture/exclude decision about.
    /// PipeWire fires `info` more than once per node and later events may
    /// carry only delta props (no application.process.id) — without this
    /// set we'd re-evaluate with empty pids and accidentally capture a
    /// node we just excluded.
    decided: HashSet<u32>,
    /// node id -> bound Node + info listener, kept alive until
    /// `global_remove` so the listener can re-fire on property updates.
    pending_bindings: HashMap<u32, NodeBinding>,
    exclude_pids: Vec<u32>,
    /// Lower-cased app names to exclude. Useful when the host sets a
    /// unique `application.name` — Chromium-based apps can't, but native
    /// PW apps usually can.
    exclude_app_names_lower: Vec<String>,
}

/// Pull every pid-like property out of a dict. Order doesn't matter —
/// the caller only checks membership.
fn parse_pids(props: &spa::utils::dict::DictRef) -> Vec<u32> {
    let mut pids: Vec<u32> = Vec::new();
    for key in ["pipewire.sec.pid", "application.process.id"] {
        if let Some(v) = props.get(key).and_then(|s| s.parse::<u32>().ok())
            && !pids.contains(&v) {
                pids.push(v);
            }
    }
    pids
}

fn any_excluded(pids: &[u32], exclude: &[u32]) -> bool {
    pids.iter().any(|p| exclude.contains(p))
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

    let stream = pw::stream::StreamRc::new(core.clone(), "pipecap-audio-mix", props)?;

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
            let Some(f32_slice) = bytes_as_f32(&samples[..size]) else { return };
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

    Ok(LiveStream {
        _stream: stream,
        _listener: listener,
    })
}

/// Decide what to do with a node. Excluded if *either* the pid or the
/// app-name matches our exclude lists. Empty `pids` and unknown `app_name`
/// mean we have nothing to filter on — capture conservatively rather than
/// silently dropping potentially-unrelated audio.
fn try_attach(
    state: &mut State,
    core: &pw::core::CoreRc,
    mix: &Arc<MixBuffer>,
    node_id: u32,
    pids: &[u32],
    app_name: Option<&str>,
) {
    // Once decided, a node stays decided until `global_remove`. See
    // `State.decided` for why.
    if state.decided.contains(&node_id) {
        return;
    }

    let pid_match = !pids.is_empty() && any_excluded(pids, &state.exclude_pids);
    let name_match = match app_name {
        Some(name) => {
            let lower = name.to_lowercase();
            state.exclude_app_names_lower.iter().any(|n| n == &lower)
        }
        None => false,
    };

    if pid_match || name_match {
        let reason = if pid_match { "pid" } else { "name" };
        eprintln!(
            "pipecap-audio: excluding node {node_id} pids={pids:?} app={app_name:?} (matched {reason})"
        );
        state.decided.insert(node_id);
        return;
    }

    if pids.is_empty() && app_name.is_none() {
        eprintln!(
            "pipecap-audio: capturing node {node_id} (no identifying info)"
        );
    } else {
        eprintln!(
            "pipecap-audio: capturing node {node_id} pids={pids:?} app={app_name:?}"
        );
    }
    match create_stream(core, node_id, mix) {
        Ok(s) => {
            state.live.insert(node_id, s);
            state.decided.insert(node_id);
        }
        Err(e) => eprintln!("pipecap-audio: stream create error: {e}"),
    }
}

pub fn run(
    exclude_pids: Vec<u32>,
    exclude_app_names: Vec<String>,
    mix: Arc<MixBuffer>,
    receiver: pw::channel::Receiver<AudioCtl>,
) -> anyhow::Result<()> {
    pw::init();

    let exclude_app_names_lower: Vec<String> =
        exclude_app_names.iter().map(|s| s.to_lowercase()).collect();
    eprintln!(
        "pipecap-audio: system-exclude mode, excluding pids={exclude_pids:?} app_names={exclude_app_names:?}"
    );

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    // `get_registry_rc` returns an Rc<Registry>; we need that (rather than
    // the lifetime-bound `get_registry`) because we have to call
    // `registry.bind(global)` from inside the registry's own `global`
    // listener closure, which requires capturing a 'static registry
    // reference. The pattern is straight from pipewire-rs's pw-mon example.
    let registry = core
        .get_registry_rc()
        .map_err(|e| anyhow::anyhow!("get_registry_rc: {e}"))?;
    let registry_weak = registry.downgrade();

    let state = Rc::new(RefCell::new(State {
        live: HashMap::new(),
        decided: HashSet::new(),
        pending_bindings: HashMap::new(),
        exclude_pids,
        exclude_app_names_lower,
    }));

    let state_g = state.clone();
    let state_r = state.clone();
    let core_g = core.clone();
    let mix_g = mix.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };
            let mut s = state_g.borrow_mut();

            if global.type_ == ObjectType::Node {
                if props.get("media.class") != Some("Stream/Output/Audio") {
                    return;
                }
                let app_name = props.get("application.name").map(|v| v.to_string());

                // The registry global only delivers a minimal property
                // subset for audio nodes — `application.process.id` lives
                // on the full info dict you get from binding the node.
                // See module-level docs.
                let Some(reg) = registry_weak.upgrade() else { return };
                let node: pw::node::Node = match reg.bind(global) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!(
                            "pipecap-audio: bind node {} failed: {e} — capturing conservatively",
                            global.id
                        );
                        try_attach(
                            &mut s,
                            &core_g,
                            &mix_g,
                            global.id,
                            &[],
                            app_name.as_deref(),
                        );
                        return;
                    }
                };

                let node_id = global.id;
                let app_name_for_info = app_name.clone();
                let state_for_info = state_g.clone();
                let core_for_info = core_g.clone();
                let mix_for_info = mix_g.clone();

                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        let pids = info
                            .props()
                            .map(parse_pids)
                            .unwrap_or_default();
                        // Loop callbacks are serial on this thread, so
                        // this borrow can never race the outer global().
                        let mut s = state_for_info.borrow_mut();
                        try_attach(
                            &mut s,
                            &core_for_info,
                            &mix_for_info,
                            node_id,
                            &pids,
                            app_name_for_info.as_deref(),
                        );
                    })
                    .register();

                s.pending_bindings.insert(
                    global.id,
                    NodeBinding {
                        _node: node,
                        _listener: listener,
                    },
                );
            }
        })
        .global_remove(move |id| {
            let mut s = state_r.borrow_mut();
            s.live.remove(&id);
            s.decided.remove(&id);
            s.pending_bindings.remove(&id);
        })
        .register();

    // mix.remove_source for nodes that go away — done via a second listener
    // closure capturing `mix` directly. The borrow above only mutates state.
    let mix_for_remove = mix.clone();
    let _reg_listener_remove = registry
        .add_listener_local()
        .global_remove(move |id| {
            mix_for_remove.remove_source(id);
        })
        .register();

    pw_util::do_roundtrip(&mainloop, &core);
    eprintln!("pipecap-audio: watching registry for output streams...");

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
