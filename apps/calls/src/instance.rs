//! Calls' own name on the session bus, `org.denialwm.Calls`. The running
//! Calls owns it; a second `calls <number>` asks it to show the number on the
//! keypad instead of opening another window.

use async_channel::Sender;
use zbus::proxy;

pub const NAME: &str = "org.denialwm.Calls";
pub const PATH: &str = "/org/denialwm/Calls";

/// A number the running Calls is asked to show, or none for the window as it
/// is, with the activation token to raise the window with.
pub type Open = (String, String);

/// What the running Calls is asked to do.
pub enum Request {
    Open(Open),
    /// Turn down the call ringing here, which the lock screen did.
    Decline,
}

/// `org.denialwm.Calls1` on Calls' bus name.
pub struct Opener(pub Sender<Request>);

#[zbus::interface(name = "org.denialwm.Calls1")]
impl Opener {
    async fn open(&self, number: String, activation_token: String) {
        // Closed only once the app is quitting.
        let _ = self.0.send(Request::Open((number, activation_token))).await;
    }

    async fn decline(&self) {
        let _ = self.0.send(Request::Decline).await;
    }
}

#[proxy(
    interface = "org.denialwm.Calls1",
    default_service = "org.denialwm.Calls",
    default_path = "/org/denialwm/Calls"
)]
pub trait Instance {
    /// Shows `number` on the keypad, or the window as it is when `number` is
    /// empty, and raises the window with `activation_token` (an
    /// xdg-activation token; empty when there is none).
    fn open(&self, number: &str, activation_token: &str) -> zbus::Result<()>;
}
