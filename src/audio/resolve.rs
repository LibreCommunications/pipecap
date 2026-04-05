//! App identity resolution and PipeWire graph queries.

use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub enum SessionMatcher {
    LinkGroup(String),
    AppName(String),
}

/// Read the full property set from a PipeWire node via `pw-cli info`.
fn get_node_full_props(node_id: u32) -> HashMap<String, String> {
    let mut props = HashMap::new();
    let Ok(output) = std::process::Command::new("pw-cli")
        .args(["info", &node_id.to_string()])
        .output()
    else {
        return props;
    };

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let trimmed = line.trim_start_matches(['*', '\t', ' ']);
        if let Some((k, v)) = trimmed.split_once(" = ") {
            let k = k.trim();
            let v = v.trim().trim_matches('"');
            if !k.is_empty() && !v.is_empty() {
                props.insert(k.to_string(), v.to_string());
            }
        }
    }
    props
}

/// Determine how to match audio nodes for a given video capture session.
pub fn resolve_session_matcher(video_node_id: u32) -> Option<SessionMatcher> {
    let props = get_node_full_props(video_node_id);

    let link_group = props.get("node.link-group").map(|s| s.as_str());
    let media_name = props.get("media.name").map(|s| s.as_str()).unwrap_or("");

    eprintln!("pipecap-audio: video node {} full props: link-group={:?} media.name={:?}",
        video_node_id, link_group, media_name);

    if let Some(lg) = link_group {
        if !lg.is_empty() {
            return Some(SessionMatcher::LinkGroup(lg.to_string()));
        }
    }

    if let Some(app) = media_name.strip_prefix("kwin-screencast-").filter(|s| !s.is_empty()) {
        eprintln!("pipecap-audio: extracted app name {:?} from media.name", app);
        return Some(SessionMatcher::AppName(app.to_string()));
    }

    eprintln!("pipecap-audio: no usable identity on video node {}", video_node_id);
    None
}

/// Check if an audio output node matches our session.
pub fn audio_node_matches(
    props: &pipewire::spa::utils::dict::DictRef,
    matcher: &SessionMatcher,
) -> bool {
    match matcher {
        SessionMatcher::LinkGroup(lg) => {
            props.get("node.link-group") == Some(lg.as_str())
        }
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
                let Some(val) = v.map(|s| s.to_lowercase()) else { return false };
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
    let output = std::process::Command::new("pw-dump")
        .output()
        .map_err(|e| anyhow::anyhow!("pw-dump: {e}"))?;

    let objects: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("pw-dump parse: {e}"))?;

    let mut seen = HashSet::new();
    let mut apps = Vec::new();

    for obj in &objects {
        if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = obj.pointer("/info/props") else { continue };
        if props.get("media.class").and_then(|v| v.as_str()) != Some("Stream/Output/Audio") {
            continue;
        }
        let name = props.get("application.name").and_then(|v| v.as_str()).unwrap_or("");
        let binary = props.get("application.process.binary").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() { continue; }
        if seen.insert(name.to_string()) {
            apps.push(AudioApp { name: name.to_string(), binary: binary.to_string() });
        }
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(apps)
}

/// Resolve the captured app's name from the portal video node.
/// Returns None if the app cannot be identified.
pub fn resolve_app_name(video_node_id: u32) -> Option<String> {
    let props = get_node_full_props(video_node_id);
    let media_name = props.get("media.name").map(|s| s.as_str()).unwrap_or("");
    eprintln!("pipecap-audio: video node {video_node_id} media.name={media_name:?}");

    media_name.strip_prefix("kwin-screencast-")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}
