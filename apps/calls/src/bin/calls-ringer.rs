//! Rings while a call comes in and opens Calls on it; dmail itself never
//! rings. Turns the screen off while a call on the earpiece is held to the ear.
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
use zbus::{Connection, proxy};

const RINGTONE: &str = "/usr/share/sounds/freedesktop/stereo/phone-incoming-call.oga";

#[proxy(
    interface = "org.dmail.Voice1",
    default_service = "org.dmail",
    default_path = "/org/dmail"
)]
trait Voice1 {
    #[zbus(property)]
    fn calls(&self) -> zbus::Result<Vec<(u8, String, String, String, i64)>>;
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
    Calls(Vec<(u8, String, String, String, i64)>),
    Near(bool),
    Speaker(bool),
    Closed,
}

async fn run() -> zbus::Result<()> {
    let system = Connection::system().await?;
    let voice = Voice1Proxy::new(&system).await?;
    let sensors = Sensors1Proxy::new(&system).await?;
    let audio = CallAudio1Proxy::new(&Connection::session().await?).await?;
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
                let talking = list
                    .iter()
                    .any(|call| matches!(call.3.as_str(), "dialing" | "alerting" | "active" | "held"));
                follow(list, &mut ringing);
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
            Event::Closed => return Ok(()),
        }
        let want_dark = in_call && near && !speaker;
        if want_dark != dark {
            dark = want_dark;
            eprintln!("calls-ringer: screen {}", if dark { "off" } else { "on" });
            // A compositor that restarted needs a new connection.
            let set = match screen.as_mut().map(|screen| screen.set(!dark)) {
                Some(Ok(())) => Ok(()),
                _ => Screen::connect().and_then(|mut new| {
                    new.set(!dark)?;
                    screen = Some(new);
                    Ok(())
                }),
            };
            if let Err(error) = set {
                eprintln!("calls-ringer: screen: {error}");
            }
        }
        event = future::or(
            async {
                match calls.next().await {
                    Some(change) => Event::Calls(change.get().await.unwrap_or_default()),
                    None => Event::Closed,
                }
            },
            future::or(
                async {
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
                },
            ),
        )
        .await;
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
fn follow(calls: Vec<(u8, String, String, String, i64)>, ringing: &mut Option<Arc<AtomicBool>>) {
    let rings = calls.iter().any(|call| call.3 == "incoming");
    match (rings, ringing.is_some()) {
        (true, false) => {
            open_calls();
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            std::thread::spawn(move || ring(&flag));
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

/// Plays the ringtone over and over until told to stop.
fn ring(stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        let mut player = match Command::new("pw-play").arg(RINGTONE).spawn() {
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
