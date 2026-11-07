//! The phone's RGB lights: multicolor LEDs named `rgb:<name>` (on the
//! RedMagic 11 Pro the logo, the "game mode" light, the shoulder trigger
//! lights and four fan zones). Settings writes them as they change and keeps
//! the choice in `~/.config/denial/rgb-lights.json`, which Denial's shell
//! puts back when it starts.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value, json};

const LEDS: &str = "/sys/class/leds";
const PREFIX: &str = "rgb:";

pub const FAN: &str = "fan";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    Solid,
    Breathe,
    Blink,
}

impl Effect {
    pub const ALL: [Effect; 3] = [Effect::Solid, Effect::Breathe, Effect::Blink];

    fn word(self) -> &'static str {
        match self {
            Effect::Solid => "solid",
            Effect::Breathe => "breathe",
            Effect::Blink => "blink",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Light {
    pub on: bool,
    /// 0xRRGGBB.
    pub color: u32,
    /// 0 to 1.
    pub brightness: f32,
    pub effect: Effect,
    /// How fast breathing and blinking go: 0 slowest to 1 fastest.
    pub speed: f32,
}

impl Default for Light {
    fn default() -> Self {
        Light {
            on: false,
            color: 0xff0000,
            brightness: 1.0,
            effect: Effect::Solid,
            speed: 0.5,
        }
    }
}

impl Light {
    fn to_json(self) -> Value {
        json!({
            "on": self.on,
            "color": self.color & 0xffffff,
            "brightness": self.brightness,
            "effect": self.effect.word(),
            "speed": self.speed,
        })
    }

    fn from_json(value: &Value) -> Light {
        let mut light = Light::default();
        light.on = value["on"].as_bool().unwrap_or(false);
        if let Some(color) = value["color"].as_u64() {
            light.color = color as u32 & 0xffffff;
        }
        if let Some(brightness) = value["brightness"].as_f64() {
            light.brightness = (brightness as f32).clamp(0.0, 1.0);
        }
        if let Some(speed) = value["speed"].as_f64() {
            light.speed = (speed as f32).clamp(0.0, 1.0);
        }
        light.effect = Effect::ALL
            .into_iter()
            .find(|effect| value["effect"].as_str() == Some(effect.word()))
            .unwrap_or(Effect::Solid);
        light
    }
}

#[derive(Clone, Default, Debug)]
pub struct Lights {
    /// By light: logo, gamemode, trigger-left, trigger-right, fan (all its
    /// zones together) and fan-1 to fan-4 (zones on their own).
    pub lights: BTreeMap<String, Light>,
    pub fan_zones_separate: bool,
}

impl Lights {
    pub fn get(&self, light: &str) -> Light {
        self.lights.get(light).copied().unwrap_or_default()
    }

    /// What `led` shows: a fan zone takes the fan's setting unless the zones
    /// are set on their own.
    pub fn for_led(&self, led: &str) -> Light {
        if is_fan_zone(led) && !self.fan_zones_separate {
            self.get(FAN)
        } else {
            self.get(led)
        }
    }

    pub fn load() -> Lights {
        let Ok(text) = fs::read_to_string(file()) else {
            return Lights::default();
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return Lights::default();
        };
        Lights {
            fan_zones_separate: value["fanZonesSeparate"].as_bool().unwrap_or(false),
            lights: value["lights"]
                .as_object()
                .map(|lights| {
                    lights
                        .iter()
                        .map(|(name, light)| (name.clone(), Light::from_json(light)))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let lights: Map<String, Value> = self
            .lights
            .iter()
            .map(|(name, light)| (name.clone(), light.to_json()))
            .collect();
        let value = json!({ "fanZonesSeparate": self.fan_zones_separate, "lights": lights });
        let file = file();
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let temporary = file.with_extension("json.tmp");
        fs::write(&temporary, value.to_string())?;
        fs::rename(temporary, file)
    }
}

pub fn is_fan_zone(led: &str) -> bool {
    led.strip_prefix(FAN)
        .is_some_and(|rest| rest.starts_with('-'))
}

fn file() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    config.join("denial").join("rgb-lights.json")
}

/// The LEDs present, without the `rgb:` prefix, sorted.
pub fn leds() -> Vec<String> {
    let mut leds: Vec<String> = fs::read_dir(LEDS)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix(PREFIX))
                .map(str::to_owned)
        })
        .collect();
    leds.sort();
    leds
}

/// Shows `light` on `led`. The colour goes in at full intensity and the
/// brightness in the LED's brightness, so the chip keeps the hue when dim.
/// Breathing and blinking run on the chip as a hardware pattern. The LED
/// leaves a pattern only when it must, so a change doesn't blank it.
pub fn apply(led: &str, light: Light) -> io::Result<()> {
    let dir = PathBuf::from(LEDS).join(format!("{PREFIX}{led}"));
    let write = |attribute: &str, value: &str| fs::write(dir.join(attribute), value);
    let patterned = fs::read_to_string(dir.join("trigger"))
        .is_ok_and(|triggers| triggers.contains("[pattern]"));
    let level = ((light.brightness * 255.0).round() as u32).clamp(1, 255);
    if !light.on {
        if patterned {
            write("trigger", "none")?;
        }
        return write("brightness", "0");
    }
    let (r, g, b) = (
        light.color >> 16 & 0xff,
        light.color >> 8 & 0xff,
        light.color & 0xff,
    );
    match light.effect {
        Effect::Solid => {
            if patterned {
                write("trigger", "none")?;
            }
            write("multi_intensity", &format!("{r} {g} {b}"))?;
            write("brightness", &level.to_string())
        }
        Effect::Breathe => {
            let ms = half_period(light.speed, 3000.0, 300.0);
            pattern(&dir, (r, g, b), &format!("0 {ms} {level} {ms}"))
        }
        Effect::Blink => {
            let ms = half_period(light.speed, 1200.0, 100.0);
            pattern(
                &dir,
                (r, g, b),
                &format!("{level} {ms} {level} 1 0 {ms} 0 1"),
            )
        }
    }
}

/// Half an effect's cycle in ms at `speed`, from `slowest` to `fastest`.
pub fn half_period(speed: f32, slowest: f32, fastest: f32) -> u32 {
    (slowest + (fastest - slowest) * speed.clamp(0.0, 1.0)).round() as u32
}

fn pattern(dir: &std::path::Path, (r, g, b): (u32, u32, u32), steps: &str) -> io::Result<()> {
    fs::write(dir.join("multi_intensity"), format!("{r} {g} {b}"))?;
    fs::write(dir.join("trigger"), "pattern")?;
    // hw_pattern comes with the trigger; udev opens it to us just after
    let mut tries = 0;
    loop {
        match fs::write(dir.join("hw_pattern"), steps) {
            Ok(()) => return Ok(()),
            Err(error) if tries >= 20 => return Err(error),
            Err(_) => {
                tries += 1;
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Writes the LEDs on a thread of its own: a write that takes long never
/// holds the page up, and a setting replaced while another is written is
/// dropped, so a finger dragging a colour sends only the newest.
pub struct Writer {
    queue: Arc<(Mutex<BTreeMap<String, Light>>, Condvar)>,
    failed: Arc<Mutex<Option<String>>>,
}

impl Writer {
    pub fn new() -> Writer {
        let queue: Arc<(Mutex<BTreeMap<String, Light>>, Condvar)> = Arc::default();
        let failed: Arc<Mutex<Option<String>>> = Arc::default();
        let (thread_queue, thread_failed) = (queue.clone(), failed.clone());
        let spawned = thread::Builder::new().name("lights".into()).spawn(move || {
            let (pending, ready) = &*thread_queue;
            loop {
                let batch = {
                    let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
                    while pending.is_empty() {
                        pending = ready.wait(pending).unwrap_or_else(|e| e.into_inner());
                    }
                    std::mem::take(&mut *pending)
                };
                for (led, light) in batch {
                    if let Err(error) = apply(&led, light) {
                        *thread_failed.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(format!("Could not set the light: {error}"));
                    }
                }
            }
        });
        if let Err(error) = spawned {
            log::error!("lights: no writer thread: {error}");
        }
        Writer { queue, failed }
    }

    /// Show `light` on `led` soon.
    pub fn write(&self, led: &str, light: Light) {
        let (pending, ready) = &*self.queue;
        pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(led.to_owned(), light);
        ready.notify_one();
    }

    /// The last write that failed since asked, if one did.
    pub fn failure(&self) -> Option<String> {
        self.failed.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}
