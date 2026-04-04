//! xdg-desktop-portal ScreenCast client via ashpd.
//! Shows the native Wayland screen/window picker and returns PipeWire stream info.

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;

/// Result from the portal's Start method.
pub struct StreamInfo {
    pub node_id: u32,
    pub source_type: u32,
    pub width: i32,
    pub height: i32,
}

/// Show the native screen picker via xdg-desktop-portal.
/// `source_types` is a bitmask: 1=MONITOR, 2=WINDOW, 3=BOTH.
/// Returns None if the user cancelled.
pub async fn request_screen_cast(_source_types: u32) -> anyhow::Result<Option<Vec<StreamInfo>>> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;

    let st = SourceType::Monitor | SourceType::Window;

    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            st,
            false, // multiple
            None,  // restore_token
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

    Ok(Some(
        streams
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
    ))
}
