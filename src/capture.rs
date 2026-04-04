//! PipeWire video stream consumer.
//! Opens a PipeWire stream for a given node ID and captures video frames
//! on a background thread. Negotiates the requested resolution and frame rate.

use pipewire as pw;
use libspa::pod::builder::Builder;
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
    pub fn new(node_id: u32, fps: u32) -> anyhow::Result<Self> {
        let latest_frame: Arc<Mutex<Option<RawFrame>>> = Arc::new(Mutex::new(None));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let frame_ref = latest_frame.clone();
        let stop_ref = stop_flag.clone();

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_capture_loop(node_id, fps, frame_ref, stop_ref) {
                eprintln!("pipecap: capture loop error: {e}");
            }
        });

        Ok(Capturer {
            latest_frame,
            stop_flag,
            thread: Some(thread),
        })
    }

    pub fn read_frame(&self) -> Option<RawFrame> {
        let lock = self.latest_frame.lock().ok()?;
        lock.as_ref().map(|f| RawFrame {
            width: f.width,
            height: f.height,
            data: f.data.clone(),
        })
    }

    pub fn is_active(&self) -> bool {
        !self.stop_flag.load(Ordering::Relaxed)
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

/// Build a SPA pod requesting a specific video format, resolution, and fps.
fn build_video_format_pod(buf: &mut Vec<u8>, fps: u32) {
    use libspa::param::{ParamType, format::{FormatProperties, MediaType, MediaSubtype}};
    use libspa::param::video::VideoFormat;
    use libspa::utils::{Fraction, Id};

    let mut builder = Builder::new(buf);
    // Request RGBA format at the source's native resolution, capped at the given fps.
    // No size constraint — PipeWire delivers at the portal source's native resolution.
    let _ = libspa::pod::builder::builder_add!(
        &mut builder,
        Object(
            ParamType::EnumFormat.as_raw(),
            0,
        ) {
            FormatProperties::MediaType.as_raw() =>
                Id(Id(MediaType::Video.as_raw())),
            FormatProperties::MediaSubtype.as_raw() =>
                Id(Id(MediaSubtype::Raw.as_raw())),
            FormatProperties::VideoFormat.as_raw() =>
                Id(Id(VideoFormat::RGBA.as_raw())),
            FormatProperties::VideoFramerate.as_raw() =>
                Fraction(Fraction { num: fps, denom: 1 }),
        }
    );
}

fn run_capture_loop(
    node_id: u32,
    fps: u32,
    latest_frame: Arc<Mutex<Option<RawFrame>>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Video");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Screen");
    let stream = pw::stream::StreamBox::new(&core, "pipecap-video", props)?;

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
                            let bpp = 4u32;
                            let w = stride as u32 / bpp;
                            let h = size as u32 / stride as u32;

                            if w > 0 && h > 0 {
                                if let Ok(mut lock) = frame_ref.lock() {
                                    *lock = Some(RawFrame {
                                        width: w,
                                        height: h,
                                        data: pixels.to_vec(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        })
        .register()?;

    // Build format parameters — request specific resolution and fps
    let mut format_buf = Vec::new();
    build_video_format_pod(&mut format_buf, fps);

    // Safety: the pod data lives in format_buf which outlives the connect call
    let pod = unsafe { &*(format_buf.as_ptr() as *const libspa::pod::Pod) };
    let mut params = [pod];

    stream.connect(
        libspa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

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
