//! The phone's calls: dmail's `org.dmail.Voice1` (Phonewave's
//! services/modem/dmail/DESIGN.md) and callaudio's microphone mute.

use zbus::proxy;

/// Id, number (empty when withheld), direction (`in`, `out`), state (dialing,
/// alerting, incoming, waiting, active, held, ending), connected time.
pub type CallRow = (u8, String, String, String, i64);

#[proxy(
    interface = "org.dmail.Voice1",
    default_service = "org.dmail",
    default_path = "/org/dmail"
)]
pub trait Voice1 {
    #[zbus(property)]
    fn calls(&self) -> zbus::Result<Vec<CallRow>>;

    /// Returns the new call's id.
    fn dial(&self, number: &str) -> zbus::Result<u8>;

    fn answer(&self, id: u8) -> zbus::Result<()>;

    /// Ends a call, or turns down an incoming one.
    fn hangup(&self, id: u8) -> zbus::Result<()>;

    fn send_dtmf(&self, id: u8, digits: &str) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.callaudio.CallAudio1",
    default_service = "org.callaudio",
    default_path = "/org/callaudio"
)]
pub trait CallAudio1 {
    /// Only during a call; each call starts unmuted.
    #[zbus(property)]
    fn set_muted(&self, muted: bool) -> zbus::Result<()>;

    /// Only during a call; each call starts on the earpiece.
    #[zbus(property)]
    fn set_speaker(&self, speaker: bool) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
pub trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}
