//! xdg-desktop-portal ScreenCast client via ashpd.
//! Shows the native Wayland screen/window picker and returns PipeWire stream info.

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use ashpd::enumflags2::BitFlags;
use std::os::fd::{OwnedFd, IntoRawFd};

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

/// Show the native screen picker via xdg-desktop-portal.
/// Returns the streams + PipeWire remote fd, or None if cancelled.
pub async fn request_screen_cast(source_types: u32) -> anyhow::Result<Option<PortalResult>> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;

    let st: BitFlags<SourceType> = if source_types == 1 {
        SourceType::Monitor.into()
    } else if source_types == 2 {
        SourceType::Window.into()
    } else {
        SourceType::Monitor | SourceType::Window
    };

    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            st,
            false,
            None,
            PersistMode::DoNot,
        )
        .await?;

    let response = proxy
        .start(&session, None)
        .await?
        .response()?;

    let streams = response.streams();
    if streams.is_empty() {
        return Ok(None);
    }

    let pw_fd: OwnedFd = proxy.open_pipe_wire_remote(&session).await?;
    let raw_fd = pw_fd.into_raw_fd();

    Ok(Some(PortalResult {
        streams: streams
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
            .collect(),
        pipewire_fd: raw_fd,
    }))
}
