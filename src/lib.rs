mod portal;
mod capture;
mod audio;
mod shm;

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

/// Result from the portal picker — streams + PipeWire remote fd.
#[napi(object)]
pub struct PickerResult {
    pub streams: Vec<PortalStream>,
    pub pipewire_fd: i32,
}

/// Audio samples (interleaved f32 PCM).
#[napi(object)]
pub struct AudioChunk {
    pub channels: u32,
    pub sample_rate: u32,
    pub data: napi::bindgen_prelude::Buffer,
}

/// Shared memory info returned by startCapture.
#[napi(object)]
pub struct ShmInfo {
    pub shm_path: String,
    pub shm_size: u32,
    pub header_size: u32,
}

/// Capture options.
#[napi(object)]
pub struct CaptureOptions {
    pub node_id: u32,
    pub pipewire_fd: i32,
    pub fps: u32,
    pub audio: bool,
    pub exclude_pid: Option<u32>,
}

// ── Portal ──────────────────────────────────────────

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

/// Start capturing. Returns a Buffer backed by the shared memory region.
/// The renderer can read frames directly from this buffer — zero copy.
///
/// Layout: ShmHeader (32 bytes) + slot0 + slot1
/// ShmHeader: { seq: u64, width: u32, height: u32, stride: u32, data_offset: u32, data_size: u32 }
/// Poll seq to detect new frames, read pixels from data_offset..data_offset+data_size.
/// Start capturing. Returns a handle object with shm info.
/// Call `getShmBuffer()` to get a zero-copy view into the shared memory.
#[napi]
pub fn start_capture(options: CaptureOptions) -> Result<ShmInfo> {
    // Video
    let shm_ptr;
    let shm_size;
    {
        let mut lock = CAPTURER
            .lock()
            .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        lock.take();
        let capturer = capture::Capturer::new(options.node_id, options.pipewire_fd, options.fps)
            .map_err(|e| Error::new(Status::GenericFailure, format!("capture error: {e}")))?;
        shm_ptr = capturer.shm_ptr();
        shm_size = capturer.shm_size();
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

    Ok(ShmInfo {
        shm_path: "/dev/shm/pipecap-frames".to_string(),
        shm_size: shm_size as u32,
        header_size: 32,
    })
}

/// Read accumulated audio samples. Returns null if not available.
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

/// Read the current frame header from shared memory. Returns [seq, width, height, dataOffset, dataSize].
/// The renderer uses this to know where to read pixels from the mmap'd buffer.
/// Returns null if not capturing or no frame available.
#[napi]
pub fn read_frame_info() -> Result<Option<Vec<u32>>> {
    let lock = CAPTURER
        .lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    match lock.as_ref() {
        None => Ok(None),
        Some(cap) => {
            let ptr = cap.shm_ptr();
            if ptr.is_null() { return Ok(None); }

            // Read header atomically
            let header = unsafe { &*(ptr as *const shm::ShmHeader) };
            let seq = header.seq.load(std::sync::atomic::Ordering::Acquire);
            if seq == 0 { return Ok(None); }

            let width = header.width.load(std::sync::atomic::Ordering::Relaxed);
            let height = header.height.load(std::sync::atomic::Ordering::Relaxed);
            let data_offset = header.data_offset.load(std::sync::atomic::Ordering::Relaxed);
            let data_size = header.data_size.load(std::sync::atomic::Ordering::Relaxed);

            Ok(Some(vec![seq as u32, width, height, data_offset, data_size]))
        }
    }
}

/// Read frame pixels from shared memory. Zero-copy — returns a Buffer view into mmap'd memory.
/// `offset` and `size` come from readFrameInfo().
#[napi]
pub fn read_frame_pixels(offset: u32, size: u32) -> Result<napi::bindgen_prelude::Buffer> {
    let lock = CAPTURER
        .lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    match lock.as_ref() {
        None => Err(Error::new(Status::GenericFailure, "not capturing")),
        Some(cap) => {
            let ptr = cap.shm_ptr();
            let shm_size = cap.shm_size();
            let end = offset as usize + size as usize;
            if end > shm_size {
                return Err(Error::new(Status::GenericFailure, "offset+size exceeds shm"));
            }
            // Copy from mmap — single memcpy, no allocation overhead
            let slice = unsafe {
                std::slice::from_raw_parts(ptr.add(offset as usize), size as usize)
            };
            Ok(slice.to_vec().into())
        }
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
