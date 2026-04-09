//! System audio capture via PipeWire sink monitor.

use pipewire as pw;
use pw::spa;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use super::{bytes_as_f32, connect_stream_to, mix::MixBuffer, AudioCapturer, AudioCtl};

pub fn run(mix: Arc<MixBuffer>, receiver: pw::channel::Receiver<AudioCtl>) -> anyhow::Result<()> {
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

    let mix_p = mix.clone();
    let mix_f = mix.clone();
    let fc = Arc::new(AtomicU64::new(0));

    let _listener = stream
        .add_local_listener_with_user_data(spa::param::audio::AudioInfoRaw::default())
        .param_changed(move |_, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if ud.parse(param).is_ok() {
                eprintln!(
                    "pipecap-audio: negotiated {}ch {}Hz",
                    ud.channels(),
                    ud.rate()
                );
                mix_f.set_format(ud.channels(), ud.rate());
            }
        })
        .process(move |stream_ref, _| {
            let n = fc.fetch_add(1, Ordering::Relaxed);
            let Some(mut pw_buf) = stream_ref.dequeue_buffer() else { return };
            let Some(data) = pw_buf.datas_mut().first_mut() else { return };

            let size = data.chunk().size() as usize;
            if n < 3 {
                eprintln!("pipecap-audio: frame #{n} size={size}");
            }

            let Some(samples) = data.data() else { return };
            if size == 0 || size > samples.len() {
                return;
            }
            let Some(f32_slice) = bytes_as_f32(&samples[..size]) else {
                eprintln!("pipecap-audio: dropping misaligned f32 chunk ({size} bytes)");
                return;
            };
            mix_p.push(AudioCapturer::SINGLE_SOURCE, f32_slice);
        })
        .register()?;

    connect_stream_to(&stream, None);
    eprintln!("pipecap-audio: connected to sink monitor");

    let mainloop_weak = mainloop.downgrade();
    let _recv = receiver.attach(mainloop.loop_(), move |msg| match msg {
        AudioCtl::Stop => {
            if let Some(m) = mainloop_weak.upgrade() {
                m.quit();
            }
        }
    });

    mainloop.run();
    Ok(())
}
