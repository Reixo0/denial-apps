//! The wallpaper, as Denial's shell keeps it: `$XDG_STATE_HOME/denial/wallpaper`,
//! JSON naming the picture for every screen, which the shell watches and
//! shows as soon as it changes. The pictures offered are Denial's own and
//! the ones in the wallpaper folder, as the shell's picker offers them.
//!
//! The lock screen sets its clock behind what stands in front, from a depth
//! mask of the wallpaper that `wallpaper-depth` makes; when it is installed,
//! a new wallpaper gets its mask here, as the shell's picker does.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use image::imageops::FilterType;
use serde_json::{Value, json};

/// Denial's own wallpaper, as the shell names it.
pub const DENIAL: &str =
    "asset:packages/denial_flutter_sdk/assets/wallpapers/trails-h-accent-9-dark.png";

/// Thumbnails, in pixels: a phone screen's shape.
const THUMB_WIDTH: u32 = 270;
const THUMB_HEIGHT: u32 = 585;
/// The placement preview's picture, uncropped: its longer side, in pixels.
const PREVIEW_SIDE: u32 = 900;

/// The largest zoom the shell takes.
pub const MAX_ZOOM: f32 = 4.0;

/// Where the wallpaper sits on the screen, as the shell draws it: scaled to
/// cover the screen, enlarged `zoom` times about the point that `x` and `y`
/// name, each from the leading edge at -1 to the trailing edge at 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

impl Default for Placement {
    fn default() -> Self {
        Placement {
            x: 0.0,
            y: 0.0,
            zoom: 1.0,
        }
    }
}

impl Placement {
    /// The picture's place in a `width` by `height` screen when it is
    /// `picture` pixels: its left and top edges and its size.
    pub fn frame(&self, (width, height): (f32, f32), picture: (u32, u32)) -> [f32; 4] {
        let (w, h) = (picture.0.max(1) as f32, picture.1.max(1) as f32);
        let scale = f32::max(width / w, height / h) * self.zoom;
        let (shown_w, shown_h) = (w * scale, h * scale);
        [
            (width - shown_w) * (self.x + 1.0) / 2.0,
            (height - shown_h) * (self.y + 1.0) / 2.0,
            shown_w,
            shown_h,
        ]
    }
}

/// What a wallpaper is for.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Target {
    /// The phone's home screen, and its lock screen unless that has its own.
    #[default]
    Home,
    /// The lock screen's own.
    Lock,
    /// The monitor of that name.
    Output(String),
}

/// A wallpaper to pick: how the shell names it, and a picture of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub resource: String,
    pub name: String,
    /// The picture itself, or None for Denial's own when its file is not
    /// found.
    pub file: Option<PathBuf>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn state_file() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".local/state")))
        .map(|dir| dir.join("denial/wallpaper"))
}

fn cache() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".cache")))
        .map(|dir| dir.join("denial/settings/wallpapers"))
}

fn read_state() -> Option<Value> {
    serde_json::from_slice(&std::fs::read(state_file()?).ok()?).ok()
}

/// The wallpaper `target` shows, as the shell names it.
pub fn current(target: &Target) -> Option<String> {
    let state = read_state()?;
    let own = match target {
        Target::Home => None,
        Target::Lock => state["lock"]["resource"].as_str(),
        Target::Output(name) => state["outputs"][name].as_str(),
    };
    own.or_else(|| state["all"].as_str()).map(str::to_owned)
}

/// The lock screen's own wallpaper; None while it shows the home screen's.
pub fn own_lock() -> Option<String> {
    read_state()?["lock"]["resource"]
        .as_str()
        .map(str::to_owned)
}

/// Where the wallpaper sits on `target`, as the shell's file says.
pub fn placement(target: &Target) -> Placement {
    read_state().map_or_else(Placement::default, |state| placement_in(&state, target))
}

fn placement_in(state: &Value, target: &Target) -> Placement {
    let number = |value: &Value| value.as_f64().filter(|n| n.is_finite()).map(|n| n as f32);
    if *target == Target::Lock && state["lock"].is_object() {
        let lock = &state["lock"];
        return Placement {
            x: number(&lock["alignmentX"]).unwrap_or(0.0).clamp(-1.0, 1.0),
            y: number(&lock["alignmentY"]).unwrap_or(0.0).clamp(-1.0, 1.0),
            zoom: number(&lock["zoom"]).unwrap_or(1.0).clamp(1.0, MAX_ZOOM),
        };
    }
    let own = |key: &str| match target {
        Target::Output(name) => &state[key][name],
        _ => &Value::Null,
    };
    Placement {
        x: number(&own("outputAlignments")["x"])
            .or_else(|| number(&state["alignmentX"]))
            .unwrap_or(0.0)
            .clamp(-1.0, 1.0),
        y: number(&own("outputAlignments")["y"])
            .or_else(|| number(&state["alignmentY"]))
            .unwrap_or(0.0)
            .clamp(-1.0, 1.0),
        zoom: number(own("outputZooms"))
            .or_else(|| number(&state["zoom"]))
            .unwrap_or(1.0)
            .clamp(1.0, MAX_ZOOM),
    }
}

/// What a resource is called in a row: its file's name, or Denial.
pub fn name(resource: &str) -> String {
    match resource.strip_prefix("file:") {
        Some(path) => Path::new(path).file_stem().map_or_else(
            || path.to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        ),
        None => "Denial".to_owned(),
    }
}

/// The wallpaper folder, as the shell finds it: `$DENIAL_WALLPAPER_DIR`,
/// else `~/Pictures/Wallpapers`. Downloaded wallpapers go there too.
pub fn folder() -> Option<PathBuf> {
    std::env::var_os("DENIAL_WALLPAPER_DIR")
        .filter(|folder| !folder.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join("Pictures/Wallpapers")))
}

/// Whether `path` is named like a picture the shell can show.
fn is_picture(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp"
            )
        })
}

/// What a folder holds for picking a file: its folders by name, and its
/// pictures, the newest first.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub folders: Vec<PathBuf>,
    pub pictures: Vec<PathBuf>,
}

/// Where picking a file starts: the pictures folder, else home.
pub fn pictures_folder() -> PathBuf {
    let home = home().unwrap_or_else(|| PathBuf::from("/"));
    let pictures = home.join("Pictures");
    if pictures.is_dir() { pictures } else { home }
}

/// The folders and pictures in `folder`, hidden ones left out.
pub fn list(folder: &Path) -> std::io::Result<Listing> {
    let mut listing = Listing::default();
    let mut pictures = Vec::new();
    for entry in std::fs::read_dir(folder)?.flatten() {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        // Through links, as the files are read.
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            listing.folders.push(path);
        } else if metadata.is_file() && is_picture(&path) {
            pictures.push((metadata.modified().ok(), path));
        }
    }
    listing
        .folders
        .sort_by_key(|path| path.to_string_lossy().to_lowercase());
    pictures.sort_by(|a, b| b.cmp(a));
    listing.pictures = pictures.into_iter().map(|(_, path)| path).collect();
    Ok(listing)
}

/// Copies `file` into the wallpaper folder, turned upright, so it stays when
/// the original goes. A picture already there is taken as it is.
pub fn import(file: &Path) -> Result<PathBuf, String> {
    use image::metadata::Orientation;

    let why = |error: std::io::Error| error.to_string();
    let folder = folder().ok_or("no wallpaper folder")?;
    let (picture, orientation) = decode_as_kept(file).ok_or("the picture could not be read")?;
    if file.parent() == Some(folder.as_path()) {
        return Ok(file.to_owned());
    }
    std::fs::create_dir_all(&folder).map_err(why)?;
    let turned = orientation != Orientation::NoTransforms;
    let stem = file
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = if turned {
        "jpg".to_owned()
    } else {
        file.extension()
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let bytes = if turned {
        let mut picture = picture;
        picture.apply_orientation(orientation);
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95)
            .encode_image(&picture.into_rgb8())
            .map_err(|error| error.to_string())?;
        bytes
    } else {
        std::fs::read(file).map_err(why)?
    };
    let mut copy = 1;
    let to = loop {
        let name = match copy {
            1 => format!("{stem}.{extension}"),
            copy => format!("{stem}-{copy}.{extension}"),
        };
        let to = folder.join(name);
        match std::fs::read(&to) {
            // Picked before.
            Ok(there) if there == bytes => return Ok(to),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break to,
            _ => copy += 1,
        }
    };
    let temporary = folder.join(format!(".{stem}.new"));
    let written = std::fs::File::create(&temporary).and_then(|mut out| {
        std::io::Write::write_all(&mut out, &bytes)?;
        out.sync_all()
    });
    if let Err(error) = written.and_then(|()| std::fs::rename(&temporary, &to)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(why(error));
    }
    Ok(to)
}

/// Denial's own wallpaper and the pictures in the wallpaper folder.
pub fn choices() -> Vec<Choice> {
    let mut files: Vec<PathBuf> = folder()
        .and_then(|folder| std::fs::read_dir(folder).ok())
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_picture(path))
        .collect();
    files.sort();
    let denial = Choice {
        resource: DENIAL.to_owned(),
        name: "Denial".to_owned(),
        file: denial_file(),
    };
    std::iter::once(denial)
        .chain(files.into_iter().map(|file| {
            let resource = format!("file:{}", file.display());
            Choice {
                name: name(&resource),
                resource,
                file: Some(file),
            }
        }))
        .collect()
}

/// Denial's own wallpaper in the shell's bundle, beside the compositor that
/// started Settings, or where packages put it.
fn denial_file() -> Option<PathBuf> {
    let asset = DENIAL.strip_prefix("asset:")?;
    let beside = std::env::var_os("DENIAL_COMPOSITOR_BINARY")
        .map(PathBuf::from)
        .and_then(|binary| Some(binary.parent()?.parent()?.join("shell")));
    beside
        .into_iter()
        .chain([
            PathBuf::from("/usr/lib/denial/shell"),
            PathBuf::from("/usr/share/denial/shell"),
        ])
        .map(|bundle| bundle.join("data/flutter_assets").join(asset))
        .find(|file| file.is_file())
}

/// Makes `target` show `choice`, placed afresh: the shell's file is
/// rewritten beside itself and renamed over, keeping its version and
/// dimming. The home screen's picture goes on every screen without one of
/// its own; monitors keep theirs, and so does a lock screen with its own.
pub fn set(choice: &Choice, target: &Target) -> std::io::Result<()> {
    let path = state_file().ok_or(std::io::ErrorKind::NotFound)?;
    let mut state = read_file(&path).unwrap_or_else(|| json!({"version": 5, "all": DENIAL}));
    match target {
        Target::Output(name) => {
            entry(&mut state, "outputs", name, json!(choice.resource));
            write_own_placement(&mut state, name, Placement::default());
        }
        Target::Lock => write_lock(&mut state, &choice.resource, Placement::default()),
        Target::Home => {
            state["all"] = json!(choice.resource);
            forget_built_in(&mut state, "outputs");
            write_placement(&mut state, Placement::default());
        }
    }
    write_state(&path, &state)
}

/// Gives the lock screen its own wallpaper, at first the home screen's as it
/// sits, or makes it follow the home screen again.
pub fn set_separate(separate: bool) -> std::io::Result<()> {
    let path = state_file().ok_or(std::io::ErrorKind::NotFound)?;
    let mut state = read_file(&path).unwrap_or_else(|| json!({"version": 5, "all": DENIAL}));
    separate_in(&mut state, separate);
    write_state(&path, &state)
}

fn separate_in(state: &mut Value, separate: bool) {
    if separate {
        let resource = state["all"].as_str().unwrap_or(DENIAL).to_owned();
        let placement = placement_in(state, &Target::Home);
        write_lock(state, &resource, placement);
    } else if let Some(map) = state.as_object_mut() {
        map.remove("lock");
    }
}

fn write_lock(state: &mut Value, resource: &str, placement: Placement) {
    state["lock"] = json!({
        "resource": resource,
        "alignmentX": placement.x,
        "alignmentY": placement.y,
        "zoom": placement.zoom,
    });
}

/// Places the wallpaper on `target`.
pub fn place(placement: Placement, target: &Target) -> std::io::Result<()> {
    let path = state_file().ok_or(std::io::ErrorKind::NotFound)?;
    let mut state = read_file(&path).unwrap_or_else(|| json!({"version": 5, "all": DENIAL}));
    match target {
        Target::Output(name) => write_own_placement(&mut state, name, placement),
        Target::Lock => {
            let resource = state["lock"]["resource"]
                .as_str()
                .or_else(|| state["all"].as_str())
                .unwrap_or(DENIAL)
                .to_owned();
            write_lock(&mut state, &resource, placement);
        }
        Target::Home => write_placement(&mut state, placement),
    }
    write_state(&path, &state)
}

fn read_file(path: &Path) -> Option<Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
}

/// Sets `name`'s entry in the map at `key`, making the map when missing.
fn entry(state: &mut Value, key: &str, name: &str, value: Value) {
    if !state[key].is_object() {
        state[key] = json!({});
    }
    state[key][name] = value;
}

/// Drops the built-in screens' entries from the map at `key`: the phone's
/// own picture and placement follow the phone's settings.
fn forget_built_in(state: &mut Value, key: &str) {
    if let Some(map) = state[key].as_object_mut() {
        map.retain(|name, _| !crate::denial::internal(name));
    }
}

fn write_placement(state: &mut Value, placement: Placement) {
    // The anchors older shells read, as the shell names them.
    let anchor = |at: f32, names: [&'static str; 3]| match at {
        at if at < -0.5 => names[0],
        at if at > 0.5 => names[2],
        _ => names[1],
    };
    state["alignmentX"] = json!(placement.x);
    state["alignmentY"] = json!(placement.y);
    state["horizontalAlignment"] = json!(anchor(placement.x, ["left", "center", "right"]));
    state["verticalAlignment"] = json!(anchor(placement.y, ["top", "center", "bottom"]));
    state["zoom"] = json!(placement.zoom);
    forget_built_in(state, "outputAlignments");
    forget_built_in(state, "outputZooms");
}

/// A monitor's own placement, apart from the phone's.
fn write_own_placement(state: &mut Value, name: &str, placement: Placement) {
    entry(
        state,
        "outputAlignments",
        name,
        json!({"x": placement.x, "y": placement.y}),
    );
    entry(state, "outputZooms", name, json!(placement.zoom));
}

/// Writes the shell's file beside itself and renames it over.
fn write_state(path: &Path, state: &Value) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec(state)?)?;
    std::fs::rename(&temporary, path)
}

/// Starts the lock screen's depth mask of `file`, when `wallpaper-depth` is
/// installed, at the lowest priority; it runs on after Settings closes.
pub fn make_depth(file: &Path) {
    let started = Command::new("nice")
        .args(["-n", "19", "wallpaper-depth", "mask"])
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match started {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(error) => eprintln!("settings: starting wallpaper-depth: {error}"),
    }
}

/// Whether `wallpaper-depth` is installed, so the lock screen has depth.
pub fn depth_installed() -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|folder| folder.join("wallpaper-depth").is_file())
    })
}

/// Where the cache keeps `kind` of picture of `file`, under the file's
/// name, size and time, so a changed file gets a new one.
fn cached(file: &Path, kind: &str) -> Option<PathBuf> {
    let metadata = std::fs::metadata(file).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (file, metadata.len(), metadata.modified().ok()).hash(&mut hasher);
    Some(cache()?.join(format!("{:016x}{kind}.jpg", hasher.finish())))
}

/// `file`'s picture, upright.
fn decode(file: &Path) -> Option<image::DynamicImage> {
    let (mut picture, orientation) = decode_as_kept(file)?;
    picture.apply_orientation(orientation);
    Some(picture)
}

/// `file`'s picture as its pixels lie, and the turn that puts it upright,
/// which cameras write beside them.
fn decode_as_kept(file: &Path) -> Option<(image::DynamicImage, image::metadata::Orientation)> {
    use image::ImageDecoder;

    let read = || -> image::ImageResult<_> {
        let mut decoder = image::ImageReader::open(file)?
            .with_guessed_format()?
            .into_decoder()?;
        let orientation = decoder.orientation()?;
        Ok((image::DynamicImage::from_decoder(decoder)?, orientation))
    };
    read()
        .map_err(|error| eprintln!("settings: reading {}: {error}", file.display()))
        .ok()
}

/// Saves `picture` as `to`, through a file beside it.
fn save(picture: &image::RgbImage, to: &Path) -> Option<()> {
    std::fs::create_dir_all(to.parent()?)
        .map_err(|error| eprintln!("settings: making {}: {error}", to.display()))
        .ok()?;
    let temporary = to.with_extension("new");
    picture
        .save_with_format(&temporary, image::ImageFormat::Jpeg)
        .ok()?;
    std::fs::rename(&temporary, to).ok()
}

/// `file` whole and smaller, for the placement preview, with its size:
/// made once and kept in the cache.
pub fn preview(file: &Path) -> Option<(PathBuf, (u32, u32))> {
    let preview = cached(file, "-whole")?;
    if preview.is_file() {
        return Some((preview.clone(), image::image_dimensions(&preview).ok()?));
    }
    let picture = decode(file)?
        .thumbnail(PREVIEW_SIDE, PREVIEW_SIDE)
        .into_rgb8();
    save(&picture, &preview)?;
    Some((preview, picture.dimensions()))
}

/// A small picture of `file` in the screen's shape, made once and kept in
/// the cache.
pub fn thumbnail(file: &Path) -> Option<PathBuf> {
    let thumb = cached(file, "")?;
    if thumb.is_file() {
        return Some(thumb);
    }
    let picture = decode(file)?;
    // Shrinks fast to twice the size first: wallpapers run to 40 megapixels.
    let cover = f64::max(
        f64::from(THUMB_WIDTH * 2) / f64::from(picture.width().max(1)),
        f64::from(THUMB_HEIGHT * 2) / f64::from(picture.height().max(1)),
    )
    .min(1.0);
    let picture = picture
        .thumbnail(
            (f64::from(picture.width()) * cover).ceil() as u32,
            (f64::from(picture.height()) * cover).ceil() as u32,
        )
        .resize_to_fill(THUMB_WIDTH, THUMB_HEIGHT, FilterType::Triangle)
        .into_rgb8();
    save(&picture, &thumb)?;
    Some(thumb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_covers_then_zooms() {
        // A square on a 100 by 200 screen covers it 200 wide.
        let centred = Placement::default();
        assert_eq!(
            centred.frame((100.0, 200.0), (50, 50)),
            [-50.0, 0.0, 200.0, 200.0]
        );
        let left = Placement { x: -1.0, ..centred };
        assert_eq!(
            left.frame((100.0, 200.0), (50, 50)),
            [0.0, 0.0, 200.0, 200.0]
        );
        let zoomed = Placement {
            zoom: 2.0,
            y: 1.0,
            ..centred
        };
        assert_eq!(
            zoomed.frame((100.0, 200.0), (50, 50)),
            [-150.0, -200.0, 400.0, 400.0]
        );
    }

    #[test]
    fn the_phone_keeps_the_monitors_own() {
        let mut state = json!({
            "all": "a",
            "outputs": {"DSI-1": "b", "DP-1": "c"},
            "outputAlignments": {"DSI-1": {"x": 1, "y": 0}, "DP-1": {"x": -1, "y": 0}},
        });
        forget_built_in(&mut state, "outputs");
        write_placement(&mut state, Placement::default());
        assert_eq!(state["outputs"], json!({"DP-1": "c"}));
        assert_eq!(
            state["outputAlignments"],
            json!({"DP-1": {"x": -1, "y": 0}})
        );
        write_own_placement(
            &mut state,
            "DP-1",
            Placement {
                x: 0.5,
                y: 0.0,
                zoom: 2.0,
            },
        );
        assert_eq!(state["outputZooms"]["DP-1"], json!(2.0));
        assert_eq!(state["alignmentX"], json!(0.0));
    }

    #[test]
    fn the_lock_screen_starts_as_the_home_screen() {
        let mut state = json!({"all": "file:/a.png", "alignmentX": 0.5, "zoom": 2.0});
        separate_in(&mut state, true);
        assert_eq!(
            state["lock"],
            json!({"resource": "file:/a.png", "alignmentX": 0.5, "alignmentY": 0.0, "zoom": 2.0})
        );
        write_placement(&mut state, Placement::default());
        assert_eq!(placement_in(&state, &Target::Lock).x, 0.5);
        assert_eq!(placement_in(&state, &Target::Home).x, 0.0);
        separate_in(&mut state, false);
        assert!(state.get("lock").is_none());
        assert_eq!(placement_in(&state, &Target::Lock).x, 0.0);
    }

    #[test]
    fn names_come_from_files() {
        assert_eq!(name("file:/home/a/Pictures/Wallpapers/dunes.jpg"), "dunes");
        assert_eq!(name(DENIAL), "Denial");
    }
}
