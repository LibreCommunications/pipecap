//! System audio capture via PipeWire sink monitor.

use pipewire as pw;
use pw::spa;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use super::{audio_format_params, MAX_SAMPLES, STREAM_FLAGS};

pub fn run(
    buffer: Arc<Mutex<Vec<f32>>>,
    channels_out: Arc<Mutex<u32>>,
    sample_rate_out: Arc<Mutex<u32>>,
    stop_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Music");
    props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");

    let stream = pw::stream::StreamRc::new(core, "pipecap-audio", props)?;

    let fc = Arc::new(AtomicU64::new(0));
    let fc2 = fc.clone();
    let ch_out = channels_out;
    let sr_out = sample_rate_out;

    let _listener = stream
        .add_local_listener_with_user_data(spa::param::audio::AudioInfoRaw::default())
        .param_changed(move |_, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() { return; }
            if ud.parse(param).is_ok() {
                eprintln!("pipecap-audio: negotiated {}ch {}Hz", ud.channels(), ud.rate());
                if let Ok(mut c) = ch_out.lock() { *c = ud.channels(); }
                if let Ok(mut r) = sr_out.lock() { *r = ud.rate(); }
            }
        })
        .process(move |stream_ref, _| {
            let n = fc2.fetch_add(1, Ordering::Relaxed);
            let Some(mut pw_buf) = stream_ref.dequeue_buffer() else { return };
            let Some(data) = pw_buf.datas_mut().first_mut() else { return };

            let size = data.chunk().size() as usize;
            if n < 3 { eprintln!("pipecap-audio: frame #{n} size={size}"); }

            let Some(samples) = data.data() else { return };
            if size == 0 || size > samples.len() { return; }

            let f32_slice: &[f32] = unsafe {
                std::slice::from_raw_parts(
                    samples.as_ptr() as *const f32,
                    size / std::mem::size_of::<f32>(),
                )
            };

            if let Ok(mut lock) = buffer.lock() {
                lock.extend_from_slice(f32_slice);
                if lock.len() > MAX_SAMPLES {
                    let excess = lock.len() - MAX_SAMPLES;
                    lock.drain(..excess);
                }
            }
        })
        .register()?;

    let bytes = audio_format_params();
    let mut params = [spa::pod::Pod::from_bytes(&bytes).unwrap()];
    stream.connect(spa::utils::Direction::Input, None, STREAM_FLAGS, &mut params)?;
    eprintln!("pipecap-audio: connected to sink monitor");

    let ml = mainloop.downgrade();
    let _timer = mainloop.loop_().add_timer(move |_| {
        if stop_flag.load(Ordering::Relaxed) {
            if let Some(m) = ml.upgrade() { m.quit(); }
        }
    });
    _timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );

    mainloop.run();
    Ok(())
}
