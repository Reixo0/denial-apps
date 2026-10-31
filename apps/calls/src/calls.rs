//! The Calls window: its state, what each action does, and how its screens
//! are stacked and animated. The modem's calls come from dmail (`dmail`); the
//! one on screen is `Live`, moved on by what dmail reports. The screens are drawn in the
//! modules below, all to Denial's mobile materials (Islands,
//! denial/docs/MOBILE_APP_MATERIALS.md), as Messages draws its own.

mod call;
mod contacts;
mod keypad;
mod parts;
mod recents;

use denial_look::words::tr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use denial_contacts::{Contacts, Watch, dialable, same_number};
use futures_lite::StreamExt;
use denial_look::mobile::{
    GESTURE_INSET, GROUP_HEIGHT, KEYBOARD_GAP, ScrollAway, TAB_BAR, content_bottom, dp as dp_lp,
};
use denial_look::{
    INSET, Look, SettingsWatch, Tween, decelerate, ease_in_cubic, ease_out_cubic, motion, stagger,
};
use denial_look_gpui::input::TextInput;
use denial_look_gpui::{backing, dp, solid};
use gpui::{
    AnyElement, App, AsyncApp, AsyncWindowContext, ClipboardItem, Context, Div, Entity,
    FocusHandle, Focusable, Font, FontFallbacks, KeyDownEvent, KeyboardInset, ListAlignment,
    ListState, SharedString, Subscription, Task, WeakEntity, Window, actions, div, prelude::*, px,
};

use crate::data::{self, Call, Direction};
use crate::dmail::{CallAudio1Proxy, CallRow, NotificationsProxy, Sensors1Proxy, Voice1Proxy};
use crate::local::{CallLog, Favourites};
use crate::people::People;

actions!(calls, [Back]);

/// Items past this many arrive together: further ones are off screen.
const STAGGERED: usize = 12;
/// Emoji the UI font lacks are drawn from this font first, as in Messages.
const EMOJI_FONT: &str = "Noto Color Emoji";
/// The id of the first call in the log.
const FIRST_ID: u64 = 1;
/// A call that ended stays on screen this long.
const ENDED_FOR: Duration = Duration::from_millis(1200);
/// Recent calls shown on a contact.
const HISTORY: usize = 10;

/// Whom a detail screen is about.
#[derive(Clone, PartialEq)]
pub(crate) enum Who {
    /// A contact, by `People::key`.
    Contact(String),
    /// A number no contact has.
    Number(String),
}

#[derive(Clone, PartialEq)]
enum Screen {
    Recents,
    Contacts,
    Keypad,
    Person(Who),
    /// The call, while there is one.
    Call,
}

impl Screen {
    /// One of the three screens the tab bar switches between.
    fn tab(&self) -> bool {
        self.tab_index().is_some()
    }

    /// Where it is in the tab bar, left to right.
    fn tab_index(&self) -> Option<usize> {
        match self {
            Screen::Recents => Some(0),
            Screen::Contacts => Some(1),
            Screen::Keypad => Some(2),
            Screen::Person(_) | Screen::Call => None,
        }
    }

    /// It has the search bar at the top.
    fn searchable(&self) -> bool {
        matches!(self, Screen::Recents | Screen::Contacts)
    }
}

/// What a long press lifted.
#[derive(Clone, PartialEq)]
enum Held {
    /// A call in the log, by id.
    Call(u64),
    /// A contact, by key.
    Contact(String),
}

/// Something that cannot be undone, asked about first.
#[derive(Clone, PartialEq)]
enum Confirm {
    DeleteCall(u64),
}

/// What sits over the screen and dims the rest of it.
#[derive(Clone, PartialEq)]
enum Overlay {
    /// Lifted, with its actions under it.
    Held(Held),
    Confirm(Confirm),
}

/// Which of the screens drawn at once: the one shown, or the one leaving
/// during a switch, which takes no input.
#[derive(Clone, Copy, PartialEq)]
enum Layer {
    Shown,
    Leaving,
}

impl Layer {
    fn live(self) -> bool {
        self == Layer::Shown
    }
}

/// Where an island is drawn while its screen comes or goes: faded and moved
/// down by `offset` lp.
#[derive(Clone, Copy)]
struct Motion {
    opacity: f32,
    offset: f32,
}

/// How the shown screen came in: at launch, opened from another screen, or
/// back from one, which leaves meanwhile; between tabs, sliding in beside
/// the one that leaves.
struct Arrival {
    clock: Tween,
    launch: bool,
    /// Back runs the opening in reverse: the leaving islands drop as they
    /// fade, and the arriving ones only fade in.
    back: bool,
    leaving: Option<Screen>,
    /// Between tabs: 1 when the shown tab is right of the leaving one, -1
    /// when it is left of it.
    slide: Option<f32>,
    /// The leaving screen had the bar of a call going on behind it.
    call_bar: bool,
}

const STILL: Motion = Motion {
    opacity: 1.0,
    offset: 0.0,
};

impl Arrival {
    /// Island `index`, counted from the one nearest where the eye starts.
    fn item(&self, index: usize, now: Instant) -> Motion {
        if self.slide.is_some() {
            return STILL;
        }
        let index = index.min(STAGGERED);
        let (t, rise) = if self.launch {
            let delay = stagger(motion::LAUNCH_DELAY, motion::LAUNCH_STEP, index);
            let t = self.clock.segment(delay, motion::LAUNCH).progress(now);
            (ease_out_cubic(t), dp_lp(motion::LAUNCH_RISE))
        } else {
            let delay = stagger(motion::OUT, motion::IN_STEP, index);
            let t = self.clock.segment(delay, motion::IN).progress(now);
            (decelerate(t), if self.back { 0.0 } else { motion::IN_RISE })
        };
        Motion {
            opacity: t,
            offset: rise * (1.0 - t),
        }
    }

    /// An island of the leaving screen.
    fn leaving_item(&self, now: Instant) -> Motion {
        if self.slide.is_some() {
            return STILL;
        }
        let gone = self.gone(now).unwrap_or(1.0);
        Motion {
            opacity: 1.0 - gone,
            offset: if self.back {
                motion::IN_RISE * gone
            } else {
                0.0
            },
        }
    }

    /// The contents of the glass groups.
    fn group(&self, now: Instant) -> f32 {
        if self.launch {
            self.clock.segment(0, motion::GROUP_FADE).progress(now)
        } else {
            let in_ = self.clock.segment(motion::GROUP_IN_DELAY, motion::GROUP_IN);
            in_.progress(now)
        }
    }

    /// The bottom zone: it fades in at launch and rises in on a switch.
    fn bottom(&self, now: Instant) -> Motion {
        if self.slide.is_some() {
            return STILL;
        }
        if self.launch {
            return Motion {
                opacity: self.group(now),
                offset: 0.0,
            };
        }
        let t = decelerate(
            self.clock
                .segment(motion::BOTTOM_DELAY, motion::IN)
                .progress(now),
        );
        Motion {
            opacity: t,
            offset: motion::BOTTOM_TRAVEL * (1.0 - t),
        }
    }

    /// The bottom zone of the leaving screen, dropping away as it fades.
    fn leaving_bottom(&self, now: Instant) -> Motion {
        if self.slide.is_some() {
            return STILL;
        }
        let gone = self.gone(now).unwrap_or(1.0);
        Motion {
            opacity: 1.0 - gone,
            offset: motion::BOTTOM_TRAVEL * gone,
        }
    }

    /// How far the leaving screen is gone, 0 to 1, while it shows.
    fn gone(&self, now: Instant) -> Option<f32> {
        self.leaving.as_ref()?;
        if self.slide.is_some() {
            let slid = self.clock.segment(0, motion::SLIDE).progress(now);
            return (slid < 1.0).then(|| decelerate(slid));
        }
        let gone = self.clock.segment(0, motion::OUT).progress(now);
        (gone < 1.0).then(|| ease_in_cubic(gone))
    }

    /// How far a screen of `layer` is moved sideways, lp, while the tabs
    /// slide in a window `width` wide.
    fn shift(&self, layer: Layer, width: f32, now: Instant) -> f32 {
        let (Some(direction), Some(slid)) = (self.slide, self.gone(now)) else {
            return 0.0;
        };
        match layer {
            Layer::Shown => direction * width * (1.0 - slid),
            Layer::Leaving => -direction * width * slid,
        }
    }

    fn running(&self, now: Instant) -> bool {
        if self.slide.is_some() {
            return self.clock.segment(motion::SLIDE, 0).running(now);
        }
        let end = if self.launch {
            stagger(motion::LAUNCH_DELAY, motion::LAUNCH_STEP, STAGGERED) + motion::LAUNCH
        } else {
            stagger(motion::OUT, motion::IN_STEP, STAGGERED) + motion::IN
        };
        self.clock.segment(end, 0).running(now)
    }
}

/// How much of the window the on-screen keyboard covers, in lp, as the shell
/// moves it from `from` to `to` over `motion`.
struct Keyboard {
    from: f32,
    to: f32,
    motion: Tween,
}

impl Keyboard {
    /// The height now, decelerating like the shell's keyboard.
    fn height(&self, now: Instant) -> f32 {
        denial_look::lerp(
            self.from,
            self.to,
            ease_out_cubic(self.motion.progress(now)),
        )
    }
}

/// Where a call is.
#[derive(Clone, Copy)]
enum Phase {
    /// Ringing since then: out, or here for a call coming in.
    Ringing(Instant),
    /// Talking since then.
    Connected(Instant),
    /// Over then, after this many seconds of talk; shown a moment, then
    /// closed.
    Ended(Instant, u64),
}

/// The call on screen.
struct Live {
    id: u64,
    /// dmail's id for it; none until a call placed here has one.
    modem: Option<u8>,
    /// End was pressed before the call had its id.
    end_when_known: bool,
    /// Decline was pressed: it ended as turned down, not missed.
    turned_down: bool,
    number: String,
    incoming: bool,
    phase: Phase,
    /// Unix time it started ringing.
    time: i64,
    muted: bool,
    speaker: bool,
    held: bool,
    /// The in-call keypad shows in place of the controls.
    keypad: bool,
    /// What was sent with the in-call keypad.
    tones: String,
    /// Moves the call on and its clock with it; replaced on each change.
    _clock: Task<()>,
}

impl Live {
    /// Seconds talked so far.
    fn talked(&self, now: Instant) -> u64 {
        match self.phase {
            Phase::Ringing(_) => 0,
            Phase::Connected(since) => now.saturating_duration_since(since).as_secs(),
            Phase::Ended(_, talked) => talked,
        }
    }

    fn ended(&self) -> bool {
        matches!(self.phase, Phase::Ended(..))
    }

    fn ringing_here(&self) -> bool {
        self.incoming && matches!(self.phase, Phase::Ringing(_))
    }

    /// How long until the call next changes on screen: it is answered or
    /// missed, its clock turns a second, or its end has shown long enough.
    fn wake(&self, now: Instant) -> Duration {
        let at = match self.phase {
            // Moved on by dmail, not by time.
            Phase::Ringing(_) => now + Duration::from_secs(1),
            Phase::Connected(since) => {
                let into = now.saturating_duration_since(since);
                now + Duration::from_secs(1) - Duration::from_nanos(into.subsec_nanos().into())
            }
            Phase::Ended(at, _) => at + ENDED_FOR,
        };
        at.saturating_duration_since(now)
    }
}

/// An item of the contacts list, top to bottom.
#[derive(Clone, Copy)]
enum Entry {
    Favourites,
    Contact(usize),
}

/// An item of the search results, top to bottom.
#[derive(Clone)]
enum Found {
    /// The typed number, which no contact has.
    Dial(String),
    /// A contact whose name or number matches.
    Contact(usize),
    /// Why nothing is listed.
    Chip(&'static str),
}

pub struct Calls {
    look: Look,
    /// The user's font, with colour emoji first among its fallbacks.
    font: Font,
    focus_handle: FocusHandle,
    /// The contacts folder, and what is in it.
    folder: Option<PathBuf>,
    people: People,
    /// The call log, newest first.
    log: Vec<Call>,
    /// The real calls of `log`, on the phone.
    saved: CallLog,
    voice: Option<Voice1Proxy<'static>>,
    audio: Option<CallAudio1Proxy<'static>>,
    _follow_voice: Task<()>,
    /// Something covers the screen: a cheek must not press the call's controls.
    near: bool,
    _follow_near: Task<()>,
    /// The id of the next call.
    next_id: u64,
    favourites: Favourites,
    /// The search bar's field, at the top of Recents and Contacts.
    search: Entity<TextInput>,
    /// The search bar leaves as Recents or Contacts
    /// scroll on, and come back as they scroll back.
    top_away: ScrollAway,
    /// The number on the keypad.
    typed: String,
    /// The number called last, which Call brings back on an empty keypad.
    last_dialled: Option<String>,
    live: Option<Live>,
    overlay: Option<(Overlay, Tween)>,
    /// A short note above the bottom zone, until its task hides it.
    notice: Option<(SharedString, Tween, Task<()>)>,
    screen: Screen,
    /// The screens under the shown one, which Back returns to; the tabs have
    /// none.
    trail: Vec<Screen>,
    arrival: Arrival,
    /// The on-screen keyboard, which the window is laid out around.
    keyboard: Keyboard,
    recents: ListState,
    contacts: ListState,
    found: ListState,
    /// What the contacts list and the search results hold, as of the last
    /// frame.
    entries: Vec<Entry>,
    found_items: Vec<Found>,
    _contacts: Task<()>,
    _settings: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Calls {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Calls {
    /// Opens on Recents, or on the keypad with `number` on it.
    pub fn new(number: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let look = Look::load();
        let search = cx.new(|cx| TextInput::new(tr("Search contacts and numbers"), &look, 1, cx));
        let mut subscriptions = vec![cx.observe(&search, |this, _, cx| {
            this.found.reset(0);
            cx.notify()
        })];
        // Denial stops lifting the window for the keyboard and reports it
        // instead; elsewhere the window stays as it is.
        if window.handle_keyboard_inset() {
            subscriptions.push(cx.observe_keyboard_inset(window, |this, window, cx| {
                this.keyboard_moved(window.keyboard_inset(), window, cx)
            }));
        }
        let folder = Contacts::folder();
        let people = People::read(folder.as_deref());
        let saved = CallLog::new();
        let log = saved.load(FIRST_ID);
        let next_id = FIRST_ID + log.len() as u64;
        let (screen, typed) = match number {
            Some(number) => (Screen::Keypad, keypad::keyable(&number)),
            None => (Screen::Recents, String::new()),
        };
        let font = Self::font(&look, cx);
        let arrival = Arrival {
            clock: look.tween(0),
            launch: true,
            back: false,
            leaving: None,
            slide: None,
            call_bar: false,
        };
        let keyboard = Keyboard {
            from: 0.0,
            to: 0.0,
            motion: look.tween(0),
        };
        Self {
            look,
            font,
            focus_handle: cx.focus_handle(),
            _contacts: match folder.clone() {
                Some(folder) => {
                    cx.spawn(async move |this, cx| Self::follow_contacts(this, folder, cx).await)
                }
                None => Task::ready(()),
            },
            folder,
            people,
            log,
            saved,
            voice: None,
            audio: None,
            _follow_voice: cx.spawn_in(window, async move |this, cx| {
                Self::follow_voice(this, cx).await
            }),
            near: false,
            _follow_near: cx.spawn_in(window, async move |this, cx| {
                Self::follow_near(this, cx).await
            }),
            next_id,
            favourites: Favourites::load(),
            search,
            top_away: ScrollAway::default(),
            typed,
            last_dialled: None,
            live: None,
            overlay: None,
            notice: None,
            screen,
            trail: Vec::new(),
            arrival,
            keyboard,
            recents: ListState::new(0, ListAlignment::Top, dp(600.)),
            contacts: ListState::new(0, ListAlignment::Top, dp(600.)),
            found: ListState::new(0, ListAlignment::Top, dp(600.)),
            entries: Vec::new(),
            found_items: Vec::new(),
            _settings: cx.spawn(async move |this, cx| Self::follow_settings(this, cx).await),
            _subscriptions: subscriptions,
        }
    }

    /// The user's font, with colour emoji first among its fallbacks.
    fn font(look: &Look, cx: &App) -> Font {
        Font {
            fallbacks: Some(FontFallbacks::from_fonts(vec![EMOJI_FONT.into()])),
            ..gpui::font(look.font_family(&cx.text_system().all_font_names()))
        }
    }

    /// Rereads the contacts whenever their folder changes.
    async fn follow_contacts(this: WeakEntity<Self>, folder: PathBuf, cx: &mut AsyncApp) {
        let mut watch = match Watch::new(&folder) {
            Ok(watch) => watch,
            Err(error) => return eprintln!("calls: following {}: {error}", folder.display()),
        };
        loop {
            if let Err(error) = watch.changed().await {
                return eprintln!("calls: following {}: {error}", folder.display());
            }
            let taken = this.update(cx, |view, cx| view.reread_people(cx));
            if taken.is_err() {
                return;
            }
        }
    }

    /// Takes Denial's look afresh whenever its settings change.
    async fn follow_settings(this: WeakEntity<Self>, cx: &mut AsyncApp) {
        let mut watch = match SettingsWatch::new() {
            Ok(watch) => watch,
            Err(error) => return eprintln!("calls: following Denial's settings: {error}"),
        };
        loop {
            if let Err(error) = watch.changed().await {
                return eprintln!("calls: following Denial's settings: {error}");
            }
            let look = cx.background_spawn(async { Look::load() }).await;
            let taken = this.update(cx, |view, cx| {
                view.font = Self::font(&look, cx);
                view.search
                    .update(cx, |search, cx| search.restyle(&look, cx));
                view.look = look;
                cx.notify();
            });
            if taken.is_err() {
                return;
            }
        }
    }

    /// Reads the contacts again.
    fn reread_people(&mut self, cx: &mut Context<Self>) {
        self.people = People::read(self.folder.as_deref());
        for list in [&self.recents, &self.contacts, &self.found] {
            list.reset(0);
        }
        cx.notify();
    }

    /// The calls with `who`, newest first.
    fn history(&self, who: &Who) -> impl Iterator<Item = &Call> {
        self.log.iter().filter(move |call| match who {
            Who::Contact(key) => self
                .people
                .find(&call.number)
                .is_some_and(|index| self.people.key(index) == key),
            Who::Number(number) => same_number(&call.number, number),
        })
    }

    /// Whom `number` reaches: their contact, else the number itself.
    fn who(&self, number: &str) -> Who {
        match self.people.find(number) {
            Some(index) => Who::Contact(self.people.key(index).to_owned()),
            None => Who::Number(number.to_owned()),
        }
    }

    /// The contact a detail screen is about, if it is a contact's.
    fn contact_of(&self, who: &Who) -> Option<usize> {
        match who {
            Who::Contact(key) => self.people.by_key(key),
            Who::Number(_) => None,
        }
    }

    /// The number to call or message `who` on: a contact's first.
    fn number_of(&self, who: &Who) -> Option<String> {
        match who {
            Who::Contact(_) => {
                let contact = &self.people.all()[self.contact_of(who)?];
                contact.phones.first().map(|phone| phone.number.clone())
            }
            Who::Number(number) => Some(number.clone()),
        }
    }

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Follows dmail's calls for as long as the window lives.
    async fn follow_voice(this: WeakEntity<Self>, cx: &mut AsyncWindowContext) {
        let connected = async {
            let voice = Voice1Proxy::new(&zbus::Connection::system().await?).await?;
            let audio = CallAudio1Proxy::new(&zbus::Connection::session().await?).await?;
            zbus::Result::Ok((voice, audio))
        };
        let (voice, audio) = match connected.await {
            Ok(proxies) => proxies,
            Err(error) => return eprintln!("calls: reaching dmail: {error}"),
        };
        let mut changes = voice.receive_calls_changed().await;
        let first = voice.calls().await.unwrap_or_default();
        let taken = this.update_in(cx, |view, window, cx| {
            view.voice = Some(voice);
            view.audio = Some(audio);
            view.sync(first, window, cx);
        });
        if taken.is_err() {
            return;
        }
        while let Some(change) = changes.next().await {
            let rows = change.get().await.unwrap_or_default();
            let taken = this.update_in(cx, |view, window, cx| view.sync(rows, window, cx));
            if taken.is_err() {
                return;
            }
        }
    }

    /// Follows the proximity sensor, for as long as the window lives.
    async fn follow_near(this: WeakEntity<Self>, cx: &mut AsyncWindowContext) {
        let connected = async {
            let bus = zbus::Connection::system().await?;
            Sensors1Proxy::new(&bus).await
        };
        let sensors = match connected.await {
            Ok(sensors) => sensors,
            Err(error) => return eprintln!("calls: reaching steinerd: {error}"),
        };
        let mut changes = sensors.receive_near_changed().await;
        while let Some(change) = changes.next().await {
            let near = change.get().await.unwrap_or(false);
            let taken = this.update(cx, |view, cx| {
                view.near = near;
                cx.notify();
            });
            if taken.is_err() {
                return;
            }
        }
    }

    /// Takes dmail's list of calls: shows a call that started, moves the one
    /// on screen on, and ends it when it left the list. One call at a time: a
    /// second one waits unseen until the first is over.
    fn sync(&mut self, rows: Vec<CallRow>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_mut().filter(|live| !live.ended()) else {
            let Some(row) = rows.iter().find(|row| row.3 != "ending") else {
                return;
            };
            let number = if row.1.is_empty() {
                tr("No caller ID").to_owned()
            } else {
                row.1.clone()
            };
            self.ring(number, row.2 == "in", Some(row.0), window, cx);
            return self.sync(rows, window, cx);
        };
        // A call placed here can show in the list before Dial answers.
        let known = live.modem.or_else(|| {
            rows.iter()
                .find(|row| row.2 == "out")
                .map(|row| row.0)
        });
        let Some(id) = known else {
            return;
        };
        live.modem = Some(id);
        let Some(row) = rows.iter().find(|row| row.0 == id) else {
            let direction = match (live.incoming, live.phase, live.turned_down) {
                (false, ..) => Direction::Outgoing,
                (true, Phase::Connected(_), _) => Direction::Incoming,
                (true, _, true) => Direction::Declined,
                (true, _, false) => Direction::Missed,
            };
            return self.finish(direction, cx);
        };
        live.held = row.3 == "held";
        if matches!(row.3.as_str(), "active" | "held")
            && !matches!(live.phase, Phase::Connected(_))
        {
            let ago = (data::now() - row.4).max(0) as u64;
            let since = if row.4 == 0 { 0 } else { ago };
            live.phase = Phase::Connected(Instant::now() - Duration::from_secs(since));
        }
        if live.end_when_known {
            live.end_when_known = false;
            self.hang_up(window, cx);
        }
        cx.notify();
    }

    /// Places a call to `number`. One call at a time: while one runs, its
    /// screen shows instead.
    fn dial(&mut self, number: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.live.as_ref().is_some_and(|live| !live.ended()) {
            return self.open(Screen::Call, window, cx);
        }
        let Some(number) = dialable(&number).map(|_| number) else {
            return self.note(tr("Not a phone number"), cx);
        };
        let Some(voice) = self.voice.clone() else {
            return self.note(tr("The modem is not available"), cx);
        };
        self.overlay = None;
        self.last_dialled = Some(number.clone());
        // dmail takes digits, +, * and #; contacts may space them out.
        let plain: String = number
            .chars()
            .filter(|c| c.is_ascii_digit() || "+*#".contains(*c))
            .collect();
        self.ring(number, false, None, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let placed = voice.dial(&plain).await;
            let _ = this.update_in(cx, |view, window, cx| match placed {
                Ok(id) => {
                    if let Some(live) = view.live.as_mut().filter(|live| live.modem.is_none()) {
                        live.modem = Some(id);
                        if live.end_when_known {
                            live.end_when_known = false;
                            view.hang_up(window, cx);
                        }
                    }
                }
                Err(error) => {
                    eprintln!("calls: dialling: {error}");
                    view.note(tr("The call could not be placed"), cx);
                    view.finish(Direction::Outgoing, cx);
                }
            });
        })
        .detach();
    }

    /// Starts a call with `number` ringing, and shows it.
    fn ring(
        &mut self,
        number: String,
        incoming: bool,
        modem: Option<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.take_id();
        // Opened first: the screen it leaves had no call going on behind it.
        self.open(Screen::Call, window, cx);
        self.live = Some(Live {
            id,
            modem,
            end_when_known: false,
            turned_down: false,
            number,
            incoming,
            phase: Phase::Ringing(Instant::now()),
            time: data::now(),
            muted: false,
            speaker: false,
            held: false,
            keypad: false,
            tones: String::new(),
            _clock: self.clock(id, window, cx),
        });
    }

    /// Moves call `id` on whenever it next changes, for as long as it is the
    /// one on screen.
    fn clock(&self, id: u64, window: &Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            loop {
                let wake = this.update_in(cx, |view, window, cx| view.advance(id, window, cx));
                let Ok(Some(wake)) = wake else {
                    return;
                };
                cx.background_executor().timer(wake).await;
            }
        })
    }

    /// Where call `id` should be by now; how long until it next changes,
    /// none once it is gone.
    fn advance(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Duration> {
        let now = Instant::now();
        let live = self.live.as_mut().filter(|live| live.id == id)?;
        match live.phase {
            Phase::Ended(at, _) if now >= at + ENDED_FOR => {
                self.close_call(window, cx);
                return None;
            }
            _ => {}
        }
        cx.notify();
        self.live.as_ref().map(|live| live.wake(now))
    }

    /// Asks dmail to answer the call ringing here; `sync` shows the result.
    fn answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.live.as_ref().filter(|l| l.ringing_here()).and_then(|l| l.modem) else {
            return;
        };
        let Some(voice) = self.voice.clone() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = voice.answer(id).await {
                eprintln!("calls: answering: {error}");
                let _ = this.update(cx, |view, cx| view.note(tr("The call could not be answered"), cx));
            }
        })
        .detach();
    }

    /// Asks dmail to end the call, or turn down the one ringing here; `sync`
    /// ends it on screen when it leaves dmail's list.
    fn hang_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_mut().filter(|live| !live.ended()) else {
            return;
        };
        live.turned_down = live.ringing_here();
        let (Some(id), Some(voice)) = (live.modem, self.voice.clone()) else {
            live.end_when_known = true;
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = voice.hangup(id).await {
                eprintln!("calls: ending the call: {error}");
                let _ = this.update(cx, |view, cx| view.note(tr("The call could not be ended"), cx));
            }
        })
        .detach();
    }

    /// Turns the microphone off or on for the other side, through callaudio.
    pub(super) fn toggle_mute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(audio) = self.audio.clone() else {
            return;
        };
        let muted = !live.muted;
        live.muted = muted;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = audio.set_muted(muted).await {
                eprintln!("calls: muting: {error}");
                let _ = this.update(cx, |view, cx| {
                    if let Some(live) = view.live.as_mut() {
                        live.muted = !muted;
                    }
                    view.note(tr("The microphone could not be changed"), cx);
                });
            }
        })
        .detach();
    }

    /// Moves the call between the earpiece and the speakers, through callaudio.
    pub(super) fn toggle_speaker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Some(audio) = self.audio.clone() else {
            return;
        };
        let speaker = !live.speaker;
        live.speaker = speaker;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = audio.set_speaker(speaker).await {
                eprintln!("calls: changing the speaker: {error}");
                let _ = this.update(cx, |view, cx| {
                    if let Some(live) = view.live.as_mut() {
                        live.speaker = !speaker;
                    }
                    view.note(tr("The speaker could not be changed"), cx);
                });
            }
        })
        .detach();
    }

    /// Tells of a call no one answered.
    fn notify_missed(&self, number: &str, cx: &mut Context<Self>) {
        let name = self.people.name(number).to_owned();
        cx.background_spawn(async move {
            let posted = async {
                let bus = zbus::Connection::session().await?;
                let hints = std::collections::HashMap::from([(
                    "desktop-entry",
                    zbus::zvariant::Value::from(crate::instance::NAME),
                )]);
                NotificationsProxy::new(&bus)
                    .await?
                    .notify("Calls", 0, "call-missed", tr("Missed call"), &name, &[], hints, -1)
                    .await
            };
            if let Err(error) = posted.await {
                eprintln!("calls: notifying: {error}");
            }
        })
        .detach();
    }

    /// The call is over, as `direction` says: it goes in the log and shows
    /// that it ended until its clock closes it.
    fn finish(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let now = Instant::now();
        let Some(live) = self.live.as_mut().filter(|live| !live.ended()) else {
            return;
        };
        let talked = live.talked(now);
        live.phase = Phase::Ended(now, talked);
        live.keypad = false;
        let call = Call {
            id: live.id,
            number: live.number.clone(),
            direction,
            time: live.time,
            talked,
        };
        let missed = (direction == Direction::Missed).then(|| call.number.clone());
        self.log.insert(0, call);
        self.saved.save(&self.log);
        if let Some(number) = missed {
            self.notify_missed(&number, cx);
        }
        self.recents.reset(0);
        cx.notify();
    }

    /// Takes the ended call off the screen.
    fn close_call(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.live = None;
        self.trail.retain(|screen| *screen != Screen::Call);
        if self.screen == Screen::Call {
            self.go_back(window, cx);
        }
        cx.notify();
    }

    /// Opens Messages on the conversation with `number`.
    fn message(&mut self, number: &str, cx: &mut Context<Self>) {
        self.overlay = None;
        // Installed beside Calls, else on the PATH. A running Messages takes
        // the number and raises its window.
        let beside = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("messages")))
            .filter(|messages| messages.exists());
        let program = beside.unwrap_or_else(|| "messages".into());
        match std::process::Command::new(program).arg(number).spawn() {
            // Waited for, so it leaves no zombie when it exits.
            Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
            Err(error) => {
                eprintln!("calls: starting Messages: {error}");
                self.note(tr("Messages is not installed"), cx);
            }
        }
    }

    fn copy(&mut self, text: String, note: &'static str, cx: &mut Context<Self>) {
        self.overlay = None;
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.note(note, cx);
    }

    /// Copies a contact's name and numbers, to paste wherever it goes.
    fn share(&mut self, index: usize, cx: &mut Context<Self>) {
        let contact = &self.people.all()[index];
        let mut text = contact.name.clone();
        for phone in &contact.phones {
            text.push_str(&format!("\n{}: {}", tr(phone.kind.label()), phone.number));
        }
        self.copy(text, tr("Contact copied"), cx);
    }

    fn toggle_favourite(&mut self, key: String, cx: &mut Context<Self>) {
        self.overlay = None;
        let favourite = self.favourites.contains(&key);
        self.favourites.set(&key, !favourite);
        self.contacts.reset(0);
        cx.notify();
    }

    fn delete_call(&mut self, id: u64, cx: &mut Context<Self>) {
        self.log.retain(|call| call.id != id);
        self.saved.save(&self.log);
        self.recents.reset(0);
        self.note(tr("Deleted"), cx);
    }

    fn open(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        let leaving = self.screen.clone();
        if screen == leaving {
            return;
        }
        if screen.tab() {
            self.trail.clear();
        } else {
            self.trail.push(leaving);
        }
        self.switch(screen, false, window, cx);
    }

    /// Back to the screen under the shown one; a call that is over is
    /// skipped.
    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut parent = self.trail.pop().unwrap_or(Screen::Recents);
        while parent == Screen::Call && self.live.is_none() {
            parent = self.trail.pop().unwrap_or(Screen::Recents);
        }
        self.switch(parent, true, window, cx);
    }

    /// Switches to `screen`: the shown one leaves as the new one arrives, in
    /// reverse when going `back`.
    fn switch(&mut self, screen: Screen, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        self.search.update(cx, |search, cx| search.clear(cx));
        let slide = match (self.screen.tab_index(), screen.tab_index()) {
            (Some(from), Some(to)) => Some(if to > from { 1.0 } else { -1.0 }),
            _ => None,
        };
        let leaving = std::mem::replace(&mut self.screen, screen);
        self.top_away = ScrollAway::shown(self.scrolled(&self.screen).unwrap_or(0.0));
        self.arrival = Arrival {
            clock: self.look.tween(0),
            launch: false,
            back,
            call_bar: self.call_behind(&leaving),
            leaving: Some(leaving),
            slide,
        };
        // Leaving search closes the on-screen keyboard.
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Shows `number` on the keypad, as asked from outside by `calls
    /// <number>` while Calls runs.
    pub fn show_number(&mut self, number: String, window: &mut Window, cx: &mut Context<Self>) {
        self.typed = keypad::keyable(&number);
        self.open(Screen::Keypad, window, cx);
    }

    /// What the search bar holds, when it holds anything.
    fn searched(&self, cx: &App) -> bool {
        !self.search.read(cx).text().trim().is_empty()
    }

    /// Clears the search bar and lets go of it, which puts the keyboard
    /// away.
    fn end_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.clear(cx));
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Denial's Back gesture, Escape and the top group's back button: undoes
    /// the innermost state. An overlay closes; then the search bar clears and
    /// lets go; then the screen pops; on a tab, the window closes. A call
    /// goes on behind the screens it goes back to.
    fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        let typing = self.search.focus_handle(cx).is_focused(window);
        if self.overlay.is_some() {
            self.dismiss(cx);
        } else if self.screen.searchable() && (typing || self.searched(cx)) {
            self.end_search(window, cx);
        } else if self.screen.tab() {
            window.remove_window();
        } else {
            self.go_back(window, cx);
        }
    }

    /// Digits typed on a hardware keyboard go to the keypad, or out as tones
    /// during a call.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if self.overlay.is_some() || keystroke.modifiers.control || keystroke.modifiers.alt {
            return;
        }
        let typed = keystroke.key_char.as_deref().and_then(keypad::key);
        match (&self.screen, keystroke.key.as_str()) {
            (Screen::Keypad, "backspace") => self.erase(false, cx),
            (Screen::Keypad, "enter") => self.dial_typed(window, cx),
            (Screen::Keypad, _) if let Some(key) = typed => self.press_key(key, cx),
            (Screen::Call, _) if let Some(key) = typed.filter(|&key| key != '+') => {
                self.tone(key, cx)
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn show(&mut self, overlay: Overlay, cx: &mut Context<Self>) {
        self.overlay = Some((overlay, self.look.tween(0)));
        cx.notify();
    }

    /// Lifts `held`, or drops it when it is lifted already.
    fn hold(&mut self, held: Held, cx: &mut Context<Self>) {
        let overlay = Overlay::Held(held);
        self.overlay = match &self.overlay {
            Some((shown, _)) if *shown == overlay => None,
            _ => Some((overlay, self.look.tween(0))),
        };
        cx.notify();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.overlay.take().is_some() {
            cx.notify();
        }
    }

    fn confirmed(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        self.overlay = None;
        match confirm {
            Confirm::DeleteCall(id) => self.delete_call(id, cx),
        }
        cx.notify();
    }

    /// Shows `note` on a chip above the bottom zone for a moment.
    fn note(&mut self, note: impl Into<SharedString>, cx: &mut Context<Self>) {
        let hide = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(motion::NOTICE))
                .await;
            let _ = this.update(cx, |view, cx| {
                view.notice = None;
                cx.notify();
            });
        });
        self.notice = Some((note.into(), self.look.tween(motion::CHIP_FADE), hide));
        cx.notify();
    }

    /// Follows the keyboard to its new height, over the shell's motion there.
    fn keyboard_moved(
        &mut self,
        inset: KeyboardInset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The user put the keyboard away, by dragging it down or with Back:
        // the field it typed into lets go of the focus, so a tap brings it back.
        if inset.height == px(0.) && self.keyboard.to > 0.0 {
            window.focus(&self.focus_handle, cx);
        }
        self.keyboard = Keyboard {
            from: self.keyboard.height(Instant::now()),
            to: f32::from(inset.height),
            motion: self.look.tween(inset.duration.as_millis() as u64),
        };
        cx.notify();
    }

    fn animating(&self, now: Instant) -> bool {
        let tweens = [
            self.overlay
                .as_ref()
                .map(|(_, lift)| lift.segment(0, motion::DIM)),
            self.notice.as_ref().map(|(_, fade, _)| *fade),
            Some(self.keyboard.motion),
        ];
        self.arrival.running(now) || tweens.iter().flatten().any(|tween| tween.running(now))
    }

    /// How far the rest of the screen is dimmed behind an overlay, 0 to 1.
    fn dim(&self, now: Instant) -> f32 {
        self.overlay
            .as_ref()
            .map_or(0.0, |(_, lift)| lift.segment(0, motion::DIM).progress(now))
    }

    /// Item `index` of a screen's content: where it is while the screen comes
    /// or goes, and dimmed behind an overlay unless it is what the overlay is
    /// about.
    fn item_motion(&self, layer: Layer, index: usize, lifted: bool, now: Instant) -> Motion {
        let motion = match layer {
            Layer::Shown => self.arrival.item(index, now),
            Layer::Leaving => self.arrival.leaving_item(now),
        };
        let dim = if lifted || !layer.live() {
            1.0
        } else {
            denial_look::lerp(1.0, motion::DIM_ALPHA, self.dim(now))
        };
        Motion {
            opacity: motion.opacity * dim,
            ..motion
        }
    }

    /// The bars that stay on the gesture zone under `screen`'s content, lp
    /// from their lower edge to their top: the tab bar, and the bar of a call
    /// going on behind; 0 when there are none.
    fn fixed_zone(&self, screen: &Screen) -> f32 {
        match (screen.tab(), self.call_behind(screen)) {
            (true, true) => dp_lp(TAB_BAR) + INSET + dp_lp(GROUP_HEIGHT),
            (true, false) => dp_lp(TAB_BAR),
            (false, true) => dp_lp(GROUP_HEIGHT),
            (false, false) => 0.0,
        }
    }

    /// How far `screen`'s list is scrolled, lp, when it has the search bar.
    fn scrolled(&self, screen: &Screen) -> Option<f32> {
        let list = match screen {
            Screen::Recents => &self.recents,
            Screen::Contacts => &self.contacts,
            _ => return None,
        };
        Some(-f32::from(list.scroll_px_offset_for_scrollbar().y))
    }

    /// Moves the search bar with the shown list's scroll. It stays while
    /// the search is in use.
    fn follow_scroll(&mut self, window: &Window, cx: &App) {
        let offset = self.scrolled(&self.screen);
        let searching = self.searched(cx) || self.search.focus_handle(cx).is_focused(window);
        match offset {
            Some(offset) if !searching => {
                self.top_away.follow(offset, INSET + dp_lp(GROUP_HEIGHT));
            }
            offset => self.top_away = ScrollAway::shown(offset.unwrap_or(0.0)),
        }
    }

    /// Padding under the last island of `screen`'s scrolling content: clear
    /// of its bars and of the keyboard.
    fn bottom_padding(&self, screen: &Screen, now: Instant) -> f32 {
        content_bottom(self.keyboard.height(now), self.fixed_zone(screen), 0.0)
    }

    /// Where `screen`'s bottom zone sits, lp above the window's lower edge:
    /// on its bars, or on the gesture zone or the keyboard.
    fn zone_inset(&self, screen: &Screen, now: Instant) -> f32 {
        let fixed = self.fixed_zone(screen);
        let bars = if fixed > 0.0 { fixed + INSET } else { 0.0 };
        (GESTURE_INSET + bars).max(self.keyboard.height(now) + KEYBOARD_GAP)
    }

    /// One screen: its content, which scrolls from the top group's upper
    /// edge to the window's lower one, under the bars and the keyboard, and
    /// its bottom zone. Between tabs it slides sideways as a whole.
    fn layer(
        &self,
        screen: &Screen,
        layer: Layer,
        width: f32,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = match screen {
            Screen::Recents | Screen::Contacts if self.searched(cx) && layer.live() => {
                self.found_list(screen, layer, cx)
            }
            Screen::Recents => self.recents_list(layer, now, cx),
            Screen::Contacts => self.contacts_list(layer, now, cx),
            Screen::Keypad => div().into_any_element(),
            Screen::Person(who) => self.person(who, layer, now, cx),
            Screen::Call => self.call_screen(layer, now),
        };
        let bottom = match layer {
            Layer::Shown => self.arrival.bottom(now),
            Layer::Leaving => self.arrival.leaving_bottom(now),
        };
        let mut layer_div = div()
            .id(match layer {
                Layer::Shown => "shown",
                Layer::Leaving => "leaving",
            })
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(self.arrival.shift(layer, width, now)))
            .w_full()
            // Above the top group the gutter stays on the backing, which
            // Denial paints the status strip with.
            .child(
                div()
                    .absolute()
                    .top(px(INSET))
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .child(content),
            )
            .child(self.bottom_zone(screen, layer, bottom, now, cx));
        if layer.live() {
            layer_div = layer_div.on_click(cx.listener(|this, _, _, cx| this.dismiss(cx)));
        }
        layer_div.into_any_element()
    }

    /// What sits on the bars at the bottom, or on the gesture zone: the
    /// keypad and its Call, the in-call controls, the notice chip over them,
    /// and a confirmation in their place. Only text fields ride on the
    /// keyboard, and none is down here, so it stays under the keys.
    fn bottom_zone(
        &self,
        screen: &Screen,
        layer: Layer,
        motion: Motion,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> Div {
        let confirm = match &self.overlay {
            Some((Overlay::Confirm(confirm), lift)) if layer.live() => Some((confirm, *lift)),
            _ => None,
        };
        let content = match confirm {
            Some((confirm, lift)) => Some(self.dialog(confirm, lift, now, cx).into_any_element()),
            None => match screen {
                Screen::Keypad => Some(self.keypad(layer, cx).into_any_element()),
                Screen::Call => self.call_controls(layer, cx),
                _ => None,
            },
        };
        let notice = self
            .notice
            .as_ref()
            .filter(|_| layer.live())
            .map(|(note, fade, _)| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(gpui::relative(1.))
                    .pb(px(INSET))
                    .flex()
                    .justify_center()
                    .opacity(fade.progress(now))
                    .child(denial_look_gpui::chip(&self.look).child(note.clone()))
            });
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(-motion.offset))
            .px(px(INSET))
            .pb(px(self.zone_inset(screen, now)))
            .opacity(motion.opacity)
            .children(notice)
            .children(content)
    }

    /// The bars on the gesture zone: the tab bar on the tabs, and over it, or
    /// alone elsewhere, the bar of a call going on behind. Between tabs they
    /// stay; to and from other screens they drop away and rise in.
    fn bars(&self, now: Instant, cx: &mut Context<Self>) -> Div {
        let leaving = self
            .arrival
            .leaving
            .as_ref()
            .filter(|_| self.arrival.gone(now).is_some());
        let gone = self.arrival.gone(now).unwrap_or(1.0);
        let shown_tab = self.screen.tab();
        let leaving_tab = leaving.is_some_and(Screen::tab);
        // The tab bar, and how much of it is up: it slides up from under the
        // window's edge, stays, or slides back down. The call bar sits on
        // what is up of it.
        let (tabs, up) = match (shown_tab, leaving_tab) {
            (true, false) if leaving.is_some() => (
                Some((&self.screen, Layer::Shown)),
                self.arrival.bottom(now).opacity,
            ),
            (true, _) => (Some((&self.screen, Layer::Shown)), 1.0),
            (false, true) => (leaving.map(|screen| (screen, Layer::Leaving)), 1.0 - gone),
            (false, false) => (None, 0.0),
        };
        let height = GESTURE_INSET + dp_lp(TAB_BAR);
        let tab_bar = tabs.map(|(screen, layer)| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(-(1.0 - up) * height))
                .child(self.tab_bar(screen, layer, now, cx))
        });
        // The call bar, while a call goes on behind the shown screen or the
        // leaving one.
        let call_bar = self
            .live
            .as_ref()
            .filter(|live| !live.ended())
            .and_then(|_| {
                let motion = match (self.screen == Screen::Call, leaving == Some(&Screen::Call)) {
                    (false, true) => self.arrival.bottom(now),
                    (false, false) => STILL,
                    (true, false) if leaving.is_some() && self.arrival.call_bar => {
                        self.arrival.leaving_bottom(now)
                    }
                    (true, _) => return None,
                };
                let layer = if self.screen == Screen::Call {
                    Layer::Leaving
                } else {
                    Layer::Shown
                };
                let lift = up * (dp_lp(TAB_BAR) + INSET);
                Some(
                    div()
                        .absolute()
                        .left(px(INSET))
                        .right(px(INSET))
                        .bottom(px(GESTURE_INSET + lift - motion.offset))
                        .opacity(motion.opacity)
                        .children(self.call_bar(layer, now, cx)),
                )
            });
        div()
            .absolute()
            .inset_0()
            .children(tab_bar)
            .children(call_bar)
    }

    /// A call is going on behind `screen`, and its bar shows there.
    fn call_behind(&self, screen: &Screen) -> bool {
        *screen != Screen::Call && self.live.as_ref().is_some_and(|live| !live.ended())
    }

    /// The top of the window: on the tabs the search bar, where they have
    /// it; on other screens the top group. The
    /// search bar stays between Recents and Contacts and slides with its
    /// screen to and from the keypad; the rest cross-fades.
    fn top_area(&self, width: f32, now: Instant, cx: &mut Context<Self>) -> Div {
        let leaving = self
            .arrival
            .leaving
            .as_ref()
            .filter(|_| self.arrival.gone(now).is_some());
        let out = self.arrival.gone(now).map_or(1.0, |gone| {
            let out = self
                .arrival
                .clock
                .segment(0, motion::GROUP_OUT)
                .progress(now);
            1.0 - out.max(gone)
        });
        let shown_in = self.arrival.group(now);
        let row = || div().absolute().inset_0().flex().gap(px(INSET));
        let mut area = div()
            .absolute()
            .top(px(INSET))
            .left(px(INSET))
            .right(px(INSET))
            .h(dp(GROUP_HEIGHT));
        // The tabs' search bars.
        let tabs = [
            Some((&self.screen, Layer::Shown)),
            leaving.map(|screen| (screen, Layer::Leaving)),
        ];
        let tabs: Vec<_> = tabs
            .into_iter()
            .flatten()
            .filter(|(s, _)| s.tab())
            .collect();
        if !tabs.is_empty() {
            let opacity = match (self.screen.tab(), leaving.map(Screen::tab)) {
                (true, Some(true)) => 1.0,
                (true, _) => shown_in,
                (false, _) => out,
            };
            let both = tabs.len() == 2 && tabs.iter().all(|(s, _)| s.searchable());
            let mut bars = div().relative().flex_1().min_w_0().h_full();
            for &(screen, layer) in &tabs {
                if !screen.searchable() || (both && layer == Layer::Leaving) {
                    continue;
                }
                let shift = if both {
                    0.0
                } else {
                    self.arrival.shift(layer, width, now)
                };
                bars = bars.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(shift))
                        .w_full()
                        .flex()
                        .child(self.search_bar(layer, cx)),
                );
            }
            area = area.child(
                row()
                    .top(px(-self.top_away.away()))
                    .bottom(px(self.top_away.away()))
                    .opacity(opacity)
                    .child(bars)
                    ,
            );
        }
        // The top group of a screen under another.
        let grouped = [
            leaving
                .filter(|screen| !screen.tab())
                .map(|screen| (screen, Layer::Leaving)),
            (!self.screen.tab()).then_some((&self.screen, Layer::Shown)),
        ];
        let grouped: Vec<_> = grouped.into_iter().flatten().collect();
        if !grouped.is_empty() {
            let opacity = match grouped.as_slice() {
                [_, _] => 1.0,
                [(_, Layer::Shown)] => shown_in,
                _ => out,
            };
            let mut group = denial_look_gpui::glass_group(&self.look)
                .size_full()
                .relative()
                .opacity(opacity);
            for (screen, layer) in grouped {
                let contents = match (layer, leaving.is_some_and(|l| !l.tab()), self.screen.tab()) {
                    (Layer::Leaving, _, false) => out,
                    (Layer::Shown, true, _) => shown_in,
                    _ => 1.0,
                };
                group = group.child(
                    div()
                        .id(match layer {
                            Layer::Shown => "shown-top",
                            Layer::Leaving => "leaving-top",
                        })
                        .absolute()
                        .inset_0()
                        .opacity(contents)
                        .child(self.top_contents(screen, layer, cx)),
                );
            }
            area = area.child(group);
        }
        area
    }

    /// What the top group holds on a screen under another.
    fn top_contents(&self, screen: &Screen, layer: Layer, cx: &mut Context<Self>) -> Div {
        match screen {
            Screen::Person(who) => self.person_top(who, layer, cx),
            Screen::Call => self.titled_top(tr("Call"), layer, cx),
            Screen::Recents | Screen::Contacts | Screen::Keypad => self.top_row(),
        }
    }

    /// Takes what each list shows for this frame.
    fn take_items(&mut self, cx: &mut Context<Self>) {
        self.entries = self.contact_entries();
        self.found_items = if self.searched(cx) {
            self.find_items(cx)
        } else {
            Vec::new()
        };
        for (state, count) in [
            (&self.recents, self.log.len()),
            (&self.contacts, self.entries.len()),
            (&self.found, self.found_items.len()),
        ] {
            if state.item_count() != count {
                state.splice(0..state.item_count(), count);
            }
        }
    }
}

impl Render for Calls {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        if self.animating(now) {
            window.request_animation_frame();
        }
        self.take_items(cx);
        self.follow_scroll(window, cx);
        let width = f32::from(window.viewport_size().width);
        let leaving = self
            .arrival
            .leaving
            .clone()
            .filter(|_| self.arrival.gone(now).is_some())
            .map(|screen| self.layer(&screen, Layer::Leaving, width, now, cx));
        let shown = self.layer(&self.screen.clone(), Layer::Shown, width, now, cx);
        div()
            .size_full()
            .relative()
            .key_context("Calls")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::back))
            .on_key_down(cx.listener(Self::key_down))
            .bg(backing(&self.look))
            .font(self.font.clone())
            .text_color(solid(self.look.palette.foreground))
            .children(leaving)
            .child(shown)
            .child(self.bars(now, cx))
            .child(self.top_area(width, now, cx))
    }
}
