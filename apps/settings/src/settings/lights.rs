//! The Lights page: each RGB light on its own, with its colour, brightness
//! and effect. The fan's zones go together unless set separately. A colour
//! is picked on a square (saturation across, value down) over a hue bar, or
//! from a few swatches.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use denial_look::mobile::TOUCH;
use denial_look::{Tween, decelerate, lerp, lerp_rgb, motion, stagger};
use denial_look_gpui::{control_fill, dp, island, solid};
use gpui::{
    Context, DispatchPhase, Div, ElementId, HitboxBehavior, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Stateful, TouchDragEvent, TouchPhase, Transformation,
    canvas, div, linear_color_stop, linear_gradient, prelude::*, px, radians, relative,
};

use super::page::FAINT;
use super::parts::ROW_PAD;
use super::{Layer, Settings};
use crate::lights::{self, Effect, FAN, Light, Lights};

/// Colours that look right on the LEDs: pure, with no white in them.
const SWATCHES: [u32; 9] = [
    0xff0000, 0xff3000, 0xffa000, 0x00ff00, 0x00ffff, 0x0000ff, 0x8000ff, 0xff0060, 0xffffff,
];

const EFFECTS: &[&str] = &["Solid", "Breathing", "Blinking"];

/// The colour square's height and the knobs on it and on the hue bar, in dp.
const SQUARE: f32 = 170.0;
const KNOB: f32 = 24.0;
const BAR: f32 = 12.0;

/// While a finger drags a colour or a level, the light follows at most this
/// often: each change is a command over I2C.
const LIVE_EVERY: Duration = Duration::from_millis(50);

/// What a drag on the square or the hue bar does with where the finger is,
/// 0 to 1 across and down, and whether it has let go.
type OnDrag = Rc<dyn Fn(&mut Settings, f32, f32, bool, &mut Context<Settings>)>;

/// One light's rows, by their keys.
struct Keys {
    light: &'static str,
    title: &'static str,
    header: &'static str,
    on: &'static str,
    swatches: &'static str,
    square: &'static str,
    hue: &'static str,
    brightness: &'static str,
    effect: &'static str,
    speed: &'static str,
}

macro_rules! keys {
    ($light:literal, $title:literal) => {
        Keys {
            light: $light,
            title: $title,
            header: concat!("lights.", $light, ".header"),
            on: concat!("lights.", $light, ".on"),
            swatches: concat!("lights.", $light, ".swatches"),
            square: concat!("lights.", $light, ".square"),
            hue: concat!("lights.", $light, ".hue"),
            brightness: concat!("lights.", $light, ".brightness"),
            effect: concat!("lights.", $light, ".effect"),
            speed: concat!("lights.", $light, ".speed"),
        }
    };
}

/// The lights the page names, in its order. The fan's zones come after the
/// fan, shown while they are set separately.
const LIGHTS: [Keys; 9] = [
    keys!("logo", "RedMagic logo"),
    keys!("gamemode", "Game mode light"),
    keys!("trigger-left", "Left trigger"),
    keys!("trigger-right", "Right trigger"),
    keys!("fan", "Fan"),
    keys!("fan-1", "Fan zone 1"),
    keys!("fan-2", "Fan zone 2"),
    keys!("fan-3", "Fan zone 3"),
    keys!("fan-4", "Fan zone 4"),
];

const ZONES_SEPARATE: &str = "lights.fan.separate";

/// The lights as the page last set them, and the LEDs on the phone.
pub(super) struct State {
    leds: Vec<String>,
    lights: Lights,
    /// Each light's colour as hue, saturation and value while picked here:
    /// the hue stays put where the colour is grey or black.
    picked: HashMap<&'static str, [f32; 3]>,
    /// The lights whose settings show under their name.
    open: HashSet<&'static str>,
    /// When a drag last reached the light.
    shown_at: Option<Instant>,
    writer: lights::Writer,
    /// Changes still animating: a light opening or closing, turning on or
    /// off, and the fan's zones coming or going.
    flips: HashMap<(Flip, &'static str), (bool, Tween)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Flip {
    Open,
    On,
    Zones,
}

impl State {
    pub(super) fn load() -> State {
        let leds = lights::leds();
        crate::catalog::set_lights(!leds.is_empty());
        State {
            leds,
            lights: Lights::load(),
            picked: HashMap::new(),
            open: HashSet::new(),
            shown_at: None,
            writer: lights::Writer::new(),
            flips: HashMap::new(),
        }
    }

    fn has(&self, light: &str) -> bool {
        if light == FAN {
            self.leds.iter().any(|led| lights::is_fan_zone(led))
        } else {
            self.leds.iter().any(|led| led == light)
        }
    }

    /// The LEDs `light`'s setting drives.
    fn leds_of(&self, light: &str) -> Vec<String> {
        self.leds
            .iter()
            .filter(|led| {
                if light == FAN {
                    lights::is_fan_zone(led)
                } else {
                    *led == light
                }
            })
            .cloned()
            .collect()
    }

    /// How far `light` has come to its `flip` side, 0 to 1, on the
    /// decelerate curve, `delay` ms into the change; `now_on` when it is not
    /// changing.
    fn level(
        &self,
        flip: Flip,
        light: &'static str,
        now_on: bool,
        delay: u64,
        now: Instant,
    ) -> f32 {
        let Some((to, tween)) = self.flips.get(&(flip, light)) else {
            return f32::from(u8::from(now_on));
        };
        let millis = if *to { motion::IN } else { motion::OUT };
        let t = decelerate(tween.segment(delay, millis).progress(now));
        if *to { t } else { 1.0 - t }
    }

    pub(super) fn animating(&self, now: Instant) -> bool {
        self.flips.values().any(|(_, tween)| {
            tween
                .segment(stagger(0, motion::IN_STEP, 4), motion::IN)
                .running(now)
        })
    }

    fn hsv(&self, light: &'static str) -> [f32; 3] {
        self.picked
            .get(light)
            .copied()
            .unwrap_or_else(|| to_hsv(self.lights.get(light).color))
    }
}

/// Hue, saturation and value, each 0 to 1, as 0xRRGGBB.
fn to_rgb([h, s, v]: [f32; 3]) -> u32 {
    let h = (h.clamp(0.0, 1.0) * 6.0) % 6.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let byte = |value: f32| ((value + m).clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(r) << 16 | byte(g) << 8 | byte(b)
}

/// 0xRRGGBB as hue, saturation and value, each 0 to 1 (hue 0 for greys).
fn to_hsv(color: u32) -> [f32; 3] {
    let (r, g, b) = (
        (color >> 16 & 0xff) as f32 / 255.0,
        (color >> 8 & 0xff) as f32 / 255.0,
        (color & 0xff) as f32 / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    [h, s, max]
}

fn clear(rgb: u32) -> Hsla {
    let mut color: Hsla = solid(rgb);
    color.a = 0.0;
    color
}

impl Settings {
    pub(super) fn lights_page(&self, layer: Layer, cx: &mut Context<Self>) -> Vec<Div> {
        let state = &self.lights;
        let live = layer.live();
        let now = Instant::now();
        let mut parts = Vec::new();
        for keys in LIGHTS
            .iter()
            .filter(|keys| !lights::is_fan_zone(keys.light))
        {
            if !state.has(keys.light) {
                continue;
            }
            if keys.light != FAN {
                parts.push(self.light_island(keys, live, now, cx));
                continue;
            }
            let separate = state.lights.fan_zones_separate;
            parts.push(self.chip_row("Fan"));
            let row = self
                .row(
                    ZONES_SEPARATE,
                    None,
                    "Set zones separately",
                    Some("Each of the fan's zones its own colour and effect".into()),
                    (true, true),
                )
                .child(self.switch(state.level(Flip::Zones, FAN, separate, 0, now)));
            parts.push(
                island(&self.look).child(self.live(row, live, cx, move |this, _, cx| {
                    this.set_fan_zones_separate(!separate, cx)
                })),
            );
            parts.push(self.light_island(keys, live, now, cx));
            // The zones rise in one after another, and fade out together
            for (ix, zone) in LIGHTS
                .iter()
                .filter(|keys| lights::is_fan_zone(keys.light) && state.has(keys.light))
                .enumerate()
            {
                let delay = if separate {
                    stagger(0, motion::IN_STEP, ix)
                } else {
                    0
                };
                let shown = state.level(Flip::Zones, FAN, separate, delay, now);
                if shown <= 0.0 {
                    continue;
                }
                parts.push(
                    div()
                        .relative()
                        .top(dp(motion::NEW_MESSAGE_RISE * (1.0 - shown)))
                        .opacity(shown)
                        .child(self.light_island(zone, live && separate, now, cx)),
                );
            }
        }
        parts
    }

    /// One light's island: its name and switch, then while open its colour,
    /// brightness and effect, faint and still while the light is off.
    /// Tapping the name opens or closes it.
    fn light_island(
        &self,
        keys: &'static Keys,
        live: bool,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> Div {
        let state = &self.lights;
        let light = state.lights.get(keys.light);
        let on = light.on;
        let open = state.open.contains(keys.light);
        let opened = state.level(Flip::Open, keys.light, open, 0, now);
        let lit = state.level(Flip::On, keys.light, on, 0, now);
        let title = if keys.light == FAN {
            "Fan lights"
        } else {
            keys.title
        };
        let radius = px(self.look.group_radius());
        let name = div()
            .id(keys.header)
            .flex_1()
            .flex()
            .items_center()
            .gap(dp(8.))
            .pl(dp(ROW_PAD))
            .py(dp(12.))
            .rounded_tl(radius)
            .when(opened <= 0.0, |name| name.rounded_bl(radius))
            .child(
                self.chevron()
                    .with_transformation(Transformation::rotate(radians(
                        (90.0 * opened).to_radians(),
                    ))),
            )
            .child(self.text_column(title, None));
        let switch = div()
            .id(keys.on)
            .flex_none()
            .px(dp(ROW_PAD))
            .py(dp(12.))
            .rounded_tr(radius)
            .when(opened <= 0.0, |switch| switch.rounded_br(radius))
            .child(self.switch(lit));
        let header = div()
            .flex()
            .items_center()
            .child(self.live(name, live, cx, move |this, _, cx| {
                let open = !this.lights.open.remove(keys.light);
                if open {
                    this.lights.open.insert(keys.light);
                }
                this.animate_light(Flip::Open, keys.light, open);
                cx.notify();
            }))
            .child(self.live(switch, live, cx, move |this, _, cx| {
                this.animate_light(Flip::On, keys.light, !on);
                this.change_light(keys.light, |light| light.on = !on, cx)
            }));
        let island = island(&self.look).child(header);
        // Kept while it closes, fading
        if opened <= 0.0 {
            return island;
        }
        // The controls answer only while the light is on
        let live = live && on && open;
        let mut block = div()
            .flex()
            .flex_col()
            .child(self.separator(0.))
            .child(self.swatches(keys, light, live, cx))
            .child(self.picker(keys, live, cx))
            .child(self.separator(0.))
            .child(self.level_row(
                keys.brightness,
                None,
                "Brightness",
                None,
                light.brightness,
                live,
                (false, false),
                cx,
                Rc::new(move |this, level, done, cx| {
                    let level = level.clamp(0.05, 1.0);
                    this.slide_light(keys.light, |light| light.brightness = level, done, cx)
                }),
            ))
            .child(self.separator(0.))
            .child(
                self.options_row(
                    keys.effect,
                    "Effect",
                    None,
                    EFFECTS,
                    Effect::ALL
                        .iter()
                        .position(|effect| *effect == light.effect)
                        .unwrap_or(0),
                    live,
                    (false, light.effect == Effect::Solid),
                    cx,
                    Rc::new(move |this, option, _, cx| {
                        let effect = Effect::ALL[option.min(Effect::ALL.len() - 1)];
                        this.change_light(keys.light, |light| light.effect = effect, cx)
                    }),
                ),
            );
        if light.effect != Effect::Solid {
            let half = match light.effect {
                Effect::Breathe => lights::half_period(light.speed, 3000.0, 300.0),
                _ => lights::half_period(light.speed, 1200.0, 100.0),
            };
            block = block.child(self.separator(0.)).child(self.slider_row(
                keys.speed,
                None,
                "Speed",
                None,
                format!("{:.1} s", 2.0 * half as f32 / 1000.0).into(),
                light.speed,
                live,
                (false, true),
                cx,
                Rc::new(move |this, speed, done, cx| {
                    let speed = speed.clamp(0.0, 1.0);
                    this.slide_light(keys.light, |light| light.speed = speed, done, cx)
                }),
            ));
        }
        island.child(
            div()
                .relative()
                .top(dp(-motion::NEW_MESSAGE_RISE * (1.0 - opened)))
                .opacity(opened * lerp(FAINT, 1.0, lit))
                .child(block),
        )
    }

    /// Start `light`'s `flip` toward `to`.
    fn animate_light(&mut self, flip: Flip, light: &'static str, to: bool) {
        let millis = if to { motion::IN } else { motion::OUT };
        let tween = self.look.tween(millis);
        self.lights.flips.insert((flip, light), (to, tween));
    }

    /// An on or off switch `level` of the way on: the knob slides and the
    /// track takes the accent.
    fn switch(&self, level: f32) -> Div {
        let knob = lerp_rgb(self.look.palette.secondary, self.look.on_accent, level);
        div()
            .flex_none()
            .relative()
            .w(dp(45.))
            .h(dp(26.))
            .rounded_full()
            .bg(control_fill(&self.look))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .bg(solid(self.look.accent))
                    .opacity(level),
            )
            .child(
                div()
                    .absolute()
                    .top(dp(3.))
                    .left(dp(lerp(3.0, 22.0, level)))
                    .size(dp(20.))
                    .rounded_full()
                    .bg(solid(knob)),
            )
    }

    /// The colour swatches, the light's colour ringed.
    fn swatches(
        &self,
        keys: &'static Keys,
        light: Light,
        live: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ring = self.look.palette.foreground;
        let swatches = SWATCHES.iter().enumerate().map(|(ix, &color)| {
            let picked = color == light.color;
            let swatch = div()
                .id(ElementId::NamedInteger(keys.swatches.into(), ix as u64))
                .size(dp(36.))
                .rounded_full()
                .bg(solid(color))
                .border(dp(if picked { 3. } else { 1. }))
                .border_color(solid(if picked {
                    ring
                } else {
                    self.look.palette.secondary
                }));
            self.live(swatch, live, cx, move |this, _, cx| {
                this.lights.picked.remove(keys.light);
                this.change_light(keys.light, |light| light.color = color, cx)
            })
        });
        div()
            .id(keys.swatches)
            .px(dp(ROW_PAD))
            .py(dp(12.))
            .flex()
            .flex_wrap()
            .gap(dp(10.))
            .children(swatches)
    }

    /// The colour square, saturation across and value down at the light's
    /// hue, and the hue bar under it.
    fn picker(&self, keys: &'static Keys, live: bool, cx: &mut Context<Self>) -> Div {
        let [h, s, v] = self.lights.hsv(keys.light);
        let pure = to_rgb([h, 1.0, 1.0]);
        let radius = px(self.look.group_radius() / 2.0);
        let mut square = div()
            .relative()
            .h(dp(SQUARE))
            .rounded(radius)
            .bg(solid(pure))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(radius)
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(solid(0xffffff), 0.),
                        linear_color_stop(clear(0xffffff), 1.),
                    )),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(radius)
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(clear(0x000000), 0.),
                        linear_color_stop(solid(0x000000), 1.),
                    )),
            )
            .child(
                div()
                    .absolute()
                    .left(relative(s))
                    .top(relative(1.0 - v))
                    .ml(-dp(KNOB / 2.))
                    .mt(-dp(KNOB / 2.))
                    .size(dp(KNOB))
                    .rounded_full()
                    .border(dp(3.))
                    .border_color(solid(0xffffff))
                    .bg(solid(to_rgb([h, s, v]))),
            );
        if live {
            square = square.child(self.color_drag(
                keys.square,
                0.0,
                cx,
                Rc::new(move |this, x, y, done, cx| {
                    let [h, _, _] = this.lights.hsv(keys.light);
                    this.pick_color(keys.light, [h, x, 1.0 - y], done, cx)
                }),
            ));
        }

        // Six two-stop gradients make the rainbow
        let stops = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0].map(|n| to_rgb([n / 6.0, 1.0, 1.0]));
        let rainbow = div()
            .absolute()
            .left(dp(KNOB / 2.))
            .right(dp(KNOB / 2.))
            .top(dp((TOUCH - BAR) / 2.))
            .h(dp(BAR))
            .flex()
            .rounded_full()
            .children(stops.windows(2).enumerate().map(|(ix, pair)| {
                div()
                    .flex_1()
                    .h_full()
                    .when(ix == 0, |part| part.rounded_l_full())
                    .when(ix == 5, |part| part.rounded_r_full())
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(solid(pair[0]), 0.),
                        linear_color_stop(solid(pair[1]), 1.),
                    ))
            }));
        let knob = div()
            .absolute()
            .left(dp(KNOB / 2.))
            .right(dp(KNOB / 2.))
            .top_0()
            .bottom_0()
            .child(
                div()
                    .absolute()
                    .left(relative(h))
                    .ml(-dp(KNOB / 2.))
                    .top(dp((TOUCH - KNOB) / 2.))
                    .size(dp(KNOB))
                    .rounded_full()
                    .border(dp(3.))
                    .border_color(solid(0xffffff))
                    .bg(solid(pure)),
            );
        let mut bar = div().relative().h(dp(TOUCH)).child(rainbow).child(knob);
        if live {
            bar = bar.child(self.color_drag(
                keys.hue,
                KNOB / 2.,
                cx,
                Rc::new(move |this, x, _, done, cx| {
                    let [_, s, v] = this.lights.hsv(keys.light);
                    this.pick_color(keys.light, [x, s, v], done, cx)
                }),
            ));
        }

        div()
            .px(dp(ROW_PAD))
            .pb(dp(8.))
            .flex()
            .flex_col()
            .gap(dp(8.))
            .child(square)
            .child(bar)
    }

    /// A layer over its parent that follows a finger or the mouse, `pad` dp
    /// in from the sides, and calls `drag` with where it is.
    fn color_drag(
        &self,
        key: &'static str,
        pad: f32,
        cx: &mut Context<Self>,
        drag: OnDrag,
    ) -> impl IntoElement {
        let view = cx.weak_entity();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                let pad = dp(pad);
                let at = move |x: Pixels, y: Pixels| {
                    (
                        ((x - bounds.left() - pad) / (bounds.size.width - pad * 2.))
                            .clamp(0.0, 1.0),
                        ((y - bounds.top()) / bounds.size.height).clamp(0.0, 1.0),
                    )
                };
                let touch_view = view.clone();
                let touch_hitbox = hitbox.clone();
                let (touch_drag, down_drag, move_drag) = (drag.clone(), drag.clone(), drag.clone());
                window.on_mouse_event(move |event: &TouchDragEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let started = event.phase == TouchPhase::Started;
                    if started && !touch_hitbox.is_hovered(window) {
                        return;
                    }
                    let done = matches!(event.phase, TouchPhase::Ended | TouchPhase::Cancelled);
                    let taken = touch_view.update(cx, |this, cx| {
                        if started {
                            this.sliding = Some(key);
                        } else if this.sliding != Some(key) {
                            return false;
                        }
                        if done {
                            this.sliding = None;
                        }
                        let (x, y) = at(event.position.x, event.position.y);
                        touch_drag(this, x, y, done, cx);
                        true
                    });
                    if started && taken.unwrap_or(false) {
                        // Claims the touch: the page does not scroll.
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                });
                let down_view = view.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase.bubble()
                        && event.button == MouseButton::Left
                        && hitbox.is_hovered(window)
                    {
                        let _ = down_view.update(cx, |this, cx| {
                            this.sliding = Some(key);
                            let (x, y) = at(event.position.x, event.position.y);
                            down_drag(this, x, y, false, cx);
                        });
                    }
                });
                let move_view = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase.bubble() && event.pressed_button == Some(MouseButton::Left) {
                        let _ = move_view.update(cx, |this, cx| {
                            if this.sliding == Some(key) {
                                let (x, y) = at(event.position.x, event.position.y);
                                move_drag(this, x, y, false, cx);
                            }
                        });
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase.bubble() && event.button == MouseButton::Left {
                        let _ = view.update(cx, |this, cx| {
                            if this.sliding == Some(key) {
                                this.sliding = None;
                                let (x, y) = at(event.position.x, event.position.y);
                                drag(this, x, y, true, cx);
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }

    /// A colour picked on the square or the bar: shown as the finger moves,
    /// sent to the light once it lets go.
    fn pick_color(
        &mut self,
        light: &'static str,
        hsv: [f32; 3],
        done: bool,
        cx: &mut Context<Self>,
    ) {
        self.lights.picked.insert(light, hsv);
        self.slide_light(light, |setting| setting.color = to_rgb(hsv), done, cx);
    }

    /// Change one light, show it and keep it.
    fn change_light(
        &mut self,
        light: &'static str,
        change: impl FnOnce(&mut Light),
        cx: &mut Context<Self>,
    ) {
        self.set_light(light, change);
        self.show_light(light, true, cx);
        cx.notify();
    }

    /// A drag's change: the light follows the finger, a few times a second,
    /// and is kept once the finger lets go.
    fn slide_light(
        &mut self,
        light: &'static str,
        change: impl FnOnce(&mut Light),
        done: bool,
        cx: &mut Context<Self>,
    ) {
        self.set_light(light, change);
        let now = Instant::now();
        if done {
            self.lights.shown_at = None;
            self.show_light(light, true, cx);
        } else if self
            .lights
            .shown_at
            .is_none_or(|at| now.duration_since(at) >= LIVE_EVERY)
        {
            self.lights.shown_at = Some(now);
            self.show_light(light, false, cx);
        }
        cx.notify();
    }

    /// Change one light's setting. The fan's, while its zones are set
    /// separately, goes to every zone.
    fn set_light(&mut self, light: &'static str, change: impl FnOnce(&mut Light)) {
        let state = &mut self.lights;
        let mut setting = state.lights.get(light);
        change(&mut setting);
        state.lights.lights.insert(light.to_owned(), setting);
        if light == FAN && state.lights.fan_zones_separate {
            for zone in LIGHTS.iter().filter(|keys| lights::is_fan_zone(keys.light)) {
                state.lights.lights.insert(zone.light.to_owned(), setting);
                state.picked.remove(zone.light);
            }
        }
    }

    fn set_fan_zones_separate(&mut self, separate: bool, cx: &mut Context<Self>) {
        let lights = &mut self.lights.lights;
        if separate {
            // The zones start from the shared setting
            let fan = lights.get(FAN);
            for zone in LIGHTS.iter().filter(|keys| lights::is_fan_zone(keys.light)) {
                lights.lights.entry(zone.light.to_owned()).or_insert(fan);
            }
        }
        lights.fan_zones_separate = separate;
        self.animate_light(Flip::Zones, FAN, separate);
        self.show_light(FAN, true, cx);
        cx.notify();
    }

    /// Write `light`'s LEDs, and save the lights when `keep`.
    fn show_light(&mut self, light: &str, keep: bool, cx: &mut Context<Self>) {
        let state = &self.lights;
        for led in state.leds_of(light) {
            state.writer.write(&led, state.lights.for_led(&led));
        }
        // A failure shows with the change after it
        let mut failed = state.writer.failure();
        if keep && let Err(error) = state.lights.save() {
            failed = Some(format!("Could not keep the lights: {error}"));
        }
        if let Some(note) = failed {
            self.note(note, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_go_round() {
        assert_eq!(to_rgb([0.0, 1.0, 1.0]), 0xff0000);
        assert_eq!(to_rgb([1.0 / 3.0, 1.0, 1.0]), 0x00ff00);
        assert_eq!(to_rgb([2.0 / 3.0, 1.0, 1.0]), 0x0000ff);
        assert_eq!(to_rgb([0.5, 0.0, 1.0]), 0xffffff);
        assert_eq!(to_rgb([0.5, 1.0, 0.0]), 0x000000);
        for color in [
            0xff0000, 0x00ff00, 0x0000ff, 0xffff00, 0x00ffff, 0xff00ff, 0x804020,
        ] {
            assert_eq!(to_rgb(to_hsv(color)), color, "{color:06x}");
        }
    }

    #[test]
    fn keys_are_unique() {
        let mut keys = std::collections::HashSet::new();
        for light in &LIGHTS {
            for key in [
                light.on,
                light.swatches,
                light.square,
                light.hue,
                light.brightness,
                light.effect,
            ] {
                assert!(keys.insert(key), "{key} twice");
            }
        }
        assert!(keys.insert(ZONES_SEPARATE));
    }
}
