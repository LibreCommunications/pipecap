//! PipeWire video stream consumer.
//! Connects to a portal's PipeWire remote and captures video frames
//! into shared memory for zero-copy access from the renderer.

use pipewire as pw;
use pw::spa;
use spa::pod::Pod;
use std::os::fd::OwnedFd;
use std::sync::{atomic::AtomicU64, Arc};

use crate::pw_util;
use crate::shm::ShmBuffer;

/// Message sent from the controller thread to the PipeWire mainloop thread.
enum CaptureMsg {
    Stop,
}

pub struct Capturer {
    shm: Arc<ShmBuffer>,
    sender: pw::channel::Sender<CaptureMsg>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capturer {
    pub fn new(node_id: u32, pipewire_fd: OwnedFd, fps: u32) -> anyhow::Result<Self> {
        let max_frame = 7680 * 4320 * 4; // 8K @ 4bpp
        let shm = Arc::new(ShmBuffer::new(max_frame)?);

        let (sender, receiver) = pw::channel::channel::<CaptureMsg>();

        let shm_ref = shm.clone();
        let thread = std::thread::Builder::new()
            .name("pipecap-video".into())
            .spawn(move || {
                if let Err(e) = run_capture_loop(node_id, pipewire_fd, fps, shm_ref, receiver) {
                    eprintln!("pipecap: capture loop error: {e}");
                }
            })?;

        Ok(Capturer {
            shm,
            sender,
            thread: Some(thread),
        })
    }

    pub fn shm_size(&self) -> usize {
        self.shm.size()
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        let _ = self.sender.send(CaptureMsg::Stop);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn video_format_params(fps: u32) -> Vec<u8> {
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
            Choice, Enum, Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBA,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::RGB
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice, Range, Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 7680, height: 4320 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice, Range, Fraction,
            spa::utils::Fraction { num: fps, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 1000, denom: 1 }
        ),
    );
    pw_util::serialize_pod_object(obj)
}

fn run_capture_loop(
    node_id: u32,
    pipewire_fd: OwnedFd,
    fps: u32,
    shm: Arc<ShmBuffer>,
    receiver: pw::channel::Receiver<CaptureMsg>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(pipewire_fd, None)?;
    eprintln!("pipecap: connected to PipeWire remote");

    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Video");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Screen");

    let stream = pw::stream::StreamRc::new(core, "pipecap-video", props)?;
    let values = video_format_params(fps);
    let pod = Pod::from_bytes(&values)
        .ok_or_else(|| anyhow::anyhow!("invalid pod bytes for video format"))?;
    let mut params = [pod];

    let frame_num = Arc::new(AtomicU64::new(0));
    let fc = frame_num.clone();

    // Negotiated video info shared with the process callback so we don't
    // have to derive width/height from chunk size + stride (which is wrong
    // for padded formats).
    let _listener = stream
        .add_local_listener_with_user_data(spa::param::video::VideoInfoRaw::default())
        .param_changed(|_, vi, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if vi.parse(param).is_ok() {
                eprintln!(
                    "pipecap: negotiated {:?} {}x{} {}/{}fps",
                    vi.format(),
                    vi.size().width,
                    vi.size().height,
                    vi.framerate().num,
                    vi.framerate().denom
                );
            }
        })
        .process(move |stream_ref, vi| {
            let n = fc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(mut buffer) = stream_ref.dequeue_buffer() else { return };
            let Some(data) = buffer.datas_mut().first_mut() else { return };

            let chunk = data.chunk();
            let (size, stride) = (chunk.size() as usize, chunk.stride());
            if size == 0 || stride <= 0 {
                return;
            }
            let offset = chunk.offset() as usize;

            let (w, h) = (vi.size().width, vi.size().height);
            // Fall back to size/stride only if format hasn't been negotiated yet.
            let (w, h) = if w == 0 || h == 0 {
                (stride as u32 / 4, size as u32 / stride as u32)
            } else {
                (w, h)
            };
            if w == 0 || h == 0 {
                return;
            }

            let Some(slice) = data.data() else { return };
            if offset.saturating_add(size) > slice.len() {
                return;
            }

            shm.write_frame(w, h, stride as u32, &slice[offset..offset + size]);

            if n < 3 {
                eprintln!("pipecap: frame #{n} {w}x{h} stride={stride}");
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

    // Cross-thread shutdown via pipewire channel — wakes the loop immediately
    // instead of polling a flag every 100ms.
    let mainloop_weak = mainloop.downgrade();
    let _recv = receiver.attach(mainloop.loop_(), move |msg| match msg {
        CaptureMsg::Stop => {
            if let Some(ml) = mainloop_weak.upgrade() {
                ml.quit();
            }
        }
    });

    mainloop.run();
    Ok(())
}
