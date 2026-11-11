//! Rings and vibrates while a call comes in, wakes the screen and opens Calls
//! on it; dmail itself never rings. Turns the screen off while a call on the
//! earpiece is held to the ear. While the session is locked, posts the call as
//! a notification of Calls, whose actions the lock screen offers without the
//! PIN; while it stands, Denial shows Calls' window over the lock screen.
//! Sleeps on D-Bus, needs no window, and is started at login by
//! calls-ringer.service. callaudio leaves the sound card on its normal profile
//! while a call rings here, so the ringtone plays.

use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_lite::{StreamExt, future};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output::WlOutput, wl_registry::WlRegistry};
use wayland_client::{Connection as Wayland, Dispatch, QueueHandle};
use wayland_protocols_wlr::output_power_management::v1::client::{
    zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
    zwlr_output_power_v1::{Mode, ZwlrOutputPowerV1},
};
use denial_contacts::Contacts;
use std::collections::HashMap;
use zbus::zvariant::{OwnedObjectPath, Value};
use zbus::{Connection, proxy};

const RINGTONE: &str = "/usr/share/sounds/freedesktop/stereo/phone-incoming-call.oga";

#[proxy(
    interface = "org.dmail.Voice1",
    default_service = "org.dmail",
    default_path = "/org/dmail"
)]
trait Voice1 {
    #[zbus(property)]
    fn calls(&self) -> zbus::Result<Vec<Call>>;

    fn answer(&self, id: u8) -> zbus::Result<()>;

    fn hangup(&self, id: u8) -> zbus::Result<()>;
}

/// Id, number, direction, state, connected time.
type Call = (u8, String, String, String, i64);

#[proxy(
    interface = "org.denialwm.Calls1",
    default_service = "org.denialwm.Calls",
    default_path = "/org/denialwm/Calls"
)]
trait App {
    fn decline(&self) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Logind {
    fn list_sessions(&self) -> zbus::Result<Vec<(String, u32, String, String, OwnedObjectPath)>>;
}

#[proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Session {
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.steinerd.Sensors1",
    default_service = "org.steinerd",
    default_path = "/org/steinerd"
)]
trait Sensors1 {
    fn claim(&self, sensor: &str) -> zbus::Result<()>;
    fn release(&self, sensor: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn near(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.callaudio.CallAudio1",
    default_service = "org.callaudio",
    default_path = "/org/callaudio"
)]
trait CallAudio1 {
    #[zbus(property)]
    fn speaker(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn set_speaker(&self, speaker: bool) -> zbus::Result<()>;
}

fn main() -> ExitCode {
    match future::block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("calls-ringer: {error}");
            ExitCode::FAILURE
        }
    }
}

enum Event {
    Calls(Vec<Call>),
    Near(bool),
    Speaker(bool),
    Locked(bool),
    Action(u32, String),
    Closed,
}

async fn run() -> zbus::Result<()> {
    let system = Connection::system().await?;
    let voice = Voice1Proxy::new(&system).await?;
    let sensors = Sensors1Proxy::new(&system).await?;
    let session = Connection::session().await?;
    let audio = CallAudio1Proxy::new(&session).await?;
    let app = AppProxy::new(&session).await?;
    let notifications = NotificationsProxy::new(&session).await?;
    let mut actions = notifications.receive_action_invoked().await?;
    let mut seat_session = seat_session(&system).await;
    let mut locked_changes = match &seat_session {
        Some(session) => Some(session.receive_locked_hint_changed().await),
        None => None,
    };
    let mut locked = match &seat_session {
        Some(session) => session.locked_hint().await.unwrap_or(false),
        None => false,
    };
    let mut posted: Option<(u32, CallCard)> = None;
    let mut list_now: Vec<Call> = Vec::new();
    let mut calls = voice.receive_calls_changed().await;
    let mut near_changes = sensors.receive_near_changed().await;
    let mut speaker_changes = audio.receive_speaker_changed().await;
    let mut screen: Option<Screen> = None;
    let mut ringing: Option<Arc<AtomicBool>> = None;
    let (mut in_call, mut near, mut speaker, mut dark) = (false, false, false, false);
    let mut event = Event::Calls(voice.calls().await.unwrap_or_default());
    loop {
        match event {
            Event::Calls(list) => {
                // Denial restarting makes a new session; read the lock from
                // the one there now, and follow that one from here on.
                if let Some(current) = self::seat_session(&system).await {
                    if seat_session.as_ref().map(|session| session.inner().path())
                        != Some(current.inner().path())
                    {
                        locked_changes = Some(current.receive_locked_hint_changed().await);
                        seat_session = Some(current);
                    }
                    if let Some(session) = &seat_session {
                        locked = session.locked_hint().await.unwrap_or(locked);
                    }
                }
                let talking = list
                    .iter()
                    .any(|call| matches!(call.3.as_str(), "dialing" | "alerting" | "active" | "held"));
                let rang = ringing.is_some();
                follow(list.clone(), &mut ringing);
                // Wakes a dark screen; a ringing call is never held to an ear.
                if ringing.is_some() && !rang && !dark {
                    set_screen(&mut screen, true);
                }
                list_now = list;
                if talking != in_call {
                    in_call = talking;
                    // Only a report from this call counts as near.
                    near = false;
                    speaker = audio.speaker().await.unwrap_or(false);
                    let claimed = if talking {
                        sensors.claim("proximity").await
                    } else {
                        sensors.release("proximity").await
                    };
                    if let Err(error) = claimed {
                        eprintln!("calls-ringer: proximity sensor: {error}");
                    }
                }
            }
            Event::Near(value) => near = value,
            Event::Speaker(value) => speaker = value,
            Event::Locked(value) => {
                eprintln!("calls-ringer: locked {value}");
                locked = value;
            }
            Event::Action(id, key) => {
                if posted.as_ref().is_some_and(|(posted, _)| *posted == id) {
                    let call = posted.as_ref().map_or(0, |(_, card)| card.call);
                    let done = match key.as_str() {
                        "answer" => voice.answer(call).await,
                        "decline" => match app.decline().await {
                            Ok(()) => Ok(()),
                            Err(_) => voice.hangup(call).await,
                        },
                        "end" => voice.hangup(call).await,
                        "speaker" => audio.set_speaker(!speaker).await,
                        _ => Ok(()),
                    };
                    if let Err(error) = done {
                        eprintln!("calls-ringer: {key}: {error}");
                    }
                }
            }
            Event::Closed => return Ok(()),
        }
        let want_dark = in_call && near && !speaker;
        if want_dark != dark {
            dark = want_dark;
            eprintln!("calls-ringer: screen {}", if dark { "off" } else { "on" });
            set_screen(&mut screen, !dark);
        }
        let card = if locked { CallCard::of(&list_now, speaker) } else { None };
        if card.as_ref() != posted.as_ref().map(|(_, card)| card) {
            posted = show_card(&notifications, posted.take(), card).await;
        }
        event = future::or(
            async {
                match calls.next().await {
                    Some(change) => Event::Calls(change.get().await.unwrap_or_default()),
                    None => Event::Closed,
                }
            },
            future::or(
                future::or(
                    async {
                        match actions.next().await {
                            Some(signal) => match signal.args() {
                                Ok(args) => Event::Action(*args.id(), args.action_key().clone()),
                                Err(_) => future::pending().await,
                            },
                            None => future::pending().await,
                        }
                    },
                    async {
                        match locked_changes.as_mut() {
                            Some(changes) => match changes.next().await {
                                Some(change) => Event::Locked(change.get().await.unwrap_or(false)),
                                None => future::pending().await,
                            },
                            None => future::pending().await,
                        }
                    },
                ),
                future::or(async {
                    match near_changes.next().await {
                        Some(change) => Event::Near(change.get().await.unwrap_or(false)),
                        None => future::pending().await,
                    }
                },
                async {
                    match speaker_changes.next().await {
                        Some(change) => Event::Speaker(change.get().await.unwrap_or(false)),
                        None => future::pending().await,
                    }
                }),
            ),
        )
        .await;
    }
}

/// The session on the phone's seat, whose lock state deniald keeps in logind.
async fn seat_session(system: &Connection) -> Option<SessionProxy<'static>> {
    let uid = unsafe { libc::getuid() };
    let sessions = LogindProxy::new(system).await.ok()?.list_sessions().await.ok()?;
    let (.., path) = sessions
        .into_iter()
        .find(|(_, session_uid, _, seat, _)| *session_uid == uid && seat == "seat0")?;
    SessionProxy::builder(system).path(path).ok()?.build().await.ok()
}

/// What the lock screen shows of a call.
#[derive(PartialEq)]
struct CallCard {
    call: u8,
    number: String,
    ringing: bool,
    speaker: bool,
}

impl CallCard {
    fn of(calls: &[Call], speaker: bool) -> Option<Self> {
        let ringing = calls.iter().find(|call| call.3 == "incoming");
        let call = ringing.or_else(|| {
            calls
                .iter()
                .find(|call| matches!(call.3.as_str(), "dialing" | "alerting" | "active" | "held"))
        })?;
        Some(Self {
            call: call.0,
            number: call.1.clone(),
            ringing: ringing.is_some(),
            speaker,
        })
    }

    fn name(&self) -> String {
        if self.number.is_empty() {
            return "Unknown caller".into();
        }
        // Read afresh: calls are rare and the folder can change.
        let contacts = Contacts::folder().map(|folder| Contacts::load(&folder));
        contacts
            .as_ref()
            .and_then(|contacts| Some(contacts.all()[contacts.find(&self.number)?].name.clone()))
            .unwrap_or_else(|| self.number.clone())
    }
}

/// Posts the card in place of the last one, or takes the last one away.
async fn show_card(
    notifications: &NotificationsProxy<'_>,
    posted: Option<(u32, CallCard)>,
    card: Option<CallCard>,
) -> Option<(u32, CallCard)> {
    let replaces = posted.as_ref().map_or(0, |(id, _)| *id);
    let Some(card) = card else {
        if replaces != 0
            && let Err(error) = notifications.close_notification(replaces).await
        {
            eprintln!("calls-ringer: closing the call's notification: {error}");
        }
        return None;
    };
    let (category, body, actions): (_, _, &[&str]) = if card.ringing {
        ("call.incoming", "Incoming call", &["decline", "Decline", "answer", "Answer"])
    } else if card.speaker {
        ("call", "In a call", &["speaker", "Earpiece", "end", "End"])
    } else {
        ("call", "In a call", &["speaker", "Speaker", "end", "End"])
    };
    let hints = HashMap::from([
        ("category", Value::from(category)),
        ("desktop-entry", Value::from("org.denialwm.Calls")),
        ("urgency", Value::from(2u8)),
        ("resident", Value::from(true)),
        ("suppress-sound", Value::from(true)),
        ("x-denial-clearable", Value::from(false)),
    ]);
    match notifications
        .notify("Calls", replaces, "call-start", &card.name(), body, actions, hints, 0)
        .await
    {
        Ok(id) => {
            eprintln!("calls-ringer: posted {category} as {id}");
            Some((id, card))
        }
        Err(error) => {
            eprintln!("calls-ringer: posting the call: {error}");
            None
        }
    }
}

/// A compositor that restarted needs a new connection.
fn set_screen(screen: &mut Option<Screen>, on: bool) {
    let set = match screen.as_mut().map(|screen| screen.set(on)) {
        Some(Ok(())) => Ok(()),
        _ => Screen::connect().and_then(|mut new| {
            new.set(on)?;
            *screen = Some(new);
            Ok(())
        }),
    };
    if let Err(error) = set {
        eprintln!("calls-ringer: screen: {error}");
    }
}

struct Outputs;

impl Dispatch<WlRegistry, GlobalListContents> for Outputs {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Wayland,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(Outputs: ignore WlOutput);
wayland_client::delegate_noop!(Outputs: ignore ZwlrOutputPowerManagerV1);
impl Dispatch<ZwlrOutputPowerV1, ()> for Outputs {
    fn event(
        _: &mut Self,
        _: &ZwlrOutputPowerV1,
        event: <ZwlrOutputPowerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Wayland,
        _: &QueueHandle<Self>,
    ) {
        eprintln!("calls-ringer: output power: {event:?}");
    }
}

/// Control of every output's power, kept for as long as this program runs: the
/// compositor forgets a request still pending when its client disconnects. The
/// power key still turns the screen on, and the compositor does not lock for
/// this.
struct Screen {
    queue: wayland_client::EventQueue<Outputs>,
    controls: Vec<ZwlrOutputPowerV1>,
}

impl Screen {
    fn connect() -> Result<Self, Box<dyn std::error::Error>> {
        let wayland = Wayland::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<Outputs>(&wayland)?;
        let handle = queue.handle();
        let manager: ZwlrOutputPowerManagerV1 = globals.bind(&handle, 1..=1, ())?;
        let registry = globals.registry();
        let controls = globals.contents().with_list(|list| {
            list.iter()
                .filter(|global| global.interface == "wl_output")
                .map(|global| {
                    let output = registry.bind::<WlOutput, _, _>(global.name, 1, &handle, ());
                    manager.get_output_power(&output, &handle, ())
                })
                .collect()
        });
        Ok(Self { queue, controls })
    }

    fn set(&mut self, on: bool) -> Result<(), Box<dyn std::error::Error>> {
        for control in &self.controls {
            control.set_mode(if on { Mode::On } else { Mode::Off });
        }
        self.queue.roundtrip(&mut Outputs)?;
        Ok(())
    }
}

/// Starts ringing when a call comes in, and stops when none is left ringing.
fn follow(calls: Vec<Call>, ringing: &mut Option<Arc<AtomicBool>>) {
    let rings = calls.iter().any(|call| call.3 == "incoming");
    match (rings, ringing.is_some()) {
        (true, false) => {
            open_calls();
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            std::thread::spawn(move || ring(&flag));
            let flag = stop.clone();
            std::thread::spawn(move || vibrate(&flag));
            *ringing = Some(stop);
        }
        (false, true) => {
            if let Some(stop) = ringing.take() {
                stop.store(true, Ordering::Relaxed);
            }
        }
        _ => {}
    }
}

/// Plays the ringtone over and over until told to stop, as the Ringtone role
/// so that it follows the ringtone volume rather than the media one.
fn ring(stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        let mut player = match Command::new("pw-play")
            .args(["--media-role=Ringtone", RINGTONE])
            .spawn()
        {
            Ok(player) => player,
            Err(error) => return eprintln!("calls-ringer: pw-play: {error}"),
        };
        loop {
            if stop.load(Ordering::Relaxed) {
                let _ = player.kill();
                let _ = player.wait();
                return;
            }
            match player.try_wait() {
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Ok(Some(status)) if status.success() => break,
                // Not a loop of failures, once a second at the least.
                _ => {
                    std::thread::sleep(Duration::from_secs(1));
                    break;
                }
            }
        }
    }
}

/// hapticd, whose patterns ring the phone's vibration motor.
#[proxy(
    interface = "org.hapticd.Haptics1",
    default_service = "org.hapticd",
    default_path = "/org/hapticd/Haptics"
)]
trait Haptics1 {
    /// Steps of duration in ms and intensity, 0 to 1; on the default device
    /// when `device` is empty. Returns the effect's id.
    fn play_pattern(&self, device: &str, steps: &[(u32, f64)]) -> zbus::Result<u64>;

    fn cancel(&self, effect: u64) -> zbus::Result<()>;
}

/// Vibrates in Android's default ring pattern, 1 s on and 1 s off, until told
/// to stop. hapticd plays at most 30 s at a time, so each request is half that
/// and the next follows as it runs out.
fn vibrate(stop: &AtomicBool) {
    const BEATS: u32 = 15;
    const ON: u32 = 1000;
    const OFF: u32 = 1000;
    let haptics = zbus::blocking::Connection::system()
        .and_then(|bus| Haptics1ProxyBlocking::new(&bus));
    let haptics = match haptics {
        Ok(haptics) => haptics,
        Err(error) => return eprintln!("calls-ringer: reaching hapticd: {error}"),
    };
    let steps: Vec<(u32, f64)> = (0..BEATS).flat_map(|_| [(ON, 1.0), (OFF, 0.0)]).collect();
    let length = Duration::from_millis(u64::from(BEATS * (ON + OFF)));
    while !stop.load(Ordering::Relaxed) {
        let effect = match haptics.play_pattern("", &steps) {
            Ok(effect) => effect,
            Err(error) => return eprintln!("calls-ringer: vibrating: {error}"),
        };
        let started = std::time::Instant::now();
        while started.elapsed() < length {
            if stop.load(Ordering::Relaxed) {
                if let Err(error) = haptics.cancel(effect) {
                    eprintln!("calls-ringer: stopping the vibration: {error}");
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Starts Calls beside this program; a running one takes it and raises itself.
fn open_calls() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => return eprintln!("calls-ringer: {error}"),
    };
    let app = ["denial-calls", "calls"]
        .iter()
        .map(|name| exe.with_file_name(name))
        .find(|app| app.exists());
    let Some(app) = app else {
        return eprintln!("calls-ringer: Calls is not installed");
    };
    match Command::new(&app).spawn() {
        // Reaped on its own thread so it never lingers as a zombie.
        Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
        Err(error) => eprintln!("calls-ringer: {}: {error}", app.display()),
    }
}
