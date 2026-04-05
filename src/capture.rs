//! PipeWire video stream consumer.
//! Connects to a portal's PipeWire remote and captures video frames
//! into shared memory for zero-copy access from the renderer.

use pipewire as pw;
use pw::spa;
use spa::pod::Pod;
use std::os::fd::{OwnedFd, FromRawFd};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use crate::shm::ShmBuffer;

pub struct Capturer {
    shm: Arc<ShmBuffer>,
    stop_flag: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capturer {
    pub fn new(node_id: u32, pipewire_fd: i32, fps: u32) -> anyhow::Result<Self> {
        // Max frame size: 8K @ 4bpp = 7680*4320*4 = ~133MB. Be generous.
        let max_frame = 7680 * 4320 * 4;
        let shm = Arc::new(ShmBuffer::new(max_frame)?);
        let stop_flag = Arc::new(AtomicBool::new(false));

        let shm_ref = shm.clone();
        let stop_ref = stop_flag.clone();

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_capture_loop(node_id, pipewire_fd, fps, shm_ref, stop_ref) {
                eprintln!("pipecap: capture loop error: {e}");
            }
        });

        Ok(Capturer { shm, stop_flag, thread: Some(thread) })
    }

    pub fn shm_size(&self) -> usize {
        self.shm.size()
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
    fps: u32,
    shm: Arc<ShmBuffer>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;

    let fd = unsafe { OwnedFd::from_raw_fd(pipewire_fd) };
    let core = context.connect_fd_rc(fd, None)?;
    eprintln!("pipecap: connected to PipeWire remote via fd {pipewire_fd}");

    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Video");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Screen");
    let stream = pw::stream::StreamRc::new(core, "pipecap-video", props)?;

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
            spa::param::video::VideoFormat::RGB
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
            spa::utils::Fraction { num: if fps > 0 { fps } else { 0 }, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 1000, denom: 1 }
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

    let process_count = Arc::new(AtomicU64::new(0));
    let pc = process_count.clone();

    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(|_, _, id, param| {
            if let Some(param) = param {
                if id == spa::param::ParamType::Format.as_raw() {
                    let mut vinfo = spa::param::video::VideoInfoRaw::default();
                    if vinfo.parse(param).is_ok() {
                        eprintln!("pipecap: negotiated {:?} {}x{} {}/{}fps",
                            vinfo.format(),
                            vinfo.size().width, vinfo.size().height,
                            vinfo.framerate().num, vinfo.framerate().denom);
                    }
                }
            }
        })
        .process(move |stream_ref, _| {
            let n = pc.fetch_add(1, Ordering::Relaxed);
            match stream_ref.dequeue_buffer() {
                None => {}
                Some(mut buffer) => {
                    let datas = buffer.datas_mut();
                    if let Some(data) = datas.first_mut() {
                        let chunk = data.chunk();
                        let size = chunk.size() as usize;
                        let stride = chunk.stride();

                        if size > 0 && stride > 0 {
                            let offset = chunk.offset() as usize;
                            let bpp = 4u32;
                            let w = stride as u32 / bpp;
                            let h = size as u32 / stride as u32;

                            if let Some(slice) = data.data() {
                                if offset + size <= slice.len() && w > 0 && h > 0 {
                                    shm.write_frame(w, h, stride as u32, &slice[offset..offset + size]);
                                }
                            }

                            if n < 3 {
                                eprintln!("pipecap: frame #{n} {w}x{h} stride={stride} type={:?}", data.type_());
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

    let mainloop_weak = mainloop.downgrade();
    let _timer = mainloop.loop_().add_timer(move |_| {
        if stop_flag.load(Ordering::Relaxed) {
            if let Some(ml) = mainloop_weak.upgrade() {
                ml.quit();
            }
        }
    });
    _timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );

    mainloop.run();

    Ok(())
}
