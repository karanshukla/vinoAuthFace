//! Desktop notifications.

use std::collections::HashMap;

/// Desktop notifications over the session bus. Best effort: without a
/// notification server the tooltip still carries the same text.
pub struct Notifier {
    pub(super) bus: Option<zbus::blocking::Connection>,
}

impl Notifier {
    pub fn new() -> Notifier {
        Notifier { bus: zbus::blocking::Connection::session().ok() }
    }

    /// Show, or with a non-zero `replaces` update, a notification. Returns
    /// its ID for the next update.
    pub fn send(&self, replaces: u32, summary: &str, body: &str) -> u32 {
        let Some(bus) = &self.bus else { return 0 };
        let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
        let reply = bus.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("vinoAuthFace", replaces, "vinoauthface", summary, body, Vec::<&str>::new(), hints, -1i32),
        );
        reply.ok().and_then(|m| m.body().deserialize::<u32>().ok()).unwrap_or(0)
    }
}
