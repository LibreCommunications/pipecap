//! PipeWire video stream consumer.
//! Connects to a portal's PipeWire remote and captures video frames.

use pipewire as pw;
use pw::spa;
use spa::pod::Pod;
use std::os::fd::{OwnedFd, FromRawFd};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

pub struct Capturer {
    latest_frame: Arc<Mutex<Option<RawFrame>>>,
    stop_flag: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capturer {
    pub fn new(node_id: u32, pipewire_fd: i32, _fps: u32) -> anyhow::Result<Self> {
        let latest_frame: Arc<Mutex<Option<RawFrame>>> = Arc::new(Mutex::new(None));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let frame_ref = latest_frame.clone();
        let stop_ref = stop_flag.clone();

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_capture_loop(node_id, pipewire_fd, frame_ref, stop_ref) {
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

fn run_capture_loop(
    node_id: u32,
    pipewire_fd: i32,
    latest_frame: Arc<Mutex<Option<RawFrame>>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;

    let fd = unsafe { OwnedFd::from_raw_fd(pipewire_fd) };
    let core = context.connect_fd(fd, None)?;
    eprintln!("pipecap: connected to PipeWire remote via fd {pipewire_fd}");

    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Video");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Screen");
    let stream = pw::stream::StreamBox::new(&core, "pipecap-video", props)?;

    // Format negotiation — accept multiple formats with ranges, exactly like
    // the pipewire-rs streams.rs example. This lets PipeWire pick the best
    // SHM-compatible format instead of forcing DmaBuf.
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBA,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::RGB,
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 7680, height: 4320 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 144, denom: 1 }
        ),
    );

    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).unwrap()];

    let frame_ref = latest_frame;
    let process_count = Arc::new(AtomicU64::new(0));
    let pc = process_count.clone();

    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream_ref, _| {
            let n = pc.fetch_add(1, Ordering::Relaxed);
            match stream_ref.dequeue_buffer() {
                None => {
                    if n < 3 { eprintln!("pipecap: out of buffers"); }
                }
                Some(mut buffer) => {
                    let datas = buffer.datas_mut();
                    if let Some(data) = datas.first_mut() {
                        let chunk = data.chunk();
                        let size = chunk.size() as usize;
                        let stride = chunk.stride();
                        let offset = chunk.offset() as usize;

                        if n < 3 {
                            eprintln!("pipecap: frame #{n} size={size} stride={stride} type={:?}", data.type_());
                        }

                        if size > 0 && stride > 0 {
                            let bpp = 4u32;
                            let w = stride as u32 / bpp;
                            let h = size as u32 / stride as u32;

                            if let Some(slice) = data.data() {
                                if offset + size <= slice.len() && w > 0 && h > 0 {
                                    if let Ok(mut lock) = frame_ref.lock() {
                                        *lock = Some(RawFrame {
                                            width: w,
                                            height: h,
                                            data: slice[offset..offset + size].to_vec(),
                                        });
                                    }
                                }
                            } else if n < 3 {
                                eprintln!("pipecap: data() returned None (type={:?})", data.type_());
                            }
                        }
                    }
                }
            }
        })
        .register()?;

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    eprintln!("pipecap: stream connected to node {node_id}");

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
