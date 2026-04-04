mod portal;
mod capture;

use napi_derive::napi;
use napi::{Error, Result, Status};
use std::sync::Mutex;

static CAPTURER: Mutex<Option<capture::Capturer>> = Mutex::new(None);

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

/// Start capturing video frames from a PipeWire node.
/// The node_id must come from show_picker() (portal-consented).
#[napi]
pub fn start_capture(node_id: u32, width: u32, height: u32) -> Result<()> {
    let mut lock = CAPTURER
        .lock()
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    if lock.is_some() {
        lock.take(); // Stop previous capture
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

/// Stop capturing and release PipeWire resources.
#[napi]
pub fn stop_capture() {
    if let Ok(mut lock) = CAPTURER.lock() {
        lock.take();
    }
}
