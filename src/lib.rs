// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A simple library for adjusting display backlight settings on Linux.
//!
//! This crate uses systemd and logind to set the backlight without requiring
//! root privileges. It will only work when run by a user who is currently
//! logged in at the seat that controls the display in question.

use logind_zbus::session::SessionProxyBlocking;
use std::{ffi::OsString, fs, path::Path, io};
use zbus::blocking::Connection;

/// A description of a backlight device found by this library.
pub struct Backlight {
    /// Name of the backlight. Despite being a "device name" this is not a name
    /// you'll find in `/dev`. It appears in two places:
    ///
    /// - As a directory under `/sys/class/backlight/`
    /// - As the name passed to `logind` to control the backlight.
    pub name: OsString,

    /// Highest raw value the backlight supports. This value always means "fully
    /// on," but different drivers use different units and scales.
    pub max: u32,
}

impl Backlight {
    /// Computes a percentage of this backlight's max.
    ///
    /// `pct` must be between 0 and 100, inclusive.
    pub fn from_percent(&self, pct: u32) -> u32 {
        (u64::from(pct) * u64::from(self.max) / 100) as u32
    }

    /// Converts a setting for this backlight into a percentage of max.
    ///
    /// `value` must be valid for this backlight.
    pub fn to_percent(&self, value: u32) -> u32 {
        (u64::from(value) * 100 / u64::from(self.max)) as u32
    }
}

/// Things that can go wrong when using this library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// We couldn't find any compatible backlights, so we can't adjust anything.
    #[error("no compatible backlights found on this system")]
    EternalDarkness,
    /// Errors accessing the backlight directory in sys.
    #[error("can't access /sys/class/backlight")]
    SysAccess(#[source] io::Error),
    /// Errors accessing a specific backlight (included by path).
    #[error("can't use backlight device {0}")]
    Access(String, #[source] io::Error),
    /// A backlight device produced non-numeric output, which is super weird.
    #[error("backlight device {0} produced non-numeric output: {1}")]
    Parsing(String, String, #[source] std::num::ParseIntError),

    /// Something happened in communication with logind.
    #[error("problem changing brightness over DBus")]
    Dbus(#[from] zbus::Error),
}

/// Locates the first suitable backlight device in `/sys/class/backlight`. Since
/// most systems have either zero or one backlight, this limited operation
/// covers a lot of use cases.
///
/// On success, returns both the `Backlight` and its current raw setting.
pub fn find_first_backlight() -> Result<(Backlight, u32), Error> {
    // The Session proxy in logind will happily let us set the backlight, if we
    // know the backlight's subsystem and name. It does not, however, provide us
    // with any way to actually _discover_ that information. And so we do it the
    // hard way.
    //
    // Fortunately the hard way is available to unprivileged users, and that's
    // presumably why logind didn't offer to proxy it for us.

    let dir = fs::read_dir("/sys/class/backlight").map_err(Error::SysAccess)?;

    for dirent in dir {
        let dirent = dirent.map_err(Error::SysAccess)?;
        let path = dirent.path();

        match read_backlight_settings(&path) {
            Ok((current, max)) => {
                // We'll take the first one we found.

                // This error case really shouldn't be possible since we built
                // the path by appending a name!
                let name = path.file_name().expect("file should have a name");

                return Ok((
                    Backlight {
                        name: name.to_owned(),
                        max,
                    },
                    current,
                ));
            }
            Err(e) => {
                eprintln!("skipping backlight-like device at {}: {e}", path.display());
            }
        }
    }

    Err(Error::EternalDarkness)
}

/// Finds a backlight given a user-specified name.
///
/// On success, returns both the `Backlight` and its current setting.
pub fn use_specific_backlight(name: OsString) -> Result<(Backlight, u32), Error> {
    let path = Path::new("/sys/class/backlight").join(&name);
    let (current, max) = read_backlight_settings(&path)?;

    Ok((Backlight { name, max }, current))
}

/// Sets the brightness of a `Backlight` given an existing connection to the
/// session. This is marginally more efficient than setting up a new connection
/// each time, if you want to change the backlight repeatedly or continuously.
///
/// If you want to change the backlight only once, the
/// `connect_and_set_brightness` operation is more convenient.
///
/// # Panics
///
/// If `new_value` is out of range for `backlight` (check it against
/// `backlight.max`).
pub fn set_brightness(
    session: &SessionProxyBlocking,
    backlight: &Backlight,
    new_value: u32,
) -> Result<(), Error> {
    let Some(name) = backlight.name.to_str() else {
        // This _really_ shouldn't be able to happen, so I've decided to model
        // it as a panic rather than an error case for now.
        panic!("backlight name not valid UTF-8?! name: {:?}", backlight.name);
    };

    Ok(session.set_brightness("backlight", name, new_value)?)
}

/// Connects to the session DBus and logind and changes the brightness of a
/// given `backlight`.
///
/// # Panics
///
/// If `new_value` is out of range for `backlight` (check it against
/// `backlight.max`).
pub fn connect_and_set_brightness(
    backlight: &Backlight,
    new_value: u32,
) -> Result<(), Error> {
    assert!(new_value <= backlight.max);

    // Set up our DBus connection to the current session (.../session/auto).
    // Note that this happens on the SYSTEM bus, _not_ the SESSION bus!
    // This confused me too.
    let conn = Connection::system()?;
    let session = SessionProxyBlocking::builder(&conn)
        .path("/org/freedesktop/login1/session/auto")?
        .build()?;

    set_brightness(&session, backlight, new_value)
}

/// Loads settings for a single backlight device given its fully-qualified
/// directory path. Returns: `(current_value, max_value)`.
fn read_backlight_settings(path: &Path) -> Result<(u32, u32), Error> {
    let mut parsed = vec![];
    for component in ["brightness", "max_brightness"] {
        let c_path = path.join(component);
        let contents = fs::read_to_string(&c_path)
            .map_err(|e| Error::Access(c_path.display().to_string(), e))?;
        let number = contents.trim().parse::<u32>()
            .map_err(|e| Error::Parsing(c_path.display().to_string(), contents.trim().to_string(), e))?;
        parsed.push(number);
    }
    Ok((parsed[0], parsed[1]))
}
