//! System audio capture that excludes one or more PIDs.
//!
//! Used so a screen-recording app does not hear itself in the recording.
//!
//! ## Why this is harder than it looks
//!
//! PipeWire does **not** put `application.process.id` on the registry global
//! props of `Stream/Output/Audio` nodes — Chromium / WebRTC / Firefox set
//! it on the parent **Client** object instead, and the Node only carries a
//! `client.id` reference. So filtering nodes by their own props will see
//! `pid=None` for everything and let the host app's audio leak straight
//! into the share.
//!
//! What we actually do:
//!   1. Watch `Client` globals and build a `client_id -> pid` map. We
//!      prefer `pipewire.sec.pid` (kernel-vouched via SO_PEERCRED — the
//!      client cannot lie about it) and fall back to the client-supplied
//!      `application.process.id` if the secure key isn't present.
//!   2. Watch `Stream/Output/Audio` Node globals, look up `client.id` in
//!      the map, and capture only if the resolved pid is not in the
//!      exclude list.
//!   3. Handle the race where a Node global arrives before its Client
//!      during the initial registry replay by parking the node in a
//!      pending list and re-evaluating it when the Client appears.

use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use super::{bytes_as_f32, mix::MixBuffer, AudioCtl};
use crate::pw_util;

struct LiveStream {
    _stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<spa::param::audio::AudioInfoRaw>,
}

#[derive(Clone)]
struct PendingNode {
    client_id: u32,
    app_name: Option<String>,
}

struct State {
    /// pipewire client global id -> os pid
    client_pids: HashMap<u32, u32>,
    /// node id -> live capture stream (kept alive)
    live: HashMap<u32, LiveStream>,
    /// node id -> info we already saw, waiting for the client to appear
    pending: HashMap<u32, PendingNode>,
    exclude_pids: Vec<u32>,
}

fn parse_pid(props: &spa::utils::dict::DictRef) -> Option<u32> {
    // pipewire.sec.pid is set by the server from SO_PEERCRED — the client
    // cannot forge it. application.process.id is client-supplied and is the
    // legacy fallback for setups where the secure key isn't published.
    let raw = props
        .get("pipewire.sec.pid")
        .or_else(|| props.get("application.process.id"))?;
    raw.parse().ok()
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

/// Decide what to do with a node now that we know its pid (or that there
/// is no pid mapping yet). Returns true if a stream was created.
fn try_attach(
    state: &mut State,
    core: &pw::core::CoreRc,
    mix: &Arc<MixBuffer>,
    node_id: u32,
    pid: Option<u32>,
    app_name: Option<&str>,
) {
    if let Some(pid) = pid {
        if state.exclude_pids.contains(&pid) {
            eprintln!(
                "pipecap-audio: excluding node {node_id} pid={pid} app={app_name:?}"
            );
            return;
        }
        eprintln!(
            "pipecap-audio: capturing node {node_id} pid={pid} app={app_name:?}"
        );
    } else {
        // No pid resolvable — be conservative and capture (better to share
        // an unrelated stream than to silently drop it).
        eprintln!(
            "pipecap-audio: capturing node {node_id} pid=? app={app_name:?} (no client mapping)"
        );
    }
    match create_stream(core, node_id, mix) {
        Ok(s) => {
            state.live.insert(node_id, s);
        }
        Err(e) => eprintln!("pipecap-audio: stream create error: {e}"),
    }
}

pub fn run(
    exclude_pids: Vec<u32>,
    mix: Arc<MixBuffer>,
    receiver: pw::channel::Receiver<AudioCtl>,
) -> anyhow::Result<()> {
    pw::init();

    eprintln!("pipecap-audio: system-exclude mode, excluding pids={exclude_pids:?}");

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core
        .get_registry()
        .map_err(|e| anyhow::anyhow!("get_registry: {e}"))?;

    let state = Rc::new(RefCell::new(State {
        client_pids: HashMap::new(),
        live: HashMap::new(),
        pending: HashMap::new(),
        exclude_pids,
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

            match global.type_ {
                ObjectType::Client => {
                    let Some(pid) = parse_pid(props) else { return };
                    s.client_pids.insert(global.id, pid);

                    // Resolve any nodes that were waiting on this client.
                    let to_resolve: Vec<(u32, PendingNode)> = s
                        .pending
                        .iter()
                        .filter(|(_, snap)| snap.client_id == global.id)
                        .map(|(id, snap)| (*id, snap.clone()))
                        .collect();
                    for (node_id, snap) in to_resolve {
                        s.pending.remove(&node_id);
                        try_attach(
                            &mut s,
                            &core_g,
                            &mix_g,
                            node_id,
                            Some(pid),
                            snap.app_name.as_deref(),
                        );
                    }
                }
                ObjectType::Node => {
                    if props.get("media.class") != Some("Stream/Output/Audio") {
                        return;
                    }
                    let app_name = props.get("application.name").map(|v| v.to_string());
                    let client_id = props
                        .get("client.id")
                        .and_then(|v| v.parse::<u32>().ok());

                    match client_id {
                        Some(cid) => match s.client_pids.get(&cid).copied() {
                            Some(pid) => {
                                try_attach(
                                    &mut s,
                                    &core_g,
                                    &mix_g,
                                    global.id,
                                    Some(pid),
                                    app_name.as_deref(),
                                );
                            }
                            None => {
                                // Client hasn't appeared yet — park it.
                                s.pending.insert(
                                    global.id,
                                    PendingNode {
                                        client_id: cid,
                                        app_name,
                                    },
                                );
                            }
                        },
                        None => {
                            // No client.id at all — capture conservatively.
                            try_attach(
                                &mut s,
                                &core_g,
                                &mix_g,
                                global.id,
                                None,
                                app_name.as_deref(),
                            );
                        }
                    }
                }
                _ => {}
            }
        })
        .global_remove(move |id| {
            let mut s = state_r.borrow_mut();
            // The id could refer to either a node or a client; clean up both.
            if s.live.remove(&id).is_some() {
                // Note: mix.remove_source needs the same id we pushed under.
                // We can't borrow mix here because it's not in scope; do it
                // outside the borrow.
            }
            s.pending.remove(&id);
            s.client_pids.remove(&id);
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
