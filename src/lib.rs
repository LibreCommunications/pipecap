mod portal;
mod capture;
mod audio;

use napi_derive::napi;
use napi::{Error, Result, Status};
use std::sync::Mutex;

static CAPTURER: Mutex<Option<capture::Capturer>> = Mutex::new(None);
static AUDIO_CAPTURER: Mutex<Option<audio::AudioCapturer>> = Mutex::new(None);

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
    /// Interleaved f32 PCM samples as little-endian bytes.
    pub data: napi::bindgen_prelude::Buffer,
}

// ── Portal ──────────────────────────────────────────

/// Show the native xdg-desktop-portal screen/window picker.
/// `source_types`: 1=monitors, 2=windows, 3=both.
/// Returns the selected stream(s), or null if the user cancelled.
/// Result from the portal picker — streams + PipeWire remote fd.
#[napi(object)]
pub struct PickerResult {
    pub streams: Vec<PortalStream>,
    /// Raw fd to the PipeWire remote. Pass this to startCapture.
    pub pipewire_fd: i32,
}

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

// ── Capture ─────────────────────────────────────────

/// Capture options.
#[napi(object)]
pub struct CaptureOptions {
    pub node_id: u32,
    /// PipeWire remote fd from showPicker().
    pub pipewire_fd: i32,
    pub fps: u32,
    pub audio: bool,
    /// PID of the current process — used to exclude own audio output from capture.
    pub exclude_pid: Option<u32>,
}

/// Start capturing from a PipeWire node.
/// `node_id` must come from show_picker() (portal-consented).
#[napi]
pub fn start_capture(options: CaptureOptions) -> Result<()> {
    // Video
    {
        let mut lock = CAPTURER
            .lock()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        lock.take();
        let capturer = capture::Capturer::new(options.node_id, options.pipewire_fd, options.fps)
            .map_err(|e| Error::new(Status::GenericFailure, format!("capture error: {e}")))?;
        *lock = Some(capturer);
    }

    // Audio (optional)
    if options.audio {
        let mut lock = AUDIO_CAPTURER
            .lock()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        lock.take();
        let capturer = audio::AudioCapturer::new(options.exclude_pid.unwrap_or(0))
            .map_err(|e| Error::new(Status::GenericFailure, format!("audio capture error: {e}")))?;
        *lock = Some(capturer);
    }

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

/// Read accumulated audio samples. Returns null if no audio available or audio not enabled.
/// Audio is interleaved f32 PCM (typically stereo 48kHz). Buffer is drained on each call.
#[napi]
pub fn read_audio() -> Result<Option<AudioChunk>> {
    let lock = AUDIO_CAPTURER
        .lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    match lock.as_ref() {
        None => Ok(None), // Audio not enabled — not an error
        Some(cap) => match cap.read_audio() {
            None => Ok(None),
            Some(buf) => {
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

/// Check if capture is currently active.
#[napi]
pub fn is_capturing() -> bool {
    let video = CAPTURER.lock().map(|l| l.is_some()).unwrap_or(false);
    let audio = AUDIO_CAPTURER.lock().map(|l| l.is_some()).unwrap_or(false);
    video || audio
}

/// Stop all capture (video + audio) and release PipeWire resources.
#[napi]
pub fn stop_capture() {
    if let Ok(mut lock) = CAPTURER.lock() {
        lock.take();
    }
    if let Ok(mut lock) = AUDIO_CAPTURER.lock() {
        lock.take();
    }
}
