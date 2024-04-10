// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A simple program for adjusting display backlight settings on Linux.
//!
//! This program uses systemd and logind to set the backlight without requiring
//! root privileges. It will only work when run by a user who is currently
//! logged in at the seat that controls the display in question.

use anyhow::bail;
use brightr::Backlight;
use clap::Parser;
use std::ffi::OsString;

/// Adjust display backlight.
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

    /// Map percentages to raw values using this exponent, to apply gamma
    /// correction. A value of 3-4 is often about right; the default of 1 makes
    /// the mapping linear.
    #[clap(
        short,
        long,
        global = true,
        default_value_t = 1.,
        help_heading = "Device Options"
    )]
    exponent: f64,

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

    let current_pct = to_percent(&bl, args.exponent, current);

    // Apply the requested brightness twiddling to compute a new target value,
    // if needed. We produce None here if the value is unrepresentable, which
    // mostly happens when trying to adjust the brightness down past zero, but
    // could also happen when adjusting _up_ on a particularly goofy device that
    // uses the full 32-bit brightness range.
    let target = match args.cmd {
        SubCmd::Get => {
            let (num, den) = if args.raw {
                (current, bl.max)
            } else {
                (current_pct, 100)
            };
            println!("{num}/{den}");
            // No change required for this verb. In fact, we'll just skip the
            // rest of the program, to simplify the common case below.
            return Ok(());
        }
        // Set is just a unit conversion.
        SubCmd::Set { value } => {
            if args.raw {
                value
            } else {
                from_percent(&bl, args.exponent, value)
            }
        }
        // Up/Down convert the unit, saturating on u32 overflow. On the "Up"
        // case this is ridiculous, on the "Down" case it keeps us from wrapping
        // past zero on release builds.
        SubCmd::Up { by } => {
            if args.picky && current == bl.max {
                bail!("cannot increase brightness past range for device")
            } else if args.raw {
                current.saturating_add(by)
            } else {
                from_percent(&bl, args.exponent, current_pct.saturating_add(by))
            }
        }
        SubCmd::Down { by } => {
            if args.picky && current <= args.min {
                bail!("cannot decrease brightness past {}", args.min)
            } else if args.raw {
                current.saturating_sub(by)
            } else {
                from_percent(&bl, args.exponent, current_pct.saturating_sub(by))
            }
        }
    };

    // Send a message to the session, limiting the value sent to the device
    // range.
    brightr::connect_and_set_brightness(&bl, target.clamp(args.min, bl.max))?;

    Ok(())
}

/// Computes a percentage of this backlight's max.
///
/// `pct` must be between 0 and 100, inclusive.
fn from_percent(bl: &Backlight, e: f64, pct: u32) -> u32 {
    ((f64::from(pct) / 100.).powf(e) * f64::from(bl.max)) as u32
}

/// Converts a setting for this backlight into a percentage of max.
///
/// `value` must be valid for this backlight.
fn to_percent(bl: &Backlight, e: f64, value: u32) -> u32 {
    ((f64::from(value) / f64::from(bl.max)).powf(1. / e) * 100.) as u32
}
