//! App name matching and PipeWire graph queries.

use std::collections::HashSet;

/// Match audio nodes by app name. Case-insensitive, also matches the last
/// segment of desktop file IDs (e.g. "org.kde.haruna" matches "haruna").
pub fn app_name_matches(props: &pipewire::spa::utils::dict::DictRef, target: &str) -> bool {
    let t = target.to_lowercase();
    let short = t.rsplit('.').next().unwrap_or(&t);
    [
        props.get("application.name"),
        props.get("application.process.binary"),
        props.get("node.name"),
    ]
    .iter()
    .any(|v| {
        let Some(val) = v.map(|s| s.to_lowercase()) else { return false };
        val == t || val == short
    })
}

// ── Graph queries ──────────────────────────────────

pub struct AudioApp {
    pub name: String,
    pub binary: String,
}

/// List applications currently producing audio.
pub fn list_audio_apps() -> anyhow::Result<Vec<AudioApp>> {
    query_apps(Some("Stream/Output/Audio"))
}

/// List all applications visible in PipeWire (any Stream/* class).
pub fn list_all_apps() -> anyhow::Result<Vec<AudioApp>> {
    query_apps(None)
}

fn query_apps(class_filter: Option<&str>) -> anyhow::Result<Vec<AudioApp>> {
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
        let class = props.get("media.class").and_then(|v| v.as_str()).unwrap_or("");

        match class_filter {
            Some(filter) => { if class != filter { continue; } }
            None => { if !class.starts_with("Stream/") { continue; } }
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
