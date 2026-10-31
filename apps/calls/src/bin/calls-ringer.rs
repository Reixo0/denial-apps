//! Rings while a call comes in and opens Calls on it; dmail itself never
//! rings. Sleeps on D-Bus, needs no window, and is started at login by
//! calls-ringer.service. callaudio leaves the sound card on its normal profile
//! while a call rings here, so the ringtone plays.

use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_lite::{StreamExt, future};
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

fn main() -> ExitCode {
    match future::block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("calls-ringer: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> zbus::Result<()> {
    let voice = Voice1Proxy::new(&Connection::system().await?).await?;
    let mut changes = voice.receive_calls_changed().await;
    let mut ringing: Option<Arc<AtomicBool>> = None;
    follow(voice.calls().await.unwrap_or_default(), &mut ringing);
    while let Some(change) = changes.next().await {
        follow(change.get().await.unwrap_or_default(), &mut ringing);
    }
    Ok(())
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
