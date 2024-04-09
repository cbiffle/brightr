// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A simple library for adjusting display backlight settings on Linux.
//!
//! This crate uses systemd and logind to set the backlight without requiring
//! root privileges. It will only work when run by a user who is currently
//! logged in at the seat that controls the display in question.

use anyhow::{bail, Context};
use logind_zbus::session::SessionProxyBlocking;
use std::{ffi::OsString, fs, path::Path};
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

/// Locates the first suitable backlight device in `/sys/class/backlight`. Since
/// most systems have either zero or one backlight, this limited operation
/// covers a lot of use cases.
///
/// On success, returns both the `Backlight` and its current raw setting.
pub fn find_first_backlight() -> anyhow::Result<(Backlight, u32)> {
    // The Session proxy in logind will happily let us set the backlight, if we
    // know the backlight's subsystem and name. It does not, however, provide us
    // with any way to actually _discover_ that information. And so we do it the
    // hard way.
    //
    // Fortunately the hard way is available to unprivileged users, and that's
    // presumably why logind didn't offer to proxy it for us.

    let dir = fs::read_dir("/sys/class/backlight")
        .context("can't access directory /sys/class/backlight")?;

    for dirent in dir {
        let dirent = dirent?;
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

    bail!("cannot find any valid backlight devices in /sys/class/backlight")
}

/// Finds a backlight given a user-specified name.
///
/// On success, returns both the `Backlight` and its current setting.
pub fn use_specific_backlight(name: OsString) -> anyhow::Result<(Backlight, u32)> {
    let path = Path::new("/sys/class/backlight").join(&name);
    let (current, max) = read_backlight_settings(&path)
        .with_context(|| format!("can't use explicitly requested backlight device {name:?}"))?;

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
) -> anyhow::Result<()> {
    let Some(name) = backlight.name.to_str() else {
        // This _really_ shouldn't be able to happen, but.
        bail!("backlight name not valid UTF-8?! name: {:?}", backlight.name);
    };

    session
        .set_brightness("backlight", name, new_value)
        .with_context(|| format!("can't set backlight {name}"))
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
) -> anyhow::Result<()> {
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
fn read_backlight_settings(path: &Path) -> anyhow::Result<(u32, u32)> {
    let mut parsed = vec![];
    for component in ["brightness", "max_brightness"] {
        let c_path = path.join(component);
        let contents = fs::read_to_string(&c_path)
            .with_context(|| format!("reading backlight file {}", c_path.display()))?;
        let number = contents.trim().parse::<u32>().with_context(|| {
            format!(
                "parsing brightness value from file {}: {contents}",
                c_path.display()
            )
        })?;
        parsed.push(number);
    }
    Ok((parsed[0], parsed[1]))
}
