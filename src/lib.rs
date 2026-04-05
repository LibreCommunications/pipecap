mod portal;
mod capture;
mod audio;
mod shm;
mod pw_util;

use napi_derive::napi;
use napi::{Error, Result, Status};
use std::sync::Mutex;

static CAPTURER: Mutex<Option<capture::Capturer>> = Mutex::new(None);
static AUDIO_CAPTURER: Mutex<Option<audio::AudioCapturer>> = Mutex::new(None);

// ── Napi types ─────────────────────────────────────

#[napi(object)]
pub struct PortalStream {
    pub node_id: u32,
    /// 1=monitor, 2=window
    pub source_type: u32,
    pub width: i32,
    pub height: i32,
}

#[napi(object)]
pub struct PickerResult {
    pub streams: Vec<PortalStream>,
    pub pipewire_fd: i32,
}

#[napi(object)]
pub struct CaptureOptions {
    pub node_id: u32,
    pub pipewire_fd: i32,
    pub fps: u32,
    pub audio: bool,
    pub app_name: Option<String>,
}

#[napi(object)]
pub struct ShmInfo {
    pub shm_path: String,
    pub shm_size: u32,
    pub header_size: u32,
}

#[napi(object)]
pub struct AudioApp {
    pub name: String,
}

#[napi(object)]
pub struct AudioChunk {
    pub channels: u32,
    pub sample_rate: u32,
    pub data: napi::bindgen_prelude::Buffer,
}

// ── Portal ─────────────────────────────────────────

/// Show the native xdg-desktop-portal screen/window picker.
/// `sourceTypes`: 1=monitors, 2=windows, 3=both.
#[napi]
pub async fn show_picker(source_types: u32) -> Result<Option<PickerResult>> {
    let result = portal::request_screen_cast(source_types)
        .await
        .map_err(|e| Error::new(Status::GenericFailure, format!("portal: {e}")))?;

    let Some(r) = result else { return Ok(None) };
    Ok(Some(PickerResult {
        streams: r.streams.into_iter().map(|s| PortalStream {
            node_id: s.node_id,
            source_type: s.source_type,
            width: s.width,
            height: s.height,
        }).collect(),
        pipewire_fd: r.pipewire_fd,
    }))
}

// ── Audio apps ─────────────────────────────────────

/// List applications currently producing audio (via `pw-dump`).
#[napi]
pub fn list_audio_apps() -> Result<Vec<AudioApp>> {
    let output = std::process::Command::new("pw-dump")
        .output()
        .map_err(|e| Error::new(Status::GenericFailure, format!("pw-dump: {e}")))?;

    let objects: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::new(Status::GenericFailure, format!("pw-dump parse: {e}")))?;

    let mut apps = std::collections::HashSet::new();
    for obj in &objects {
        if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = obj.pointer("/info/props") else { continue };
        if props.get("media.class").and_then(|v| v.as_str()) != Some("Stream/Output/Audio") {
            continue;
        }
        if let Some(name) = props.get("application.name").and_then(|v| v.as_str()) {
            if !name.is_empty() {
                apps.insert(name.to_string());
            }
        }
    }

    let mut result: Vec<AudioApp> = apps.into_iter().map(|name| AudioApp { name }).collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

// ── Capture ────────────────────────────────────────

/// Start video + optional audio capture. Returns shared memory info.
#[napi]
pub fn start_capture(options: CaptureOptions) -> Result<ShmInfo> {
    let shm_size = {
        let mut lock = CAPTURER.lock()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        lock.take();
        let capturer = capture::Capturer::new(options.node_id, options.pipewire_fd, options.fps)
            .map_err(|e| Error::new(Status::GenericFailure, format!("capture: {e}")))?;
        let size = capturer.shm_size();
        *lock = Some(capturer);
        size
    };

    if options.audio {
        let mut lock = AUDIO_CAPTURER.lock()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        lock.take();
        let app_name = options.app_name.filter(|s| !s.is_empty());
        let capturer = audio::AudioCapturer::new(app_name)
            .map_err(|e| Error::new(Status::GenericFailure, format!("audio: {e}")))?;
        *lock = Some(capturer);
    }

    Ok(ShmInfo {
        shm_path: "/dev/shm/pipecap-frames".to_string(),
        shm_size: shm_size as u32,
        header_size: 32,
    })
}

/// Read accumulated audio samples (interleaved f32 PCM). Returns null if none available.
#[napi]
pub fn read_audio() -> Result<Option<AudioChunk>> {
    let lock = AUDIO_CAPTURER.lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    let Some(cap) = lock.as_ref() else { return Ok(None) };
    let Some(buf) = cap.read_audio() else { return Ok(None) };

    let bytes: Vec<u8> = buf.data.iter().flat_map(|s| s.to_le_bytes()).collect();
    Ok(Some(AudioChunk {
        channels: buf.channels,
        sample_rate: buf.sample_rate,
        data: bytes.into(),
    }))
}

#[napi]
pub fn is_capturing() -> bool {
    CAPTURER.lock().map(|l| l.is_some()).unwrap_or(false)
}

/// Stop all capture (video + audio) and release resources.
#[napi]
pub fn stop_capture() {
    if let Ok(mut lock) = CAPTURER.lock() { lock.take(); }
    if let Ok(mut lock) = AUDIO_CAPTURER.lock() { lock.take(); }
}
