//! The wallpaper screen: where the shown wallpaper sits, in a preview of
//! the screen with its zoom and position on sliders; Denial's own wallpaper
//! and the pictures in the wallpaper folder, three to a row in the screen's shape, the one shown
//! ringed in the accent; under them, Wallhaven's, found as the shell's
//! picker finds them, with a field to search and More for the next page. A
//! tap shows a picture on every screen at once; one from Wallhaven is
//! downloaded into the wallpaper folder first, its share received on it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use denial_look::INSET;
use denial_look::words::fill;
use denial_look_gpui::{Icon, dp, icon, island, say, solid};
use gpui::{
    AnyElement, Context, Div, Focusable, ObjectFit, SharedString, Stateful, StyledImage, Task,
    Window, div, img, prelude::*, px,
};

use super::parts::{Fill, ICON};
use super::{Layer, Screen, Settings};
use crate::wallhaven::{self, Found};
use crate::wallpaper::{self, Choice, Listing, MAX_ZOOM, Placement, Target};

/// Pictures to a row, and the space between them, in dp.
const COLUMNS: usize = 3;
const GAP: f32 = 8.0;
const PAD: f32 = 10.0;

/// How long typing rests before Wallhaven is asked.
const TYPING: Duration = Duration::from_millis(700);
/// Thumbnails fetched at once.
const FETCHING: usize = 4;
/// The placement preview's width, in dp.
const PREVIEW_WIDTH: f32 = 150.0;

/// Wallhaven's wallpapers, as the screen shows them.
#[derive(Default)]
pub(in crate::settings) struct Online {
    /// The words the pages found were asked for; None before the first
    /// search.
    asked: Option<String>,
    found: Vec<Found>,
    /// Pages read so far, and whether more follow.
    pages: u32,
    more: bool,
    /// Why the last page did not come.
    failed: Option<String>,
    /// Each thumbnail, by the wallpaper's id, once fetched.
    thumbs: HashMap<String, PathBuf>,
    /// The wallpaper being downloaded, by id, and the share received.
    downloading: Option<(String, f32)>,
    /// Reading a page and its thumbnails.
    reading: Option<Task<()>>,
    /// Waits for typing to rest.
    typing: Option<Task<()>>,
}

/// Pictures a folder shows at first, and more with each More.
const FILES: usize = 30;

/// The folder a wallpaper's file is picked from.
#[derive(Default)]
pub(in crate::settings) struct Files {
    folder: PathBuf,
    listing: Listing,
    /// How many of the pictures are shown.
    shown: usize,
    /// Why the folder could not be read.
    failed: Option<String>,
    /// The picture being copied.
    taking: Option<PathBuf>,
    /// Reading the folder.
    reading: Option<Task<()>>,
    /// Making the shown pictures' thumbnails.
    thumbs: Option<Task<()>>,
}

/// `tiles` three to a row.
fn rows(tiles: Vec<Stateful<Div>>) -> Vec<Div> {
    let mut tiles = tiles.into_iter().peekable();
    let mut rows = Vec::new();
    while tiles.peek().is_some() {
        rows.push(
            div()
                .flex()
                .gap(dp(GAP))
                .children(tiles.by_ref().take(COLUMNS)),
        );
    }
    rows
}

impl Settings {
    /// The screen the wallpapers are for, in pixels, wide by high: what
    /// Wallhaven's are at least, and the placement preview's shape.
    fn wallpaper_screen(&self) -> (u32, u32) {
        match (&self.wallpapers.target, &self.monitor) {
            (Target::Output(_), Some(monitor)) => monitor.size,
            _ => self.window_pixels,
        }
    }

    /// Your pictures in one island, then Wallhaven's under its chip;
    /// `width` is the window's.
    pub(super) fn wallpapers_screen(
        &self,
        width: f32,
        layer: Layer,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let shown = &self.wallpapers;
        let live = layer.live();
        let mut parts = Vec::new();
        let monitor = matches!(shown.target, Target::Output(_));
        if !monitor {
            parts.push(self.chip_row("Screens"));
            parts.push(self.screens_island(live, cx));
        }
        if shown.current.is_some() {
            parts.push(self.chip_row("Placement"));
            parts.push(self.placement_island(live, cx));
            parts.push(self.chip_row("Your pictures"));
        }
        let file = self
            .row(
                "wallpaper.file",
                Some(Icon::Photos),
                "From a file",
                None,
                (true, true),
            )
            .child(self.chevron());
        parts.push(
            island(&self.look).child(self.live(file, live, cx, move |this, window, cx| {
                this.open(Screen::WallpaperFiles { monitor }, window, cx)
            })),
        );
        if !shown.choices.is_empty() {
            let tiles = shown.choices.iter().map(|choice| {
                let picked = shown.current.as_deref() == Some(choice.resource.as_str());
                let tile = self.wallpaper_tile(
                    choice.resource.clone(),
                    shown.thumbs.get(&choice.resource),
                    &choice.name,
                    picked,
                    width,
                );
                if live && !picked {
                    let choice = choice.clone();
                    self.tappable(tile, cx, move |this, _, cx| this.set_wallpaper(&choice, cx))
                } else {
                    tile
                }
            });
            parts.push(
                island(&self.look)
                    .p(dp(PAD))
                    .flex()
                    .flex_col()
                    .gap(dp(GAP))
                    .children(rows(tiles.collect())),
            );
        }
        parts.push(self.chip_row("Wallhaven"));
        parts.push(self.wallhaven_island(width, live, cx));
        parts
    }

    /// Whether the lock screen has its own wallpaper and, when it does, which
    /// of the two the rest of the screen changes.
    fn screens_island(&self, live: bool, cx: &mut Context<Self>) -> Div {
        let separate = self.wallpapers.lock.is_some();
        let row = self
            .row(
                "wallpaper.separate",
                Some(Icon::Lock),
                "Separate lock screen wallpaper",
                None,
                (true, !separate),
            )
            .child(self.toggle(separate));
        let mut island = island(&self.look).child(self.live(row, live, cx, move |this, _, cx| {
            this.separate_lock(!separate, cx)
        }));
        if separate {
            let picked = usize::from(self.wallpapers.target == Target::Lock);
            island = island.child(self.separator(0.)).child(self.options_row(
                "wallpaper.target",
                "Changing",
                None,
                &["Home", "Lock"],
                picked,
                live,
                (false, true),
                cx,
                Rc::new(|this: &mut Self, option, _, cx| {
                    let target = [Target::Home, Target::Lock][option].clone();
                    this.read_wallpapers(target, cx);
                    cx.notify();
                }),
            ));
        }
        island
    }

    /// Gives the lock screen its own wallpaper, or the home screen's again.
    fn separate_lock(&mut self, separate: bool, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let set = cx
                .background_spawn(async move { wallpaper::set_separate(separate) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Err(error) = set {
                    view.note(
                        fill(
                            "Could not set the wallpaper: {why}",
                            &[("why", &error.to_string())],
                        ),
                        cx,
                    );
                }
                let target = if separate { Target::Lock } else { Target::Home };
                view.read_wallpapers(target, cx);
                if !separate {
                    // The lock screen's depth follows the home picture.
                    let file = view
                        .wallpapers
                        .current
                        .as_deref()
                        .and_then(|current| current.strip_prefix("file:"))
                        .map(PathBuf::from);
                    if let (true, Some(file)) = (view.lock_has_depth(), file) {
                        wallpaper::make_depth(&file);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether the lock screen draws a wallpaper with depth: the user may
    /// have told it otherwise.
    fn lock_has_depth(&self) -> bool {
        self.denial.document.as_ref().is_none_or(|document| {
            crate::denial::at(document, "lockScreen.useSystemWallpaper") != false
        }) && wallpaper::depth_installed()
    }

    /// The folder shown, its folders, and its pictures to pick from.
    pub(super) fn files_screen(
        &self,
        width: f32,
        layer: Layer,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let files = &self.wallpapers.files;
        let live = layer.live() && files.taking.is_none();
        let mut parts = vec![self.chip_row(files.folder.display().to_string())];
        let mut folders: Vec<(String, SharedString, PathBuf)> = Vec::new();
        if let Some(parent) = files.folder.parent() {
            folders.push(("..".to_owned(), "Up one folder".into(), parent.to_owned()));
        }
        folders.extend(files.listing.folders.iter().map(|folder| {
            let name = folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            (folder.display().to_string(), name.into(), folder.clone())
        }));
        let count = folders.len();
        let mut list = island(&self.look);
        for (ix, (id, name, folder)) in folders.into_iter().enumerate() {
            let row = self
                .row(
                    SharedString::from(id),
                    None,
                    name,
                    None,
                    (ix == 0, ix + 1 == count),
                )
                .child(self.chevron());
            list = list
                .when(ix > 0, |list| list.child(self.separator(0.)))
                .child(self.live(row, live, cx, move |this, _, cx| {
                    this.read_files(Some(folder.clone()), cx)
                }));
        }
        if count > 0 {
            parts.push(list);
        }
        let tiles: Vec<Stateful<Div>> = files
            .listing
            .pictures
            .iter()
            .take(files.shown)
            .map(|file| {
                let resource = format!("file:{}", file.display());
                let name = wallpaper::name(&resource);
                let thumb = self.wallpapers.thumbs.get(&resource);
                let taking = files.taking.as_ref() == Some(file);
                let tile = self.wallpaper_tile(resource, thumb, &name, taking, width);
                let file = file.clone();
                self.live(tile, live, cx, move |this, window, cx| {
                    this.take_file(file.clone(), window, cx)
                })
            })
            .collect();
        let status = if let Some(why) = &files.failed {
            Some(self.note_line(why.clone()))
        } else if tiles.is_empty() && files.reading.is_some() {
            Some(self.note_line("Looking…"))
        } else if tiles.is_empty() {
            Some(self.note_line("Nothing found"))
        } else if files.shown < files.listing.pictures.len() {
            Some(self.buttons([self.button(
                "files-more",
                "More",
                Fill::Control,
                live,
                cx,
                |this, _, cx| {
                    this.wallpapers.files.shown += FILES;
                    this.read_file_thumbs(cx);
                    cx.notify();
                },
            )]))
        } else {
            None
        };
        parts.push(
            island(&self.look)
                .p(dp(PAD))
                .flex()
                .flex_col()
                .gap(dp(GAP))
                .children(rows(tiles))
                .children(status),
        );
        parts
    }

    /// Lists `folder`, or the one shown last again, then makes its pictures'
    /// thumbnails.
    pub(super) fn read_files(&mut self, folder: Option<PathBuf>, cx: &mut Context<Self>) {
        let files = &mut self.wallpapers.files;
        let known = !files.folder.as_os_str().is_empty();
        let folder = match folder {
            Some(folder) => folder,
            None if known => files.folder.clone(),
            None => wallpaper::pictures_folder(),
        };
        files.folder = folder.clone();
        files.listing = Listing::default();
        files.shown = FILES;
        files.failed = None;
        cx.notify();
        files.reading = Some(cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn(async move { wallpaper::list(&folder) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match listed {
                    Ok(listing) => view.wallpapers.files.listing = listing,
                    Err(error) => view.wallpapers.files.failed = Some(error.to_string()),
                }
                view.read_file_thumbs(cx);
                cx.notify();
            });
        }));
    }

    /// Makes the shown pictures' thumbnails one by one.
    fn read_file_thumbs(&mut self, cx: &mut Context<Self>) {
        let shown = &self.wallpapers;
        let wanted: Vec<PathBuf> = shown
            .files
            .listing
            .pictures
            .iter()
            .take(shown.files.shown)
            .filter(|file| {
                !shown
                    .thumbs
                    .contains_key(&format!("file:{}", file.display()))
            })
            .cloned()
            .collect();
        let making = cx.spawn(async move |this, cx| {
            for file in wanted {
                let resource = format!("file:{}", file.display());
                let thumb = cx
                    .background_spawn(async move { wallpaper::thumbnail(&file) })
                    .await;
                let taken = this.update(cx, |view, cx| {
                    if let Some(thumb) = thumb {
                        view.wallpapers.thumbs.insert(resource, thumb);
                        cx.notify();
                    }
                });
                if taken.is_err() {
                    return;
                }
            }
        });
        self.wallpapers.files.reading = None;
        self.wallpapers.files.thumbs = Some(making);
    }

    /// Copies `file` into the wallpaper folder, shows it, and goes back.
    fn take_file(&mut self, file: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let monitor = matches!(self.wallpapers.target, Target::Output(_));
        self.wallpapers.files.taking = Some(file.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let taken = cx
                .background_spawn(async move { wallpaper::import(&file) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.wallpapers.files.taking = None;
                match taken {
                    Ok(file) => {
                        let resource = format!("file:{}", file.display());
                        let choice = Choice {
                            name: wallpaper::name(&resource),
                            resource,
                            file: Some(file),
                        };
                        let target = view.wallpapers.target.clone();
                        view.set_wallpaper(&choice, cx);
                        // Listed afresh with it, the choice shown meanwhile.
                        view.wallpapers.reading = None;
                        if view.screen == (Screen::WallpaperFiles { monitor }) {
                            let back = Self::parent(&view.screen);
                            view.open_at(back, true, window, cx);
                        } else {
                            view.read_wallpapers(target, cx);
                        }
                        view.wallpapers.current = Some(choice.resource);
                        view.wallpapers.placement = Placement::default();
                        view.read_preview(cx);
                    }
                    Err(why) => view.note(
                        fill("Could not set the wallpaper: {why}", &[("why", &why)]),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The shown wallpaper in a small screen, placed as the sliders under
    /// it say, and Reset.
    fn placement_island(&self, live: bool, cx: &mut Context<Self>) -> Div {
        let shown = &self.wallpapers;
        let placement = shown.placement;
        let (screen_w, screen_h) = match self.wallpaper_screen() {
            (w, h) if w > 0 && h > 0 => (w as f32, h as f32),
            _ => (270.0, 585.0),
        };
        let width = f32::from(dp(PREVIEW_WIDTH));
        let height = width * screen_h / screen_w;
        let radius = px((self.look.group_radius() - f32::from(dp(PAD))).max(6.0));
        let picture = shown
            .preview
            .as_ref()
            .filter(|(resource, ..)| shown.current.as_ref() == Some(resource))
            .map(|(_, file, size)| {
                let [left, top, w, h] = placement.frame((width, height), *size);
                img(file.clone())
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(w))
                    .h(px(h))
                    .object_fit(ObjectFit::Fill)
            });
        let preview = div().flex().justify_center().py(dp(PAD)).child(
            div()
                .relative()
                .w(px(width))
                .h(px(height))
                .rounded(radius)
                .overflow_hidden()
                .bg(denial_look_gpui::control_fill(&self.look))
                .children(picture),
        );
        let along = |at: f32| format!("{:.0}%", (at + 1.0) * 50.0);
        let slide = |set: fn(&mut Placement, f32)| -> super::page::OnSlide {
            Rc::new(
                move |this: &mut Self, level, done, cx: &mut Context<Self>| {
                    set(&mut this.wallpapers.placement, level);
                    cx.notify();
                    if done {
                        this.place_wallpaper(cx);
                    }
                },
            )
        };
        let zoom = self.slider_row(
            "wallpaper.zoom",
            None,
            "Zoom",
            None,
            format!("{:.0}%", placement.zoom * 100.0).into(),
            (placement.zoom - 1.0) / (MAX_ZOOM - 1.0),
            live,
            (false, false),
            cx,
            slide(|placement, level| placement.zoom = 1.0 + level * (MAX_ZOOM - 1.0)),
        );
        let across = self.slider_row(
            "wallpaper.x",
            None,
            "Left and right",
            None,
            along(placement.x).into(),
            (placement.x + 1.0) / 2.0,
            live,
            (false, false),
            cx,
            slide(|placement, level| placement.x = level * 2.0 - 1.0),
        );
        let down = self.slider_row(
            "wallpaper.y",
            None,
            "Up and down",
            None,
            along(placement.y).into(),
            (placement.y + 1.0) / 2.0,
            live,
            (false, false),
            cx,
            slide(|placement, level| placement.y = level * 2.0 - 1.0),
        );
        let reset = self.button(
            "wallpaper-reset",
            "Reset",
            Fill::Control,
            live && placement != Placement::default(),
            cx,
            |this, _, cx| {
                this.wallpapers.placement = Placement::default();
                this.place_wallpaper(cx);
            },
        );
        island(&self.look)
            .child(preview)
            .child(zoom)
            .child(across)
            .child(down)
            .child(
                div()
                    .px(dp(super::parts::ROW_PAD))
                    .pb(dp(super::parts::ROW_PAD))
                    .child(self.buttons([reset])),
            )
    }

    /// Writes the placement the sliders have into the shell's file.
    fn place_wallpaper(&mut self, cx: &mut Context<Self>) {
        let placement = self.wallpapers.placement;
        let target = self.wallpapers.target.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let placed = cx
                .background_spawn(async move { wallpaper::place(placement, &target) })
                .await;
            if let Err(error) = placed {
                let _ = this.update(cx, |view, cx| {
                    view.wallpapers.placement = wallpaper::placement(&view.wallpapers.target);
                    view.note(
                        fill(
                            "Could not place the wallpaper: {why}",
                            &[("why", &error.to_string())],
                        ),
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    /// Makes the preview's picture of the shown wallpaper, once the
    /// pictures are listed.
    fn read_preview(&mut self, cx: &mut Context<Self>) {
        let Some(current) = self.wallpapers.current.clone() else {
            return;
        };
        if self
            .wallpapers
            .preview
            .as_ref()
            .is_some_and(|(resource, ..)| *resource == current)
        {
            return;
        }
        let file = self
            .wallpapers
            .choices
            .iter()
            .find(|choice| choice.resource == current)
            .and_then(|choice| choice.file.clone())
            .or_else(|| current.strip_prefix("file:").map(PathBuf::from));
        let Some(file) = file else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let preview = cx
                .background_spawn(async move { wallpaper::preview(&file) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if let Some((picture, size)) = preview
                    && view.wallpapers.current.as_ref() == Some(&current)
                {
                    view.wallpapers.preview = Some((current, picture, size));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// One picture in the screen's shape: its thumbnail, or its name until
    /// that comes; ringed in the accent when it is `picked`.
    fn wallpaper_tile(
        &self,
        id: String,
        thumb: Option<&PathBuf>,
        name: &str,
        picked: bool,
        width: f32,
    ) -> Stateful<Div> {
        let inner = width - 2.0 * INSET - 2.0 * f32::from(dp(PAD));
        let tile_width = (inner - (COLUMNS - 1) as f32 * f32::from(dp(GAP))) / COLUMNS as f32;
        let tile_height = tile_width * 585.0 / 270.0;
        let radius = px((self.look.group_radius() - f32::from(dp(PAD))).max(6.0));
        let picture: AnyElement = match thumb {
            Some(thumb) => img(thumb.clone())
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(solid(self.look.palette.secondary))
                .child(SharedString::from(name.to_owned()))
                .into_any_element(),
        };
        div()
            .id(SharedString::from(id))
            .relative()
            .flex_none()
            .w(px(tile_width))
            .h(px(tile_height))
            .rounded(radius)
            .overflow_hidden()
            .bg(denial_look_gpui::control_fill(&self.look))
            .when(picked, |tile| {
                tile.border(dp(3.)).border_color(solid(self.look.accent))
            })
            .child(picture)
    }

    /// Wallhaven's island: the field, what it found, and More, or why
    /// nothing came.
    fn wallhaven_island(&self, width: f32, live: bool, cx: &mut Context<Self>) -> Div {
        let online = &self.wallpapers.online;
        let folder = wallpaper::folder();
        let field = div()
            .id("wallhaven-field")
            .flex()
            .child(self.field(
                icon(Icon::Search, dp(ICON), solid(self.look.palette.secondary)),
                &self.wallhaven_field,
            ))
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.wallhaven_field.focus_handle(cx), cx);
            }));
        let tiles: Vec<Stateful<Div>> = online
            .found
            .iter()
            .map(|found| {
                let resource = folder
                    .as_ref()
                    .map(|folder| format!("file:{}", wallhaven::file_in(folder, found).display()));
                let picked = resource.is_some() && self.wallpapers.current == resource;
                let share = online
                    .downloading
                    .as_ref()
                    .filter(|(id, _)| *id == found.id)
                    .map(|(_, share)| *share);
                let tile = self
                    .wallpaper_tile(
                        format!("wallhaven:{}", found.id),
                        online.thumbs.get(&found.id),
                        "",
                        picked,
                        width,
                    )
                    .children(share.map(|share| {
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(denial_look_gpui::color(self.look.palette.canvas, 0.6))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("{:.0}%", share * 100.0))
                    }));
                if live && !picked && online.downloading.is_none() {
                    let found = found.clone();
                    self.tappable(tile, cx, move |this, window, cx| {
                        this.pick_wallhaven(found.clone(), window, cx)
                    })
                } else {
                    tile
                }
            })
            .collect();
        let reading = online.reading.is_some();
        let status: Option<Div> = match &online.failed {
            Some(why) => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(dp(GAP))
                    .child(self.note_line(why.clone()))
                    .child(self.buttons([self.button(
                        "wallhaven-again",
                        "Try again",
                        Fill::Control,
                        live && !reading,
                        cx,
                        |this, _, cx| this.read_wallhaven(false, cx),
                    )])),
            ),
            None if online.found.is_empty() && reading => Some(self.note_line("Looking…")),
            None if online.found.is_empty() && online.asked.is_some() => {
                Some(self.note_line("Nothing found"))
            }
            None if online.more => Some(self.buttons([self.button(
                "wallhaven-more",
                if reading { "Looking…" } else { "More" },
                Fill::Control,
                live && !reading,
                cx,
                |this, _, cx| this.read_wallhaven(false, cx),
            )])),
            None => None,
        };
        island(&self.look)
            .p(dp(PAD))
            .flex()
            .flex_col()
            .gap(dp(GAP))
            .child(field)
            .children(rows(tiles))
            .children(status)
    }

    /// A quiet line of text inside an island.
    fn note_line(&self, text: impl Into<SharedString>) -> Div {
        div()
            .py(dp(8.))
            .flex()
            .justify_center()
            .text_color(solid(self.look.palette.secondary))
            .child(say(text))
    }

    /// Lists the pictures, then makes their thumbnails one by one, showing
    /// each as it comes; Wallhaven's first page comes meanwhile.
    /// `target` is what they are for; Wallhaven is asked afresh when that
    /// moves between the phone and a monitor, for the screen's size.
    pub(super) fn read_wallpapers(&mut self, target: Target, cx: &mut Context<Self>) {
        self.wallpapers.lock = wallpaper::own_lock();
        let target = match target {
            Target::Lock if self.wallpapers.lock.is_none() => Target::Home,
            target => target,
        };
        let output = |target: &Target| match target {
            Target::Output(name) => Some(name.clone()),
            _ => None,
        };
        let moved = output(&self.wallpapers.target) != output(&target);
        self.wallpapers.current = wallpaper::current(&target);
        self.wallpapers.placement = wallpaper::placement(&target);
        self.wallpapers.target = target;
        self.read_preview(cx);
        let online = &self.wallpapers.online;
        if moved || online.reading.is_none() && (online.asked.is_none() || online.failed.is_some())
        {
            self.read_wallhaven(true, cx);
        }
        if self.wallpapers.reading.is_some() {
            return;
        }
        self.wallpapers.reading = Some(cx.spawn(async move |this, cx| {
            let choices = cx.background_spawn(async { wallpaper::choices() }).await;
            let wanted: Vec<Choice> = choices
                .iter()
                .filter(|choice| choice.file.is_some())
                .cloned()
                .collect();
            if this
                .update(cx, |view, cx| {
                    view.wallpapers.choices = choices;
                    view.read_preview(cx);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }
            for choice in wanted {
                let Some(file) = choice.file.clone() else {
                    continue;
                };
                let thumb = cx
                    .background_spawn(async move { wallpaper::thumbnail(&file) })
                    .await;
                let taken = this.update(cx, |view, cx| {
                    if let Some(thumb) = thumb {
                        view.wallpapers.thumbs.insert(choice.resource, thumb);
                        cx.notify();
                    }
                });
                if taken.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |view, _| view.wallpapers.reading = None);
        }));
    }

    /// Asks Wallhaven for what the field holds: afresh from the first page
    /// when `fresh` or when the words changed, else the next page. A search
    /// running is dropped for it.
    fn read_wallhaven(&mut self, fresh: bool, cx: &mut Context<Self>) {
        let text = self.wallhaven_field.read(cx).text().trim().to_owned();
        let online = &mut self.wallpapers.online;
        online.typing = None;
        if fresh || online.asked.as_deref() != Some(text.as_str()) {
            online.found.clear();
            online.pages = 0;
            online.more = false;
        }
        online.asked = Some(text.clone());
        online.failed = None;
        let page = online.pages + 1;
        let screen = self.wallpaper_screen();
        let online = &mut self.wallpapers.online;
        cx.notify();
        online.reading = Some(cx.spawn(async move |this, cx| {
            let asked = text.clone();
            let read = cx
                .background_spawn(async move { wallhaven::search(&asked, page, screen) })
                .await;
            let found = match read {
                Ok(read) => {
                    let found = read.found.clone();
                    let taken = this.update(cx, |view, cx| {
                        let online = &mut view.wallpapers.online;
                        online.pages = page;
                        online.more = read.more;
                        online.found.extend(read.found);
                        cx.notify();
                    });
                    if taken.is_err() {
                        return;
                    }
                    found
                }
                Err(why) => {
                    let _ = this.update(cx, |view, cx| {
                        let online = &mut view.wallpapers.online;
                        online.failed = Some(why);
                        online.reading = None;
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |view, cx| {
                view.wallpapers.online.reading = None;
                view.fetch_thumbs(found, cx);
            });
        }));
    }

    /// Fetches the thumbnails of `found` a few at a time, showing each as
    /// it comes. Detached: a new search does not drop the ones under way.
    fn fetch_thumbs(&mut self, found: Vec<Found>, cx: &mut Context<Self>) {
        let wanted: Vec<Found> = found
            .into_iter()
            .filter(|found| !self.wallpapers.online.thumbs.contains_key(&found.id))
            .collect();
        cx.spawn(async move |this, cx| {
            for batch in wanted.chunks(FETCHING) {
                let fetches: Vec<_> = batch
                    .iter()
                    .cloned()
                    .map(|found| {
                        cx.background_spawn(async move {
                            let thumb = wallhaven::thumbnail(&found);
                            (found.id, thumb)
                        })
                    })
                    .collect();
                for fetch in fetches {
                    let (id, thumb) = fetch.await;
                    let taken = this.update(cx, |view, cx| match thumb {
                        Ok(thumb) => {
                            view.wallpapers.online.thumbs.insert(id, thumb);
                            cx.notify();
                        }
                        Err(why) => log::warn!("settings: Wallhaven thumbnail {id}: {why}"),
                    });
                    if taken.is_err() {
                        return;
                    }
                }
            }
        })
        .detach();
    }

    /// The field changed: once typing rests, Wallhaven is asked for it.
    /// Each change starts the wait over.
    pub(super) fn typed_wallhaven(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        let text = self.wallhaven_field.read(cx).text().trim().to_owned();
        let online = &mut self.wallpapers.online;
        if online.asked.is_none() || online.asked.as_deref() == Some(text.as_str()) {
            online.typing = None;
            return;
        }
        online.typing = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TYPING).await;
            let _ = this.update(cx, |view, cx| {
                view.wallpapers.online.typing = None;
                let text = view.wallhaven_field.read(cx).text().trim().to_owned();
                if view.wallpapers.online.asked.as_deref() != Some(text.as_str()) {
                    view.read_wallhaven(true, cx);
                }
            });
        }));
    }

    /// Enter in the field: asks at once, and the keyboard goes.
    pub(super) fn submit_wallhaven(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.read_wallhaven(true, cx);
        window.focus(&self.focus_handle, cx);
    }

    /// Downloads `found` into the wallpaper folder, then shows it.
    fn pick_wallhaven(&mut self, found: Found, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = wallpaper::folder() else {
            return self.note("No wallpaper folder", cx);
        };
        window.focus(&self.focus_handle, cx);
        self.wallpapers.online.downloading = Some((found.id.clone(), 0.0));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (shares, heard) = async_channel::unbounded::<f32>();
            let download = cx.background_spawn(async move {
                wallhaven::download(&found, &folder, move |share| {
                    let _ = shares.try_send(share);
                })
            });
            let mut shown = 0.0;
            while let Ok(share) = heard.recv().await {
                if share - shown < 0.01 && share < 1.0 {
                    continue;
                }
                shown = share;
                let taken = this.update(cx, |view, cx| {
                    if let Some((_, known)) = &mut view.wallpapers.online.downloading {
                        *known = share;
                        cx.notify();
                    }
                });
                if taken.is_err() {
                    return;
                }
            }
            let file = download.await;
            let _ = this.update(cx, |view, cx| {
                view.wallpapers.online.downloading = None;
                match file {
                    Ok(file) => {
                        let resource = format!("file:{}", file.display());
                        let choice = Choice {
                            name: wallpaper::name(&resource),
                            resource,
                            file: Some(file),
                        };
                        view.set_wallpaper(&choice, cx);
                        // It is one of yours now: list the folder again.
                        view.wallpapers.reading = None;
                        let target = view.wallpapers.target.clone();
                        view.read_wallpapers(target, cx);
                    }
                    Err(why) => view.note(
                        fill("Could not download the wallpaper: {why}", &[("why", &why)]),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Shows `choice` on what the screen changes, and gives the lock screen
    /// its depth when it shows there.
    fn set_wallpaper(&mut self, choice: &Choice, cx: &mut Context<Self>) {
        let choice = choice.clone();
        let target = self.wallpapers.target.clone();
        // The lock screen draws its own wallpaper, else the home screen's.
        let on_lock = match target {
            Target::Home => self.wallpapers.lock.is_none(),
            Target::Lock => true,
            Target::Output(_) => false,
        };
        let depth = on_lock && self.lock_has_depth();
        if target == Target::Lock {
            self.wallpapers.lock = Some(choice.resource.clone());
        }
        self.wallpapers.current = Some(choice.resource.clone());
        self.wallpapers.placement = Placement::default();
        self.read_preview(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let set = cx
                .background_spawn(async move {
                    wallpaper::set(&choice, &target)?;
                    if let (true, Some(file)) = (depth, choice.file.as_ref()) {
                        wallpaper::make_depth(file);
                    }
                    Ok::<_, std::io::Error>(())
                })
                .await;
            let _ = this.update(cx, |view, cx| match set {
                Ok(()) => view.note("Wallpaper set", cx),
                Err(error) => {
                    view.wallpapers.current = wallpaper::current(&view.wallpapers.target);
                    view.note(
                        fill(
                            "Could not set the wallpaper: {why}",
                            &[("why", &error.to_string())],
                        ),
                        cx,
                    );
                }
            });
        })
        .detach();
    }
}
