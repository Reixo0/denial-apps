//! What Calls keeps on the phone across restarts, in
//! `$XDG_STATE_HOME/denial/calls/`: the favourite contacts in `favourites`,
//! by `People::key`, as strings each ended by a NUL, and the calls in
//! `log`, each as number, direction, time and seconds talked.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::data::{Call, Direction};

/// The file `name` in Calls' state folder.
fn path(name: &str) -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .map(|dir| dir.join("denial/calls").join(name))
}

/// Saves `fields` at `path`, each ended by a NUL.
fn write<'a>(path: Option<&PathBuf>, fields: impl Iterator<Item = &'a str>) {
    let Some(path) = path else {
        return;
    };
    let data: String = fields.flat_map(|field| [field, "\0"]).collect();
    // Written beside and renamed over, so a crash never leaves half a file.
    let temporary = path.with_extension("new");
    let saved = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&temporary, data))
        .and_then(|()| std::fs::rename(&temporary, path));
    if let Err(error) = saved {
        eprintln!("calls: saving {}: {error}", path.display());
    }
}

/// The favourite contacts, by key.
pub struct Favourites {
    path: Option<PathBuf>,
    keys: BTreeSet<String>,
}

impl Favourites {
    /// The saved favourites.
    pub fn load() -> Self {
        let path = path("favourites");
        let saved = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok());
        let keys = saved
            .iter()
            .flat_map(|saved| saved.split_terminator('\0'))
            .map(str::to_owned)
            .collect();
        Self { path, keys }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// Makes `key` a favourite or not, and saves when that changed anything.
    pub fn set(&mut self, key: &str, favourite: bool) {
        let changed = if favourite {
            self.keys.insert(key.to_owned())
        } else {
            self.keys.remove(key)
        };
        if changed {
            write(self.path.as_ref(), self.keys.iter().map(String::as_str));
        }
    }
}

/// The calls, kept on the phone.
pub struct CallLog {
    path: Option<PathBuf>,
}

impl CallLog {
    pub fn new() -> Self {
        Self { path: path("log") }
    }

    /// The saved calls, newest first, numbered from `first_id`.
    pub fn load(&self, first_id: u64) -> Vec<Call> {
        let saved = self
            .path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let fields: Vec<&str> = saved.split_terminator('\0').collect();
        fields
            .chunks_exact(4)
            .enumerate()
            .filter_map(|(index, call)| {
                Some(Call {
                    id: first_id + index as u64,
                    number: call[0].to_owned(),
                    direction: match call[1] {
                        "out" => Direction::Outgoing,
                        "in" => Direction::Incoming,
                        "missed" => Direction::Missed,
                        "declined" => Direction::Declined,
                        _ => return None,
                    },
                    time: call[2].parse().ok()?,
                    talked: call[3].parse().ok()?,
                })
            })
            .collect()
    }

    pub fn save(&self, log: &[Call]) {
        let fields: Vec<String> = log
            .iter()
            .flat_map(|call| {
                let direction = match call.direction {
                    Direction::Outgoing => "out",
                    Direction::Incoming => "in",
                    Direction::Missed => "missed",
                    Direction::Declined => "declined",
                };
                [
                    call.number.clone(),
                    direction.to_owned(),
                    call.time.to_string(),
                    call.talked.to_string(),
                ]
            })
            .collect();
        write(self.path.as_ref(), fields.iter().map(String::as_str));
    }
}
