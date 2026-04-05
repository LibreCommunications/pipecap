mod portal;
mod capture;
mod audio;
mod shm;

use napi_derive::napi;
use napi::{Error, Result, Status};
use std::sync::Mutex;

static CAPTURER: Mutex<Option<capture::Capturer>> = Mutex::new(None);
static AUDIO_CAPTURER: Mutex<Option<audio::AudioCapturer>> = Mutex::new(None);

// ── Types ───────────────────────────────────────────

/// Stream info returned by the portal picker.
#[napi(object)]
pub struct PortalStream {
    pub node_id: u32,
    /// 1=monitor, 2=window
    pub source_type: u32,
    pub width: i32,
    pub height: i32,
}

/// Result from the portal picker.
#[napi(object)]
pub struct PickerResult {
    pub streams: Vec<PortalStream>,
    /// PipeWire remote fd — pass to startCapture.
    pub pipewire_fd: i32,
}

/// Capture options.
#[napi(object)]
pub struct CaptureOptions {
    /// PipeWire node ID from showPicker().
    pub node_id: u32,
    /// PipeWire remote fd from showPicker().
    pub pipewire_fd: i32,
    /// Requested frame rate (0 = source native rate).
    pub fps: u32,
    /// Enable audio capture.
    pub audio: bool,
    /// For per-app audio: the application name to capture (from listAudioApps).
    /// If unset or empty, captures all system audio.
    pub app_name: Option<String>,
}

/// Shared memory info returned by startCapture.
#[napi(object)]
pub struct ShmInfo {
    /// Path to the shared memory file.
    pub shm_path: String,
    /// Total size of the shared memory region in bytes.
    pub shm_size: u32,
    /// Size of the header at the start of the region.
    pub header_size: u32,
}

/// An audio-producing application detected in PipeWire.
#[napi(object)]
pub struct AudioApp {
    /// Display name of the application.
    pub name: String,
}

/// Audio samples (interleaved f32 PCM as little-endian bytes).
#[napi(object)]
pub struct AudioChunk {
    pub channels: u32,
    pub sample_rate: u32,
    pub data: napi::bindgen_prelude::Buffer,
}

// ── Portal ──────────────────────────────────────────

/// Show the native xdg-desktop-portal screen/window picker.
/// `sourceTypes`: 1=monitors, 2=windows, 3=both.
/// Returns the selected stream(s), or null if the user cancelled.
#[napi]
pub async fn show_picker(source_types: u32) -> Result<Option<PickerResult>> {
    let result = portal::request_screen_cast(source_types)
        .await
        .map_err(|e| Error::new(Status::GenericFailure, format!("portal error: {e}")))?;

    match result {
        None => Ok(None),
        Some(r) => Ok(Some(PickerResult {
            streams: r.streams
                .into_iter()
                .map(|st| PortalStream {
                    node_id: st.node_id,
                    source_type: st.source_type,
                    width: st.width,
                    height: st.height,
                })
                .collect(),
            pipewire_fd: r.pipewire_fd,
        })),
    }
}

// ── Audio Apps ───────────────────────────────────────

/// List applications currently producing audio.
/// Returns unique app names — show these in a dropdown for per-app audio selection.
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
        let props = match obj.pointer("/info/props") {
            Some(p) => p,
            None => continue,
        };
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

// ── Capture ─────────────────────────────────────────

/// Start video + optional audio capture.
///
/// Video frames are written to shared memory at `/dev/shm/pipecap-frames`.
/// Read the 32-byte header to detect new frames, then read pixel data.
///
/// Audio mode depends on `appName`:
/// - `undefined` or `""` → capture all system audio (sink monitor)
/// - `"Firefox"` etc → capture only that app's audio (PipeWire link-based)
///
/// Returns shared memory info for the renderer.
#[napi]
pub fn start_capture(options: CaptureOptions) -> Result<ShmInfo> {
    // Video
    let shm_size;
    {
        let mut lock = CAPTURER
            .lock()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        lock.take();
        let capturer = capture::Capturer::new(options.node_id, options.pipewire_fd, options.fps)
            .map_err(|e| Error::new(Status::GenericFailure, format!("capture: {e}")))?;
        shm_size = capturer.shm_size();
        *lock = Some(capturer);
    }

    // Audio
    if options.audio {
        let mut lock = AUDIO_CAPTURER
            .lock()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        lock.take();

        let app_name = options.app_name
            .filter(|s| !s.is_empty());

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

/// Read accumulated audio samples. Returns null if no audio available.
/// Audio is interleaved f32 PCM (typically stereo 48kHz). Buffer is drained on each call.
#[napi]
pub fn read_audio() -> Result<Option<AudioChunk>> {
    let lock = AUDIO_CAPTURER
        .lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    match lock.as_ref() {
        None => Ok(None),
        Some(cap) => match cap.read_audio() {
            None => Ok(None),
            Some(buf) => {
                let bytes: Vec<u8> = buf.data.iter().flat_map(|s| s.to_le_bytes()).collect();
                Ok(Some(AudioChunk {
                    channels: buf.channels,
                    sample_rate: buf.sample_rate,
                    data: bytes.into(),
                }))
            }
        },
    }
}

/// Check if capture is currently active.
#[napi]
pub fn is_capturing() -> bool {
    CAPTURER.lock().map(|l| l.is_some()).unwrap_or(false)
}

/// Stop all capture (video + audio) and release resources.
#[napi]
pub fn stop_capture() {
    if let Ok(mut lock) = CAPTURER.lock() {
        lock.take();
    }
    if let Ok(mut lock) = AUDIO_CAPTURER.lock() {
        lock.take();
    }
}
