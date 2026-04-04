mod portal;
mod capture;
mod audio;

use napi_derive::napi;
use napi::{Error, Result, Status};
use std::sync::Mutex;

static CAPTURER: Mutex<Option<capture::Capturer>> = Mutex::new(None);
static AUDIO_CAPTURER: Mutex<Option<audio::AudioCapturer>> = Mutex::new(None);

// Tokio runtime for async portal calls
static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

fn get_runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime")
    })
}

/// Stream info returned by the portal picker.
#[napi(object)]
pub struct PortalStream {
    pub node_id: u32,
    pub source_type: u32,
    pub width: i32,
    pub height: i32,
}

/// A single video frame (RGBA pixels).
#[napi(object)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: napi::bindgen_prelude::Buffer,
}

/// Audio samples (interleaved f32 PCM).
#[napi(object)]
pub struct AudioChunk {
    pub channels: u32,
    pub sample_rate: u32,
    pub data: napi::bindgen_prelude::Buffer,
}

// ── Portal ──────────────────────────────────────────

/// Show the native xdg-desktop-portal screen/window picker.
/// `source_types`: 1=monitors, 2=windows, 3=both.
/// Returns the selected stream(s), or null if the user cancelled.
#[napi]
pub async fn show_picker(source_types: u32) -> Result<Option<Vec<PortalStream>>> {
    let rt = get_runtime();
    let streams = rt
        .block_on(portal::request_screen_cast(source_types))
        .map_err(|e| Error::new(Status::GenericFailure, format!("portal error: {e}")))?;

    match streams {
        None => Ok(None),
        Some(s) => Ok(Some(
            s.into_iter()
                .map(|st| PortalStream {
                    node_id: st.node_id,
                    source_type: st.source_type,
                    width: st.width,
                    height: st.height,
                })
                .collect(),
        )),
    }
}

// ── Video Capture ───────────────────────────────────

/// Start capturing video frames from a PipeWire node.
/// The node_id must come from show_picker() (portal-consented).
#[napi]
pub fn start_capture(node_id: u32, width: u32, height: u32) -> Result<()> {
    let mut lock = CAPTURER
        .lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    if lock.is_some() {
        lock.take();
    }
    let capturer = capture::Capturer::new(node_id, width, height)
        .map_err(|e| Error::new(Status::GenericFailure, format!("capture error: {e}")))?;
    *lock = Some(capturer);
    Ok(())
}

/// Read the latest video frame. Returns null if no frame is available yet.
#[napi]
pub fn read_frame() -> Result<Option<Frame>> {
    let lock = CAPTURER
        .lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    match lock.as_ref() {
        None => Err(Error::new(Status::GenericFailure, "not capturing")),
        Some(cap) => match cap.read_frame() {
            None => Ok(None),
            Some(f) => Ok(Some(Frame {
                width: f.width,
                height: f.height,
                data: f.data.into(),
            })),
        },
    }
}

/// Stop video capture and release PipeWire resources.
#[napi]
pub fn stop_capture() {
    if let Ok(mut lock) = CAPTURER.lock() {
        lock.take();
    }
}

// ── Audio Capture ───────────────────────────────────

/// Start capturing system audio via PipeWire.
/// Captures from the default audio output (monitor).
/// `exclude_pid`: PID of the current process to exclude from capture (prevents feedback).
#[napi]
pub fn start_audio_capture(exclude_pid: u32) -> Result<()> {
    let mut lock = AUDIO_CAPTURER
        .lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    if lock.is_some() {
        lock.take();
    }
    let capturer = audio::AudioCapturer::new(exclude_pid)
        .map_err(|e| Error::new(Status::GenericFailure, format!("audio capture error: {e}")))?;
    *lock = Some(capturer);
    Ok(())
}

/// Read accumulated audio samples. Returns null if no audio available.
/// Audio is interleaved f32 PCM (typically stereo 48kHz).
/// The buffer is drained on each call.
#[napi]
pub fn read_audio() -> Result<Option<AudioChunk>> {
    let lock = AUDIO_CAPTURER
        .lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    match lock.as_ref() {
        None => Err(Error::new(Status::GenericFailure, "not capturing audio")),
        Some(cap) => match cap.read_audio() {
            None => Ok(None),
            Some(buf) => {
                // Convert f32 samples to bytes for the Buffer
                let bytes: Vec<u8> = buf
                    .data
                    .iter()
                    .flat_map(|s| s.to_le_bytes())
                    .collect();
                Ok(Some(AudioChunk {
                    channels: buf.channels,
                    sample_rate: buf.sample_rate,
                    data: bytes.into(),
                }))
            }
        },
    }
}

/// Stop audio capture.
#[napi]
pub fn stop_audio_capture() {
    if let Ok(mut lock) = AUDIO_CAPTURER.lock() {
        lock.take();
    }
}
