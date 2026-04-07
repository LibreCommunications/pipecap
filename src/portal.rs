//! xdg-desktop-portal ScreenCast client via ashpd.

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use ashpd::enumflags2::BitFlags;
use std::os::fd::{IntoRawFd, OwnedFd};

pub struct StreamInfo {
    pub node_id: u32,
    pub source_type: u32,
    pub width: i32,
    pub height: i32,
}

pub struct PortalResult {
    pub streams: Vec<StreamInfo>,
    pub pipewire_fd: i32,
}

/// Show the native picker, open a PipeWire remote, and return the fd and
/// stream metadata.
///
/// The portal `Session` is closed from a detached task ~1s after this
/// function returns. Closing the session is what releases the compositor's
/// screen-recording indicator, but it must happen *after* `start_capture`
/// has connected to the PipeWire node — closing earlier tears the node
/// down before frames flow. Holding the session open for the lifetime of
/// the capture (e.g. in a static) prevents PipeWire format negotiation
/// from completing, so deferred close is the only configuration that
/// satisfies both constraints.
pub async fn request_screen_cast(source_types: u32) -> anyhow::Result<Option<PortalResult>> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;

    let sources: BitFlags<SourceType> = match source_types {
        1 => SourceType::Monitor.into(),
        2 => SourceType::Window.into(),
        _ => SourceType::Monitor | SourceType::Window,
    };

    proxy
        .select_sources(&session, CursorMode::Embedded, sources, false, None, PersistMode::DoNot)
        .await?;

    let response = proxy.start(&session, None).await?.response()?;
    let streams: Vec<StreamInfo> = response
        .streams()
        .iter()
        .map(|s| {
            let (w, h) = s.size().unwrap_or((0, 0));
            StreamInfo {
                node_id: s.pipe_wire_node_id(),
                source_type: match s.source_type() {
                    Some(SourceType::Monitor) => 1,
                    Some(SourceType::Window) => 2,
                    _ => 0,
                },
                width: w as i32,
                height: h as i32,
            }
        })
        .collect();

    if streams.is_empty() {
        let _ = session.close().await;
        return Ok(None);
    }

    let fd: OwnedFd = proxy.open_pipe_wire_remote(&session).await?;
    let raw_fd = fd.into_raw_fd();

    tokio::spawn(async move {
        let _proxy = proxy;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let _ = session.close().await;
    });

    Ok(Some(PortalResult { streams, pipewire_fd: raw_fd }))
}
