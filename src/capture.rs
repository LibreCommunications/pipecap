//! PipeWire video stream consumer.
//! Opens a PipeWire stream for a given node ID and captures RGBA frames
//! on a background thread.

use pipewire as pw;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

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
    stop_flag: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capturer {
    /// Create a new capturer for the given PipeWire node.
    /// The node_id must come from a portal session (user-consented).
    pub fn new(node_id: u32, _width: u32, _height: u32) -> anyhow::Result<Self> {
        let latest_frame: Arc<Mutex<Option<RawFrame>>> = Arc::new(Mutex::new(None));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let frame_ref = latest_frame.clone();
        let stop_ref = stop_flag.clone();

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_capture_loop(node_id, frame_ref, stop_ref) {
                eprintln!("pipecap: capture loop error: {e}");
            }
        });

        Ok(Capturer {
            latest_frame,
            stop_flag,
            thread: Some(thread),
        })
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
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// The actual PipeWire event loop, runs on a background thread.
fn run_capture_loop(
    node_id: u32,
    latest_frame: Arc<Mutex<Option<RawFrame>>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    // Create stream targeting the portal's PipeWire node
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Video");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Screen");
    let stream = pw::stream::StreamBox::new(&core, "pipecap-video", props)?;

    // Register stream listener — process callback copies frames.
    // Also check stop_flag in the process callback to quit the loop.
    let frame_ref = latest_frame;

    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream_ref, _user_data| {
            if let Some(mut buffer) = stream_ref.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if let Some(data) = datas.first_mut() {
                    let chunk = data.chunk();
                    let size = chunk.size() as usize;
                    let offset = chunk.offset() as usize;
                    let stride = chunk.stride();

                    if let Some(slice) = data.data() {
                        if size > 0 && offset + size <= slice.len() && stride > 0 {
                            let pixels = &slice[offset..offset + size];
                            let bpp = 4u32; // RGBA or BGRx = 4 bytes per pixel
                            let width = stride as u32 / bpp;
                            let height = size as u32 / stride as u32;

                            if width > 0 && height > 0 {
                                if let Ok(mut lock) = frame_ref.lock() {
                                    *lock = Some(RawFrame {
                                        width,
                                        height,
                                        data: pixels.to_vec(),
                                    });
                                }
                            }
                        }
                    }
                }
                // Buffer is automatically re-queued on Drop
            }
        })
        .register()?;

    // Connect to the portal-granted node — empty params = accept any format
    stream.connect(
        libspa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [],
    )?;

    // Periodically check the stop flag via a timer.
    // We can't move mainloop into the closure, so we use a raw pointer to quit.
    let mainloop_ptr = mainloop.as_raw_ptr();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop_flag.load(Ordering::Relaxed) {
            unsafe { pipewire_sys::pw_main_loop_quit(mainloop_ptr) };
        }
    });
    timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );

    mainloop.run();

    Ok(())
}
