//! The Slider page: what the game mode slider does, slid on and back. The
//! choice is kept in `~/.config/denial/slider.json` for nx809j-slider, which
//! acts on the slider: the flashlight, the camera, or an app picked from the
//! ones installed.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use denial_look::{Tween, decelerate, motion};
use denial_look_gpui::{dp, island, solid};
use gpui::{Context, Div, Transformation, div, prelude::*, px, radians};
use serde_json::{Value, json};

use super::parts::ROW_PAD;
use super::{Layer, Settings};

/// The program that acts on the slider; the page shows only with it.
const SERVICE: &str = "/usr/bin/nx809j-slider";

const ACTIONS: &[&str] = &["Flashlight", "Camera", "An app"];
const WORDS: [&str; 3] = ["flashlight", "camera", "app"];

pub(super) struct State {
    /// Into `ACTIONS`.
    action: usize,
    /// The picked app's desktop entry id.
    app: Option<String>,
    /// The app list shows under its name.
    open: bool,
    /// The list opening or closing.
    flip: Option<(bool, Tween)>,
}

fn file() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    config.join("denial").join("slider.json")
}

impl State {
    pub(super) fn load() -> State {
        crate::catalog::set_slider(std::path::Path::new(SERVICE).exists());
        let value: Value = fs::read_to_string(file())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
        State {
            action: WORDS
                .iter()
                .position(|word| value["action"].as_str() == Some(*word))
                .unwrap_or(0),
            app: value["app"].as_str().map(str::to_owned),
            open: false,
            flip: None,
        }
    }

    fn save(&self) -> io::Result<()> {
        let file = file();
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        let value = json!({ "action": WORDS[self.action], "app": self.app });
        let temporary = file.with_extension("json.tmp");
        fs::write(&temporary, value.to_string())?;
        fs::rename(temporary, file)
    }

    /// How open the list is, 0 to 1.
    fn opened(&self, now: Instant) -> f32 {
        let Some((to, tween)) = self.flip else {
            return f32::from(u8::from(self.open));
        };
        let millis = if to { motion::IN } else { motion::OUT };
        let t = decelerate(tween.segment(0, millis).progress(now));
        if to { t } else { 1.0 - t }
    }

    pub(super) fn animating(&self, now: Instant) -> bool {
        self.flip
            .is_some_and(|(_, tween)| tween.segment(0, motion::IN).running(now))
    }
}

impl Settings {
    pub(super) fn slider_page(&self, layer: Layer, cx: &mut Context<Self>) -> Vec<Div> {
        let state = &self.slider;
        let live = layer.live();
        let now = Instant::now();
        let mut parts = vec![island(&self.look).child(self.options_row(
            "slider.action",
            "When slid on",
            Some("Sliding it back undoes it".into()),
            ACTIONS,
            state.action,
            live,
            (true, true),
            cx,
            Rc::new(|this, option, _, cx| {
                this.slider.action = option.min(ACTIONS.len() - 1);
                this.keep_slider(cx);
            }),
        ))];
        if state.action != 2 {
            return parts;
        }

        let opened = state.opened(now);
        let picked = state
            .app
            .as_ref()
            .and_then(|id| self.installed.apps.iter().find(|app| &app.id == id));
        let radius = px(self.look.group_radius());
        let header = div()
            .id("slider.app")
            .flex()
            .items_center()
            .gap(dp(8.))
            .px(dp(ROW_PAD))
            .py(dp(12.))
            .rounded_t(radius)
            .when(opened <= 0.0, |row| row.rounded_b(radius))
            .child(
                self.chevron()
                    .with_transformation(Transformation::rotate(radians(
                        (90.0 * opened).to_radians(),
                    ))),
            )
            .child(div().flex_1().child(self.text_column("App", None)))
            .child(self.value(picked.map_or("None".to_owned(), |app| app.name.clone())));
        let mut list = island(&self.look).child(self.live(header, live, cx, |this, _, cx| {
            let open = !this.slider.open;
            this.slider.open = open;
            this.slider.flip = Some((
                open,
                this.look.tween(if open { motion::IN } else { motion::OUT }),
            ));
            cx.notify();
        }));
        if opened > 0.0 {
            let apps = &self.installed.apps;
            let count = apps.len();
            let rows = apps
                .iter()
                .enumerate()
                .fold(div().flex().flex_col(), |rows, (ix, app)| {
                    let chosen = state.app.as_deref() == Some(app.id.as_str());
                    let id = app.id.clone();
                    let row = self
                        .app_row(app, (false, ix + 1 == count))
                        .when(chosen, |row| {
                            row.child(
                                div()
                                    .flex_none()
                                    .size(dp(10.))
                                    .rounded_full()
                                    .bg(solid(self.look.accent)),
                            )
                        });
                    rows.child(self.separator(0.)).child(self.live(
                        row,
                        live && self.slider.open,
                        cx,
                        move |this, _, cx| {
                            this.slider.app = Some(id.clone());
                            this.slider.open = false;
                            this.slider.flip = Some((false, this.look.tween(motion::OUT)));
                            this.keep_slider(cx);
                        },
                    ))
                });
            list = list.child(
                div()
                    .relative()
                    .top(dp(-motion::NEW_MESSAGE_RISE * (1.0 - opened)))
                    .opacity(opened)
                    .child(rows),
            );
        }
        parts.push(self.chip_row("App"));
        parts.push(list);
        parts
    }

    fn keep_slider(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.slider.save() {
            self.note(format!("Could not keep the slider's action: {error}"), cx);
        }
        cx.notify();
    }
}
