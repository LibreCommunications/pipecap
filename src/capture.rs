//! PipeWire video stream consumer.
//! Opens a PipeWire stream for a given node ID and captures RGBA frames.

use std::sync::{Arc, Mutex};

/// Raw frame data.
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// PipeWire video capturer. Runs the PipeWire main loop on a background
/// thread and stores the latest frame for the caller to read.
pub struct Capturer {
    latest_frame: Arc<Mutex<Option<RawFrame>>>,
    // TODO: hold the pw thread handle + stop signal
}

impl Capturer {
    /// Create a new capturer for the given PipeWire node.
    pub fn new(node_id: u32, width: u32, height: u32) -> anyhow::Result<Self> {
        let latest_frame: Arc<Mutex<Option<RawFrame>>> = Arc::new(Mutex::new(None));

        // TODO: implement PipeWire stream
        // 1. Create pw::main_loop::MainLoop on a new thread
        // 2. Create pw::stream::Stream connected to node_id
        // 3. Negotiate format: SPA_VIDEO_FORMAT_RGBA, width, height
        // 4. On process callback: copy buffer data → latest_frame
        // 5. Run the main loop

        let _ = (node_id, width, height);
        todo!("pipewire capture — next step")
    }

    /// Read the most recent frame. Returns None if no frame captured yet.
    pub fn read_frame(&self) -> Option<RawFrame> {
        let lock = self.latest_frame.lock().ok()?;
        lock.as_ref().map(|f| RawFrame {
            width: f.width,
            height: f.height,
            data: f.data.clone(),
        })
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        // TODO: signal the PipeWire thread to stop
    }
}
