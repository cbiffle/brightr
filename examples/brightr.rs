// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A simple program for adjusting display backlight settings on Linux.
//!
//! This program uses systemd and logind to set the backlight without requiring
//! root privileges. It will only work when run by a user who is currently
//! logged in at the seat that controls the display in question.

use anyhow::bail;
use clap::Parser;
use std::ffi::OsString;

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
    let (bl, current) = if let Some(name) = args.name {
        brightr::use_specific_backlight(name)?
    } else {
        brightr::find_first_backlight()?
    };

    // Shorthand.
    let max = bl.max;

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
        brightr::connect_and_set_brightness(&bl, new_value)?;
    } else if args.picky {
        // We've got an out of range brightness value!
        bail!("can't adjust brightness outside of range of device")
    }
    
    Ok(())
}
