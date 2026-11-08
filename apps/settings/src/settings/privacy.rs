//! The Privacy page: the camera, the microphone and location, each on or
//! off for every app, then for each app that has used it. The switches are
//! kept in `~/.config/denial/privacy.json`,
//!
//! ```json
//! {"camera": true, "microphone": true, "location": true,
//!  "apps": {"microphone": {"firefox": false}, "location": {"org.gnome.Maps": false}}}
//! ```
//!
//! which the phone's privacy service (nx809j-privacy) hands to PipeWire,
//! the camera service and geoclue, and Denial's location agent reads. The
//! apps come from the service's record of every app that used each,
//! `$XDG_STATE_HOME/denial/privacy-seen.json`, and what uses each now from
//! `$XDG_RUNTIME_DIR/denial/privacy-usage.json`. Apps are known as the
//! service knows them: the microphone's and the camera's by the program
//! they run, location's by their desktop id. Without the service the page
//! shows the switches faint.

use std::fs;
use std::io;
use std::path::PathBuf;

use denial_look::words::fill;
use denial_look_gpui::Icon;
use gpui::{Context, Div, SharedString, prelude::*};
use serde_json::{Map, Value};

use super::page::FAINT;
use super::parts::{ROW_GAP, ROW_PAD};
use super::system::APP_ICON;
use super::{Layer, Settings};
use crate::apps::App;
use crate::catalog::Section;

/// The service that acts on the switches; without it they reach nothing.
const SERVICE: &str = "/usr/bin/nx809j-privacy";

/// Each kind: its key in the files, its title, its glyph and the chip over
/// its apps.
const KINDS: [(&str, &str, Option<Icon>, &str); 3] = [
    ("camera", "Camera", None, "Apps that used the camera"),
    (
        "microphone",
        "Microphone",
        None,
        "Apps that used the microphone",
    ),
    (
        "location",
        "Location",
        Some(Icon::Location),
        "Apps that asked for location",
    ),
];

#[derive(Default)]
pub(super) struct State {
    present: bool,
    /// `privacy.json` as read, kept whole so keys the page does not know
    /// stay.
    switches: Map<String, Value>,
    /// By kind: every app that used it, oldest first.
    seen: [Vec<String>; 3],
    /// By kind: the apps using it now.
    using: [Vec<String>; 3],
}

fn config_dir(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"))
        .join("denial")
}

fn switches_file() -> PathBuf {
    config_dir("XDG_CONFIG_HOME", ".config").join("privacy.json")
}

fn seen_file() -> PathBuf {
    config_dir("XDG_STATE_HOME", ".local/state").join("privacy-seen.json")
}

fn usage_file() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(|| PathBuf::from("/nonexistent"), PathBuf::from)
        .join("denial/privacy-usage.json")
}

fn read_json(path: PathBuf) -> Map<String, Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// The app names under `kind` in a file of lists.
fn names(map: &Map<String, Value>, kind: &str) -> Vec<String> {
    map.get(kind)
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

impl State {
    pub(super) fn load() -> State {
        let seen = read_json(seen_file());
        let usage = read_json(usage_file());
        State {
            present: std::path::Path::new(SERVICE).exists(),
            switches: read_json(switches_file()),
            seen: KINDS.map(|(kind, ..)| names(&seen, kind)),
            using: KINDS.map(|(kind, ..)| names(&usage, kind)),
        }
    }

    /// Whether `kind` is on for every app. Anything but an explicit `false`
    /// is on.
    fn on(&self, kind: &str) -> bool {
        self.switches.get(kind).and_then(Value::as_bool) != Some(false)
    }

    /// Whether `kind` is on for `app`.
    fn app_on(&self, kind: &str, app: &str) -> bool {
        self.switches
            .get("apps")
            .and_then(|apps| apps.get(kind))
            .and_then(|apps| apps.get(app))
            .and_then(Value::as_bool)
            != Some(false)
    }

    fn set(&mut self, kind: &str, on: bool) {
        self.switches.insert(kind.to_owned(), Value::Bool(on));
    }

    /// Turns `kind` on or off for `app`. On is the default, so it is kept
    /// only as off.
    fn set_app(&mut self, kind: &str, app: &str, on: bool) {
        let apps = self
            .switches
            .entry("apps")
            .or_insert_with(|| Value::Object(Map::new()));
        if !apps.is_object() {
            *apps = Value::Object(Map::new());
        }
        let Some(apps) = apps.as_object_mut() else {
            return;
        };
        let these = apps
            .entry(kind)
            .or_insert_with(|| Value::Object(Map::new()));
        if !these.is_object() {
            *these = Value::Object(Map::new());
        }
        let Some(these) = these.as_object_mut() else {
            return;
        };
        if on {
            these.remove(app);
        } else {
            these.insert(app.to_owned(), Value::Bool(false));
        }
    }

    fn save(&self) -> io::Result<()> {
        let file = switches_file();
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let temporary = file.with_extension("json.tmp");
        fs::write(&temporary, Value::Object(self.switches.clone()).to_string())?;
        fs::rename(temporary, file)
    }
}

impl Settings {
    pub(super) fn privacy_page(
        &self,
        sections: &'static [Section],
        layer: Layer,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let state = &self.privacy;
        let live = layer.live() && state.present;
        let mut parts = vec![self.chip_row("Access")];
        parts.push(self.rows_island(
            KINDS.iter().enumerate(),
            ROW_PAD,
            |(ix, &(kind, title, glyph, _)), ends| {
                let on = state.on(kind);
                let detail: SharedString = if !state.present {
                    "Not on this phone yet".into()
                } else if !on {
                    "Off for every app".into()
                } else if !state.using[ix].is_empty() {
                    let using: Vec<String> = state.using[ix]
                        .iter()
                        .map(|app| self.app_name(kind, app))
                        .collect();
                    fill("In use by {apps}", &[("apps", &using.join(", "))]).into()
                } else {
                    "For all apps".into()
                };
                let row = self
                    .row(kind, glyph, title, Some(detail), ends)
                    .child(self.toggle(on))
                    .when(!state.present, |row| row.opacity(FAINT));
                self.live(row, live, cx, move |this, _, cx| {
                    this.privacy.set(kind, !on);
                    this.keep_privacy(cx);
                })
            },
        ));

        for (ix, &(kind, _, _, chip)) in KINDS.iter().enumerate() {
            let apps = &state.seen[ix];
            if !state.present || !state.on(kind) || apps.is_empty() {
                continue;
            }
            parts.push(self.chip_row(chip));
            parts.push(
                self.rows_island(apps.iter(), ROW_PAD + APP_ICON + ROW_GAP, |app, ends| {
                    let on = state.app_on(kind, app);
                    let row = match self.installed_app(kind, app) {
                        Some(installed) => self.app_row(installed, ends),
                        None => self.row(
                            SharedString::from(format!("{kind}.{app}")),
                            None,
                            app.clone(),
                            None,
                            ends,
                        ),
                    }
                    .child(self.toggle(on));
                    let app = app.clone();
                    self.live(row, live, cx, move |this, _, cx| {
                        this.privacy.set_app(kind, &app, !on);
                        this.keep_privacy(cx);
                    })
                }),
            );
        }

        parts.extend(self.page(sections, layer, cx));
        parts
    }

    /// The installed app the service names `app` for `kind`: by desktop id
    /// for location, by program for the rest.
    fn installed_app(&self, kind: &str, app: &str) -> Option<&App> {
        let apps = &self.installed.apps;
        if kind == "location" {
            let id = format!("{app}.desktop");
            apps.iter().find(|installed| installed.id == id)
        } else {
            apps.iter()
                .find(|installed| installed.binary.as_deref() == Some(app))
        }
    }

    fn app_name(&self, kind: &str, app: &str) -> String {
        self.installed_app(kind, app)
            .map_or_else(|| app.to_owned(), |installed| installed.name.clone())
    }

    /// Reads the switches and the apps again, for the page just shown.
    pub(super) fn read_privacy(&mut self, cx: &mut Context<Self>) {
        self.privacy = State::load();
        self.read_apps(cx);
    }

    fn keep_privacy(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.privacy.save() {
            self.note(format!("Could not keep the privacy switches: {error}"), cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::State;
    use serde_json::json;

    fn state(switches: serde_json::Value) -> State {
        State {
            switches: switches.as_object().cloned().unwrap_or_default(),
            ..State::default()
        }
    }

    #[test]
    fn only_an_explicit_false_is_off() {
        let state = state(json!({"camera": false, "apps": {"microphone": {"firefox": false}}}));
        assert!(!state.on("camera"));
        assert!(state.on("microphone"));
        assert!(!state.app_on("microphone", "firefox"));
        assert!(state.app_on("microphone", "other"));
        assert!(state.app_on("camera", "firefox"));
    }

    #[test]
    fn an_app_turned_back_on_leaves_the_file() {
        let mut state = state(json!({"keep": 1}));
        state.set_app("location", "org.gnome.Maps", false);
        assert_eq!(
            state.switches["apps"]["location"]["org.gnome.Maps"],
            json!(false)
        );
        state.set_app("location", "org.gnome.Maps", true);
        assert_eq!(state.switches["apps"]["location"], json!({}));
        assert_eq!(state.switches["keep"], json!(1));
    }
}
