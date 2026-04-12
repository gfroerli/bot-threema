//! Render a sample sensor detail PNG and write it to disk.
//!
//! Useful for iterating on `chart::render_sensor_charts` layout changes
//! without needing a live Gfrörli API or Threema client.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example dump_chart -- out.png
//! ```

use std::{env, path::PathBuf, process};

use anyhow::Result;
use chrono::{NaiveDate, TimeDelta, TimeZone, Utc};

use gfroerli_bot::chart::{self, DailyPoint, HourlyPoint};

fn sample_hourly() -> Vec<HourlyPoint> {
    let base = Utc.with_ymd_and_hms(2025, 7, 15, 0, 0, 0).unwrap();
    (0..24)
        .map(|h| {
            let avg = 18.0 + (h as f64 * 0.2).sin();
            HourlyPoint {
                x: base + TimeDelta::hours(h),
                min: avg - 0.5,
                max: avg + 0.5,
                avg,
            }
        })
        .collect()
}

fn sample_daily() -> Vec<DailyPoint> {
    let base = NaiveDate::from_ymd_opt(2025, 6, 15).unwrap();
    (0..30)
        .map(|d| {
            let avg = 17.0 + (d as f64 * 0.1).cos();
            DailyPoint {
                x: base + TimeDelta::days(d),
                min: avg - 1.2,
                max: avg + 1.5,
                avg,
            }
        })
        .collect()
}

fn main() -> Result<()> {
    let Some(out) = env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: cargo run --example dump_chart -- <output.png>");
        process::exit(2);
    };

    let png = chart::render_sensor_charts("Aare Bern", &sample_hourly(), &sample_daily())?;
    std::fs::write(&out, &png)?;
    println!("Wrote {} ({} bytes)", out.display(), png.len());
    Ok(())
}
