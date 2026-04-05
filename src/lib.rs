//! Native PipeWire screen capture for Electron.

mod audio;
mod capture;
mod portal;
mod pw_util;
mod shm;

use napi::{Error, Result, Status};
use napi_derive::napi;
use std::sync::Mutex;

static CAPTURER: Mutex<Option<capture::Capturer>> = Mutex::new(None);
static AUDIO_CAPTURER: Mutex<Option<audio::AudioCapturer>> = Mutex::new(None);

fn err(msg: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, msg.to_string())
}

// ── Types ──────────────────────────────────────────

#[napi(object)]
pub struct PortalStream {
    pub node_id: u32,
    /// 1 = monitor, 2 = window.
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
    /// 1 = monitor, 2 = window.
    pub source_type: u32,
}

#[napi(object)]
pub struct CaptureInfo {
    pub shm_path: String,
    pub shm_size: u32,
    pub header_size: u32,
    pub width: u32,
    pub height: u32,
    /// Auto-detected app name, or null if undetectable.
    pub detected_app: Option<String>,
}

#[napi(object)]
pub struct AudioChunk {
    pub channels: u32,
    pub sample_rate: u32,
    pub data: napi::bindgen_prelude::Buffer,
}

#[napi(object)]
pub struct AudioAppInfo {
    pub name: String,
    pub binary: String,
}

// ── API ────────────────────────────────────────────

#[napi]
pub async fn show_picker(source_types: u32) -> Result<Option<PickerResult>> {
    let result = portal::request_screen_cast(source_types)
        .await
        .map_err(|e| err(format!("portal: {e}")))?;

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

/// Start capture. Auto-detects audio target for window captures.
/// Returns `detectedApp` so the frontend knows whether to show a picker.
#[napi]
pub fn start_capture(options: CaptureOptions) -> Result<CaptureInfo> {
    let shm_size = {
        let mut lock = CAPTURER.lock().map_err(|e| err(e))?;
        lock.take();
        let cap = capture::Capturer::new(options.node_id, options.pipewire_fd, options.fps)
            .map_err(|e| err(format!("capture: {e}")))?;
        let size = cap.shm_size();
        *lock = Some(cap);
        size
    };

    let mut detected_app: Option<String> = None;

    if options.audio {
        let mut lock = AUDIO_CAPTURER.lock().map_err(|e| err(e))?;
        lock.take();

        let target = match options.source_type {
            2 => {
                // Window: try auto-detect, fall back to system
                detected_app = audio::resolve::resolve_app_name(options.node_id);
                match &detected_app {
                    Some(_) => audio::AudioTarget::AppFromVideoNode(options.node_id),
                    None => audio::AudioTarget::System, // frontend will call setAudioTarget
                }
            }
            _ => audio::AudioTarget::System,
        };

        *lock = Some(
            audio::AudioCapturer::new(target).map_err(|e| err(format!("audio: {e}")))?
        );
    }

    Ok(CaptureInfo {
        shm_path: "/dev/shm/pipecap-frames".to_string(),
        shm_size: shm_size as u32,
        header_size: 32,
        width: 0,
        height: 0,
        detected_app,
    })
}

/// Switch audio target at runtime. Recreates the audio capturer.
/// `target`: "system", "none", or an app name.
#[napi]
pub fn set_audio_target(target: String) -> Result<()> {
    let mut lock = AUDIO_CAPTURER.lock().map_err(|e| err(e))?;

    // Drop existing capturer
    lock.take();

    if target == "none" {
        return Ok(());
    }

    let audio_target = if target == "system" {
        audio::AudioTarget::System
    } else {
        audio::AudioTarget::AppByName(target)
    };

    *lock = Some(
        audio::AudioCapturer::new(audio_target).map_err(|e| err(format!("audio: {e}")))?
    );
    Ok(())
}

/// List applications currently producing audio.
#[napi]
pub fn list_audio_apps() -> Result<Vec<AudioAppInfo>> {
    let apps = audio::resolve::list_audio_apps().map_err(|e| err(format!("list apps: {e}")))?;
    Ok(apps.into_iter().map(|a| AudioAppInfo { name: a.name, binary: a.binary }).collect())
}

#[napi]
pub fn read_audio() -> Result<Option<AudioChunk>> {
    let lock = AUDIO_CAPTURER.lock().map_err(|e| err(e))?;
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

#[napi]
pub fn stop_capture() {
    if let Ok(mut lock) = CAPTURER.lock() { lock.take(); }
    if let Ok(mut lock) = AUDIO_CAPTURER.lock() { lock.take(); }
}
