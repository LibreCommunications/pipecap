//! Shared PipeWire utilities used by both audio and video capture.

use pipewire as pw;
use pw::spa;
use std::cell::Cell;
use std::rc::Rc;

/// Serialize a SPA pod Object into bytes suitable for `Pod::from_bytes`.
pub fn serialize_pod_object(obj: spa::pod::Object) -> Vec<u8> {
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .expect("pod serialization")
    .0
    .into_inner()
}

/// Block until PipeWire has processed all pending operations.
pub fn do_roundtrip(mainloop: &pw::main_loop::MainLoopRc, core: &pw::core::CoreRc) {
    let done = Rc::new(Cell::new(false));
    let done_clone = done.clone();
    let loop_clone = mainloop.clone();
    let pending = core.sync(0).expect("sync failed");
    let _listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                done_clone.set(true);
                loop_clone.quit();
            }
        })
        .register();
    while !done.get() {
        mainloop.run();
    }
}
