//! xdg-desktop-portal ScreenCast client via ashpd.
//!
//! Shows the native Wayland screen/window picker and returns PipeWire stream
//! info plus an owned PipeWire remote fd. The portal `Session` is kept alive
//! for the duration of the capture and explicitly `close()`d on teardown —
//! this is what clears KDE's screen-share indicator (the indicator tracks the
//! portal session, not the PipeWire stream).

use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::{PersistMode, ResponseError, Session};
use ashpd::enumflags2::BitFlags;
use std::os::fd::OwnedFd;
use std::path::PathBuf;

/// Best-effort cleanup of any restore-token file left behind by an earlier
/// build that experimented with `PersistMode::ExplicitlyRevoked`. Called once
/// from `request_screen_cast` so users upgrading don't get a stale silent
/// re-pick on first launch. Safe to remove in a future release.
fn purge_legacy_restore_token() {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")));
    if let Some(base) = base {
        let _ = std::fs::remove_file(base.join("pipecap").join("restore-token"));
    }
}

pub struct StreamInfo {
    pub node_id: u32,
    pub source_type: u32,
    pub width: i32,
    pub height: i32,
}

/// State that must outlive the portal request: dropping the proxy or session
/// without calling `close()` leaves the portal-side session dangling and on
/// KDE leaves a stale screen-share indicator.
pub struct PortalHandle {
    pub streams: Vec<StreamInfo>,
    pub pipewire_fd: Option<OwnedFd>,
    proxy: Screencast<'static>,
    session: Option<Session<'static, Screencast<'static>>>,
}

impl PortalHandle {
    /// Take the PipeWire fd, leaving `None` behind. The caller becomes
    /// responsible for closing it (typically by handing it to a PipeWire
    /// `Context::connect_fd_rc`, which takes ownership).
    pub fn take_fd(&mut self) -> Option<OwnedFd> {
        self.pipewire_fd.take()
    }

    /// Close the portal session. Idempotent. Must be called from a tokio
    /// context. Drops the proxy as well.
    pub async fn close(mut self) {
        if let Some(session) = self.session.take()
            && let Err(e) = session.close().await {
                eprintln!("pipecap: portal session.close() failed: {e}");
            }
        // proxy drops here
        drop(self.proxy);
    }
}

/// Show the native screen picker via xdg-desktop-portal.
/// Returns `Ok(None)` if the user cancelled.
pub async fn request_screen_cast(source_types: u32) -> anyhow::Result<Option<PortalHandle>> {
    purge_legacy_restore_token();
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;

    let st: BitFlags<SourceType> = match source_types {
        1 => SourceType::Monitor.into(),
        2 => SourceType::Window.into(),
        _ => SourceType::Monitor | SourceType::Window,
    };

    if let Err(e) = proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            st,
            false,
            None,
            PersistMode::DoNot,
        )
        .await
    {
        let _ = session.close().await;
        return Err(e.into());
    }

    let response = match proxy.start(&session, None).await {
        Ok(req) => match req.response() {
            Ok(r) => r,
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
                let _ = session.close().await;
                return Ok(None);
            }
            Err(e) => {
                let _ = session.close().await;
                return Err(e.into());
            }
        },
        Err(e) => {
            let _ = session.close().await;
            return Err(e.into());
        }
    };

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
                width: w,
                height: h,
            }
        })
        .collect();

    if streams.is_empty() {
        let _ = session.close().await;
        return Ok(None);
    }

    let pw_fd: OwnedFd = match proxy.open_pipe_wire_remote(&session).await {
        Ok(fd) => fd,
        Err(e) => {
            let _ = session.close().await;
            return Err(e.into());
        }
    };

    Ok(Some(PortalHandle {
        streams,
        pipewire_fd: Some(pw_fd),
        proxy,
        session: Some(session),
    }))
}
