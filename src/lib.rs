//! Native PipeWire audio capture for Electron on Linux.
//! Per-app or system audio with dynamic target switching.

mod audio;
mod pw_util;

use napi::{Error, Result, Status};
use napi_derive::napi;
use std::sync::Mutex;

static AUDIO_CAPTURER: Mutex<Option<audio::AudioCapturer>> = Mutex::new(None);

fn err(msg: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, msg.to_string())
}

// ── Types ──────────────────────────────────────────

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

/// Start audio capture in system mode (all desktop audio).
#[napi]
pub fn start_audio() -> Result<()> {
    let mut lock = AUDIO_CAPTURER.lock().map_err(|e| err(e))?;
    lock.take();
    *lock = Some(
        audio::AudioCapturer::new(audio::AudioTarget::System)
            .map_err(|e| err(format!("audio: {e}")))?
    );
    Ok(())
}

/// Switch audio target at runtime. Recreates the audio pipeline.
/// `target`: "system", "none", or an app name (e.g. "Firefox").
#[napi]
pub fn set_audio_target(target: String) -> Result<()> {
    let mut lock = AUDIO_CAPTURER.lock().map_err(|e| err(e))?;
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

/// Drain accumulated audio samples. Returns null when no audio is available.
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

/// List applications currently producing audio.
#[napi]
pub fn list_audio_apps() -> Result<Vec<AudioAppInfo>> {
    let apps = audio::resolve::list_audio_apps().map_err(|e| err(format!("list apps: {e}")))?;
    Ok(apps.into_iter().map(|a| AudioAppInfo { name: a.name, binary: a.binary }).collect())
}

/// List all applications visible in PipeWire (including those not currently playing audio).
#[napi]
pub fn list_all_apps() -> Result<Vec<AudioAppInfo>> {
    let apps = audio::resolve::list_all_apps().map_err(|e| err(format!("list apps: {e}")))?;
    Ok(apps.into_iter().map(|a| AudioAppInfo { name: a.name, binary: a.binary }).collect())
}

/// Whether audio capture is active.
#[napi]
pub fn is_capturing() -> bool {
    AUDIO_CAPTURER.lock().map(|l| l.is_some()).unwrap_or(false)
}

/// Stop audio capture and release resources.
#[napi]
pub fn stop_audio() {
    if let Ok(mut lock) = AUDIO_CAPTURER.lock() { lock.take(); }
}
