//! xdg-desktop-portal ScreenCast D-Bus client.
//! Calls CreateSession → SelectSources → Start to show the native picker.

use zbus::{Connection, proxy, zvariant::Value};
use std::collections::HashMap;

/// Result from the portal's Start method.
pub struct StreamInfo {
    pub node_id: u32,
    pub source_type: u32,
    pub width: i32,
    pub height: i32,
}

#[proxy(
    interface = "org.freedesktop.portal.ScreenCast",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait ScreenCast {
    fn create_session(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn select_sources(&self, session_handle: &zbus::zvariant::ObjectPath<'_>, options: HashMap<&str, Value<'_>>) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn start(&self, session_handle: &zbus::zvariant::ObjectPath<'_>, parent_window: &str, options: HashMap<&str, Value<'_>>) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

/// Show the native screen picker via xdg-desktop-portal.
/// `source_types` is a bitmask: 1=MONITOR, 2=WINDOW, 3=BOTH.
/// Returns None if the user cancelled.
pub async fn request_screen_cast(source_types: u32) -> anyhow::Result<Option<Vec<StreamInfo>>> {
    // TODO: implement full portal flow with signal handling
    // For now, this is the skeleton — the actual implementation needs
    // to listen for Response signals on the Request object paths.
    //
    // The zbus proxy approach above gives us typed method calls.
    // We need to:
    // 1. CreateSession → wait for Response signal → get session_handle
    // 2. SelectSources(session_handle, {types, cursor_mode}) → wait for Response
    // 3. Start(session_handle, "", {}) → wait for Response → parse streams
    //
    // Each portal method returns a Request object path. We subscribe to
    // the Response signal on that path to get the result.

    let _ = source_types;
    todo!("portal implementation — next step")
}
