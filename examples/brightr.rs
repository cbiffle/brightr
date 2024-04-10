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
    #[clap(short, long, global = true, help_heading = "Device Options")]
    name: Option<OsString>,

    /// Use the driver's raw brightness values for all input and output instead
    /// of percentages.
    #[clap(short, long, global = true, help_heading = "Device Options")]
    raw: bool,

    /// Saturate the bottom end of the brightness range at this (raw) value
    /// rather than zero. This is useful for systems that shut the backlight off
    /// completely at zero, if you don't want them to do that.
    #[clap(
        long,
        short,
        global = true,
        default_value_t = 0,
        value_name = "RAW",
        help_heading = "Device Options"
    )]
    min: u32,

    /// Exit with a non-zero status if the device was already at the edge of its
    /// range and could not be adjusted further. This can be useful for
    /// detecting when the top or bottom of the scale has been reached, to
    /// provide user feedback.
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
    /// Increase the backlight brightness relative to its current level,
    /// saturating at the top of the device's range.
    Up {
        /// Amount to increase by.
        by: u32,
    },
    /// Decrease the backlight brightness relative to its current level,
    /// saturating at the requested minimum brightness level.
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
    let convert = |value| {
        if args.raw {
            value
        } else {
            bl.from_percent(value)
        }
    };

    // Apply the requested brightness twiddling to compute a new target value,
    // if needed. We produce None here if the value is unrepresentable, which
    // mostly happens when trying to adjust the brightness down past zero, but
    // could also happen when adjusting _up_ on a particularly goofy device that
    // uses the full 32-bit brightness range.
    let target = match args.cmd {
        SubCmd::Get => {
            let (num, den) = if args.raw {
                (current, max)
            } else {
                (bl.to_percent(current), 100)
            };
            println!("{num}/{den}");
            // No change required for this verb. In fact, we'll just skip the
            // rest of the program, to simplify the common case below.
            return Ok(());
        }
        // Set is just a unit conversion.
        SubCmd::Set { value } => convert(value),
        // Up/Down convert the unit, saturating on u32 overflow. On the "Up"
        // case this is ridiculous, on the "Down" case it keeps us from wrapping
        // past zero on release builds.
        SubCmd::Up { by } => {
            if args.picky && current == bl.max {
                bail!("cannot increase brightness past range for device")
            } else {
                current.saturating_add(convert(by))
            }
        }
        SubCmd::Down { by } => {
            if args.picky && current <= args.min {
                bail!("cannot decrease brightness past {}", args.min)
            } else {
                current.saturating_sub(convert(by))
            }
        }
    };

    // Send a message to the session, limiting the value sent to the device
    // range.
    brightr::connect_and_set_brightness(&bl, target.clamp(args.min, bl.max))?;

    Ok(())
}
