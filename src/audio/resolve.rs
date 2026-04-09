//! App identity resolution and PipeWire graph queries.
//!
//! Uses the PipeWire registry directly via a short-lived client connection
//! instead of shelling out to `pw-cli` / `pw-dump`. The shellouts were
//! roughly 50–200ms each, allocated a child process and parsed unstructured
//! text — the registry path is in-process, structured, and ~10ms (one
//! roundtrip).

use pipewire as pw;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::pw_util;

#[derive(Clone, Debug)]
pub enum SessionMatcher {
    LinkGroup(String),
    AppName(String),
}

/// Run a one-shot registry enumeration. The closure is called for every
/// `Node` global with the node id and its property dict; mutate `state`
/// (held behind a `RefCell`) to collect what you need.
fn collect_from_registry<R, F>(init: R, on_node: F) -> anyhow::Result<R>
where
    R: 'static,
    F: Fn(&mut R, u32, &pw::spa::utils::dict::DictRef) + 'static,
{
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core
        .get_registry()
        .map_err(|e| anyhow::anyhow!("get_registry: {e}"))?;

    let state = Rc::new(RefCell::new(init));
    let state_g = state.clone();

    let listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != ObjectType::Node {
                return;
            }
            let Some(props) = global.props else { return };
            on_node(&mut state_g.borrow_mut(), global.id, props);
        })
        .register();

    // Existing globals are replayed on the first roundtrip; the second
    // ensures any in-flight events have been processed before we tear down.
    pw_util::do_roundtrip(&mainloop, &core);
    pw_util::do_roundtrip(&mainloop, &core);

    // Drop the listener so the closure (which holds the second Rc) is
    // released and we can unwrap the state.
    drop(listener);
    drop(registry);

    Rc::try_unwrap(state)
        .map(RefCell::into_inner)
        .map_err(|_| anyhow::anyhow!("internal: registry state still shared"))
}

fn dict_to_map(d: &pw::spa::utils::dict::DictRef) -> HashMap<String, String> {
    d.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Read the property set of a single PipeWire node by id.
fn query_node_props(node_id: u32) -> Option<HashMap<String, String>> {
    collect_from_registry(None::<HashMap<String, String>>, move |slot, id, props| {
        if id == node_id && slot.is_none() {
            *slot = Some(dict_to_map(props));
        }
    })
    .ok()
    .flatten()
}

/// Determine how to match audio nodes for a given video capture session.
pub fn resolve_session_matcher(video_node_id: u32) -> Option<SessionMatcher> {
    let props = query_node_props(video_node_id)?;

    let link_group = props.get("node.link-group").map(|s| s.as_str());
    let media_name = props.get("media.name").map(|s| s.as_str()).unwrap_or("");

    eprintln!(
        "pipecap-audio: video node {} props: link-group={:?} media.name={:?}",
        video_node_id, link_group, media_name
    );

    if let Some(lg) = link_group
        && !lg.is_empty() {
            return Some(SessionMatcher::LinkGroup(lg.to_string()));
        }

    if let Some(app) = media_name
        .strip_prefix("kwin-screencast-")
        .filter(|s| !s.is_empty())
    {
        eprintln!(
            "pipecap-audio: extracted app name {:?} from media.name",
            app
        );
        return Some(SessionMatcher::AppName(app.to_string()));
    }

    eprintln!(
        "pipecap-audio: no usable identity on video node {}",
        video_node_id
    );
    None
}

/// Check if an audio output node matches our session.
pub fn audio_node_matches(
    props: &pipewire::spa::utils::dict::DictRef,
    matcher: &SessionMatcher,
) -> bool {
    match matcher {
        SessionMatcher::LinkGroup(lg) => props.get("node.link-group") == Some(lg.as_str()),
        SessionMatcher::AppName(app) => {
            let target = app.to_lowercase();
            let short = target.rsplit('.').next().unwrap_or(&target);
            [
                props.get("application.name"),
                props.get("application.process.binary"),
                props.get("node.name"),
            ]
            .iter()
            .any(|v| {
                let Some(val) = v.map(|s| s.to_lowercase()) else {
                    return false;
                };
                val == target || val == short
            })
        }
    }
}

// ── Graph queries ──────────────────────────────────

pub struct AudioApp {
    pub name: String,
    pub binary: String,
}

/// List audio-producing applications from the PipeWire graph.
pub fn list_audio_apps() -> anyhow::Result<Vec<AudioApp>> {
    let collected: Vec<AudioApp> = collect_from_registry(
        Vec::<AudioApp>::new(),
        |out, _id, props| {
            if props.get("media.class") != Some("Stream/Output/Audio") {
                return;
            }
            let name = props.get("application.name").unwrap_or("");
            if name.is_empty() {
                return;
            }
            // Dedupe by name within the closure (rare to have many).
            if out.iter().any(|a| a.name == name) {
                return;
            }
            let binary = props.get("application.process.binary").unwrap_or("");
            out.push(AudioApp {
                name: name.to_string(),
                binary: binary.to_string(),
            });
        },
    )?;

    let mut apps = collected;
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(apps)
}

/// Resolve the captured app's name from the portal video node.
/// Returns None if the app cannot be identified.
pub fn resolve_app_name(video_node_id: u32) -> Option<String> {
    let props = query_node_props(video_node_id)?;
    let media_name = props.get("media.name").map(|s| s.as_str()).unwrap_or("");
    eprintln!("pipecap-audio: video node {video_node_id} media.name={media_name:?}");

    media_name
        .strip_prefix("kwin-screencast-")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}
