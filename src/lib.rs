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
static PORTAL_HANDLE: Mutex<Option<portal::PortalHandle>> = Mutex::new(None);
/// Exclude lists from the most recent `start_capture`, reused by
/// `set_audio_target("system")` so a runtime source switch never
/// downgrades to a non-filtering capture and re-introduces the self-echo.
static LAST_EXCLUDE_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static LAST_EXCLUDE_APP_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());

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
}

#[napi(object)]
pub struct CaptureOptions {
    pub node_id: u32,
    pub fps: u32,
    pub audio: bool,
    /// 1 = monitor, 2 = window.
    pub source_type: u32,
    /// PIDs whose audio output should be excluded from system audio capture.
    /// Pass `[process.pid, ...rendererPids]` to keep the recording app from
    /// hearing itself in its own screen-share. Ignored when `sourceType=2`
    /// resolves to a successful per-app capture.
    pub exclude_pids: Option<Vec<u32>>,
    /// Application names whose audio output should be excluded from system
    /// audio capture. Matched case-insensitively against PipeWire's
    /// `application.name` property on the source node. Use this when PID
    /// filtering can't reach the right process — e.g. apps connected via
    /// the PulseAudio compatibility layer (`pipewire-pulse`), which proxies
    /// many clients onto a single PipeWire Client and drops per-client
    /// pids in the process. Set `PULSE_PROP="application.name=Foo"` in the
    /// host app before any audio code initializes so PA stamps the right
    /// name on every stream you emit.
    pub exclude_app_names: Option<Vec<String>>,
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
    // If a previous picker call left a handle around without start_capture
    // being called (e.g. user pressed back), close it before opening a new one.
    let stale = PORTAL_HANDLE.lock().map_err(err)?.take();
    if let Some(stale) = stale {
        close_portal_handle_blocking(stale);
    }

    let handle = portal::request_screen_cast(source_types)
        .await
        .map_err(|e| err(format!("portal: {e}")))?;

    let Some(handle) = handle else { return Ok(None) };

    let result = PickerResult {
        streams: handle
            .streams
            .iter()
            .map(|s| PortalStream {
                node_id: s.node_id,
                source_type: s.source_type,
                width: s.width,
                height: s.height,
            })
            .collect(),
    };

    *PORTAL_HANDLE.lock().map_err(err)? = Some(handle);
    Ok(Some(result))
}

/// Start capture. Auto-detects audio target for window captures.
/// Returns `detectedApp` so the frontend knows whether to show a picker.
///
/// Must be called after a successful `showPicker()` — uses the PipeWire fd
/// owned by the most recent portal handle.
#[napi]
pub fn start_capture(options: CaptureOptions) -> Result<CaptureInfo> {
    let pw_fd = {
        let mut lock = PORTAL_HANDLE.lock().map_err(err)?;
        let handle = lock
            .as_mut()
            .ok_or_else(|| err("start_capture called without a prior showPicker"))?;
        handle
            .take_fd()
            .ok_or_else(|| err("PipeWire fd already consumed; call showPicker again"))?
    };

    let shm_size = {
        let mut lock = CAPTURER.lock().map_err(err)?;
        lock.take();
        let cap = capture::Capturer::new(options.node_id, pw_fd, options.fps)
            .map_err(|e| err(format!("capture: {e}")))?;
        let size = cap.shm_size();
        *lock = Some(cap);
        size
    };

    let mut detected_app: Option<String> = None;

    if options.audio {
        let mut lock = AUDIO_CAPTURER.lock().map_err(err)?;
        lock.take();

        let exclude_pids: Vec<u32> = options
            .exclude_pids
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| *p != 0)
            .collect();
        let exclude_app_names: Vec<String> = options
            .exclude_app_names
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();

        // Persist for set_audio_target("system") to reuse — see comment
        // on LAST_EXCLUDE_PIDS.
        if let Ok(mut g) = LAST_EXCLUDE_PIDS.lock() {
            *g = exclude_pids.clone();
        }
        if let Ok(mut g) = LAST_EXCLUDE_APP_NAMES.lock() {
            *g = exclude_app_names.clone();
        }

        let has_exclusion = !exclude_pids.is_empty() || !exclude_app_names.is_empty();
        let system_target = || -> audio::AudioTarget {
            if has_exclusion {
                audio::AudioTarget::SystemExcludeSelf {
                    pids: exclude_pids.clone(),
                    app_names: exclude_app_names.clone(),
                }
            } else {
                audio::AudioTarget::System
            }
        };

        let target = match options.source_type {
            2 => {
                // Window: try auto-detect, fall back to system (with PID
                // exclusion if requested, so we don't loop our own audio).
                detected_app = audio::resolve::resolve_app_name(options.node_id);
                match &detected_app {
                    Some(_) => audio::AudioTarget::AppFromVideoNode(options.node_id),
                    None => system_target(),
                }
            }
            _ => system_target(),
        };

        *lock = Some(
            audio::AudioCapturer::new(target).map_err(|e| err(format!("audio: {e}")))?,
        );
    }

    Ok(CaptureInfo {
        shm_path: shm::shm_public_path(),
        shm_size: shm_size as u32,
        header_size: 32,
        width: 0,
        height: 0,
        detected_app,
    })
}

/// Switch audio target at runtime. Recreates the audio capturer.
/// `target` is `"system"`, `"none"`, or an app name. The `"system"` path
/// reuses the exclude lists persisted by the last `start_capture` so
/// runtime switches can't bypass self-exclusion.
#[napi]
pub fn set_audio_target(target: String) -> Result<()> {
    let mut lock = AUDIO_CAPTURER.lock().map_err(err)?;

    // Drop existing capturer first so the new one starts on a clean slate.
    lock.take();

    if target == "none" {
        return Ok(());
    }

    let audio_target = if target == "system" {
        let pids = LAST_EXCLUDE_PIDS
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let app_names = LAST_EXCLUDE_APP_NAMES
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        if pids.is_empty() && app_names.is_empty() {
            audio::AudioTarget::System
        } else {
            audio::AudioTarget::SystemExcludeSelf { pids, app_names }
        }
    } else {
        audio::AudioTarget::AppByName(target)
    };

    *lock = Some(
        audio::AudioCapturer::new(audio_target).map_err(|e| err(format!("audio: {e}")))?,
    );
    Ok(())
}

/// List applications currently producing audio.
#[napi]
pub fn list_audio_apps() -> Result<Vec<AudioAppInfo>> {
    let apps = audio::resolve::list_audio_apps().map_err(|e| err(format!("list apps: {e}")))?;
    Ok(apps
        .into_iter()
        .map(|a| AudioAppInfo {
            name: a.name,
            binary: a.binary,
        })
        .collect())
}

#[napi]
pub fn read_audio() -> Result<Option<AudioChunk>> {
    let lock = AUDIO_CAPTURER.lock().map_err(err)?;
    let Some(cap) = lock.as_ref() else { return Ok(None) };
    let Some(buf) = cap.read_audio() else { return Ok(None) };

    // Fast path: a single memcpy from f32 -> bytes. Little-endian targets
    // (x86_64, aarch64, riscv64, the only ones this addon ships for) match
    // f32::to_le_bytes byte-for-byte, so we can reinterpret the slice and
    // copy in one shot. The previous flat_map(to_le_bytes) allocated and
    // copied four bytes per sample with bounds checks — measurably hot at
    // 48kHz × 2 channels.
    const { assert!(cfg!(target_endian = "little"), "pipecap requires a little-endian target") };
    let byte_len = buf.data.len() * std::mem::size_of::<f32>();
    let byte_slice: &[u8] =
        unsafe { std::slice::from_raw_parts(buf.data.as_ptr() as *const u8, byte_len) };
    let bytes: Vec<u8> = byte_slice.to_vec();
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
    // 1. Stop the PipeWire streams (drops the OwnedFd held by the video loop).
    if let Ok(mut lock) = CAPTURER.lock() {
        lock.take();
    }
    if let Ok(mut lock) = AUDIO_CAPTURER.lock() {
        lock.take();
    }
    // 2. Close the portal session — this is what clears the KDE indicator.
    let handle = PORTAL_HANDLE.lock().ok().and_then(|mut l| l.take());
    if let Some(handle) = handle {
        close_portal_handle_blocking(handle);
    }
}

/// Close a portal handle from a sync context. We can't `await` here, so we
/// spin up a tiny tokio runtime on a dedicated thread (the portal proxy is
/// `Send`) and wait for it to finish — closing the session is fast and
/// blocking is acceptable on the explicit stop path.
fn close_portal_handle_blocking(handle: portal::PortalHandle) {
    let join = std::thread::Builder::new()
        .name("pipecap-portal-close".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("pipecap: portal close runtime: {e}");
                    return;
                }
            };
            rt.block_on(handle.close());
        });
    if let Ok(j) = join {
        let _ = j.join();
    }
}
