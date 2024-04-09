// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A simple program for adjusting display backlight settings on Linux.
//!
//! This program uses systemd and logind to set the backlight without requiring
//! root privileges. It will only work when run by a user who is currently
//! logged in at the seat that controls the display in question.

use anyhow::{Context, bail};
use clap::Parser;
use logind_zbus::session::SessionProxyBlocking;
use std::{fs, path::Path, ffi::OsString};
use zbus::blocking::Connection;

/// Adjust display backlight. All values are in percentages unless overridden
/// using -r/--raw.
#[derive(Parser)]
struct Brightr {
    /// Name of backlight device to adjust. Use this to override the automatic
    /// detection logic.
    #[clap(short, long, global = true)]
    name: Option<OsString>,

    /// Use the driver's raw brightness values instead of percentage.
    #[clap(short, long, global = true)]
    raw: bool,

    /// Exit with a non-zero status if the requested brightness would be out of
    /// range for the device. This can be useful for detecting when the top or
    /// bottom of the scale has been reached, to provide user feedback.
    #[clap(short, long, global = true)]
    picky: bool,

    #[clap(subcommand)]
    cmd: SubCmd,
}

#[derive(Copy, Clone, Debug, Parser)]
enum SubCmd {
    /// Print the current backlight setting in the format "x/y", where x is the
    /// current setting, and y is the max.
    Get,
    /// Set the backlight to a specific value.
    Set {
        /// New backlight value.
        value: u32,
    },
    /// Increase the backlight brightness relative to its current level.
    Up {
        /// Amount to increase by.
        by: u32,
    },
    /// Decrease the backlight brightness relative to its current level.
    Down {
        /// Amount to decrease by.
        by: u32,
    },
}

fn main() -> anyhow::Result<()> {
    // First, validate the arguments.
    let args = Brightr::parse();

    // Then, see if there is a supported and matching backlight device. This way
    // we can warn the user if their system is unsupported, before presenting
    // possibly confusing DBus errors.
    let Backlight { name, current, max } = if let Some(name) = args.name {
        use_specific_backlight(name)?
    } else {
        find_backlight()?
    };

    // Ensure the device name can be formatted as UTF-8, which is required for
    // use with zbus. Since the Linux kernel tends to use 7-bit ascii for device
    // names, this _should_ always succeed, but....
    let Some(name) = name.to_str() else {
        // This _really_ shouldn't be able to happen, but.
        bail!("backlight name not valid UTF-8?! name: {:?}", name);
    };

    // Apply the requested brightness twiddling to compute a new target value,
    // if needed. We produce None here if the value is unrepresentable, which
    // mostly happens when trying to adjust the brightness down past zero, but
    // could also happen when adjusting _up_ on a particularly goofy device that
    // uses the full 32-bit brightness range.
    let mut target = match args.cmd {
        SubCmd::Get => {
            if args.raw {
                println!("{current}/{max}");
            } else {
                let pct_now = current * 100 / max;
                println!("{pct_now}/100");
            }
            // No change required for this verb. In fact, we'll just skip the
            // rest of the program, to simplify the common case below.
            return Ok(());
        }
        SubCmd::Set { value } => {
            if args.raw {
                Some(value)
            } else {
                Some(value * max / 100)
            }
        }
        SubCmd::Up { by } => {
            if args.raw {
                current.checked_add(by)
            } else {
                current.checked_add(by * max / 100)
            }
        }
        SubCmd::Down { by } => {
            if args.raw {
                current.checked_sub(by)
            } else {
                current.checked_sub(by * max / 100)
            }
        }
    };

    // Check value against device max.
    if let Some(v) = target {
        if v > max {
            // Flatten it to share error handling code below.
            target = None;
        }
    }

    // Send message if required. (We don't bother connecting to DBus at all for
    // the get subcommand.)
    if let Some(new_value) = target {
        // Clamp the value to the device's specified max.
        let new_value = u32::min(max, new_value);

        // Set up our DBus connection to the current session (.../session/auto).
        // Note that this happens on the SYSTEM bus, _not_ the SESSION bus!
        // This confused me too.
        let conn = Connection::system()?;
        let session = SessionProxyBlocking::builder(&conn)
            .path("/org/freedesktop/login1/session/auto")?
            .build()?;

        session.set_brightness("backlight", name, new_value)
            .with_context(|| format!("can't set backlight {name}"))?;
    } else if args.picky {
        // We've got an out of range brightness value!
        bail!("can't adjust brightness outside of range of device")
    }
    
    Ok(())
}

/// Locates the first suitable backlight device in `/sys/class/backlight`.
///
/// The Session proxy in logind will happily let us set the backlight, if we
/// know the backlight's subsystem and name. It does not, however, provide us
/// with any way to actually _discover_ that information. And so we do it the
/// hard way.
///
/// Fortunately the hard way is available to unprivileged users, and that's
/// presumably why logind didn't offer to proxy it for us.
fn find_backlight() -> anyhow::Result<Backlight> {
    let dir = fs::read_dir("/sys/class/backlight")
        .context("can't access directory /sys/class/backlight")?;

    for dirent in dir {
        let dirent = dirent?;
        let path = dirent.path();

        match read_backlight_settings(&path) {
            Ok((current, max)) => {
                // We'll take the first one we found.
                let name = path.file_name().expect("file should have a name");
                return Ok(Backlight {
                    name: name.to_owned(),
                    current,
                    max,
                });
            }
            Err(e) => {
                eprintln!("skipping backlight-like device at {}: {e}", path.display());
            }
        }
    }

    bail!("cannot find any valid backlight devices in /sys/class/backlight")
}

struct Backlight {
    name: OsString,
    current: u32,
    max: u32,
}

/// Finds a backlight given a user-specified name.
fn use_specific_backlight(name: OsString) -> anyhow::Result<Backlight> {
    let path = Path::new("/sys/class/backlight").join(&name);
    let (current, max) = read_backlight_settings(&path)
        .with_context(|| format!("can't use explicitly requested backlight device {name:?}"))?;

    Ok(Backlight {
        name,
        current,
        max,
    })
}

/// Loads settings for a single backlight device given its fully-qualified
/// directory path. Returns: `(current_value, max_value)`.
fn read_backlight_settings(path: &Path) -> anyhow::Result<(u32, u32)> {
    let mut parsed = vec![];
    for component in ["brightness", "max_brightness"] {
        let c_path = path.join(component);
        let contents = fs::read_to_string(&c_path)
            .with_context(|| format!("reading backlight file {}", c_path.display()))?;
        let number = contents.trim().parse::<u32>()
            .with_context(|| format!("parsing brightness value from file {}: {contents}", c_path.display()))?;
        parsed.push(number);
    }
    Ok((parsed[0], parsed[1]))
}
