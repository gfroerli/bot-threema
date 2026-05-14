//! Render a set of sample sensor detail PNGs to a directory.
//!
//! Useful for iterating on `chart::render_sensor_charts` layout changes
//! without needing a live Gfrörli API or Threema client. Produces three
//! fixtures that cover different temperature distributions.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example dump_chart -- /path/to/dir
//! ```

use std::{
    env,
    path::{Path, PathBuf},
    process,
};

use anyhow::{Context, Result};
use chrono::{TimeDelta, TimeZone};
use gfroerli_bot_threema::chart::{self, DISPLAY_TIMEZONE, DailyPoint, HourlyPoint};

/// A full scenario: two data series driving the two charts.
struct Scenario {
    file_name: &'static str,
    title: &'static str,
    hourly: Vec<HourlyPoint>,
    daily: Vec<DailyPoint>,
}

fn main() -> Result<()> {
    let Some(dir) = env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: cargo run --example dump_chart -- <output-dir>");
        process::exit(2);
    };

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create output dir {}", dir.display()))?;

    for scenario in [normal(), low_variability(), high_variability(), jagged()] {
        let out = dir.join(scenario.file_name);
        render(&out, &scenario)?;
    }

    Ok(())
}

/// Deterministic pseudo-random noise in `[-1, 1]` derived from `(i, seed)`
/// via splitmix64. Used to build jagged scenarios without pulling in the
/// `rand` crate.
fn noise(i: usize, seed: u64) -> f64 {
    let mut x = (i as u64 ^ seed).wrapping_mul(0x9E3779B97F4A7C15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58476D1CE4E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D049BB133111EB);
    x ^= x >> 31;
    (x as i64 as f64) / (i64::MAX as f64)
}

fn render(out: &Path, scenario: &Scenario) -> Result<()> {
    let rendered_at = DISPLAY_TIMEZONE
        .with_ymd_and_hms(2025, 7, 15, 14, 30, 0)
        .unwrap();
    let png = chart::render_sensor_charts(
        scenario.title,
        rendered_at,
        &scenario.hourly,
        &scenario.daily,
    )?;
    std::fs::write(out, &png).with_context(|| format!("failed to write {}", out.display()))?;
    println!("Wrote {} ({} bytes)", out.display(), png.len());
    Ok(())
}

/// A normal summer week on a Swiss river.
fn normal() -> Scenario {
    let hourly_base = DISPLAY_TIMEZONE
        .with_ymd_and_hms(2025, 7, 15, 0, 0, 0)
        .unwrap();
    let hourly = (0..24)
        .map(|h| {
            let avg = 18.0 + (h as f64 * 0.2).sin();
            HourlyPoint {
                x: hourly_base + TimeDelta::hours(h),
                min: avg - 0.5,
                max: avg + 0.5,
                avg,
            }
        })
        .collect();

    let daily_base = DISPLAY_TIMEZONE
        .with_ymd_and_hms(2025, 6, 15, 12, 0, 0)
        .unwrap();
    let daily = (0..30)
        .map(|d| {
            let avg = 17.0 + (d as f64 * 0.1).cos();
            DailyPoint {
                x: daily_base + TimeDelta::days(d),
                min: avg - 1.2,
                max: avg + 1.5,
                avg,
            }
        })
        .collect();

    Scenario {
        file_name: "normal.png",
        title: "Aare Bern",
        hourly,
        daily,
    }
}

/// A spring-fed water body that barely changes temperature: everything
/// stays between ~14.9 °C and ~15.5 °C.
fn low_variability() -> Scenario {
    let hourly_base = DISPLAY_TIMEZONE
        .with_ymd_and_hms(2025, 7, 15, 0, 0, 0)
        .unwrap();
    let hourly = (0..24)
        .map(|h| {
            let avg = 15.2 + 0.08 * (h as f64 * 0.5).sin();
            HourlyPoint {
                x: hourly_base + TimeDelta::hours(h),
                min: avg - 0.05,
                max: avg + 0.05,
                avg,
            }
        })
        .collect();

    let daily_base = DISPLAY_TIMEZONE
        .with_ymd_and_hms(2025, 6, 15, 12, 0, 0)
        .unwrap();
    let daily = (0..30)
        .map(|d| {
            let avg = 15.2 + 0.12 * (d as f64 * 0.2).cos();
            DailyPoint {
                x: daily_base + TimeDelta::days(d),
                min: avg - 0.18,
                max: avg + 0.18,
                avg,
            }
        })
        .collect();

    Scenario {
        file_name: "low_variability.png",
        title: "Quellfassung Rigi",
        hourly,
        daily,
    }
}

/// A mountain stream with wild swings: spikes and dips between ~3 °C and
/// ~29 °C driven by sun, rain, and snowmelt.
fn high_variability() -> Scenario {
    let hourly_base = DISPLAY_TIMEZONE
        .with_ymd_and_hms(2025, 7, 15, 0, 0, 0)
        .unwrap();
    let hourly = (0..24)
        .map(|h| {
            let hf = h as f64;
            let avg = 16.0 + 9.0 * (hf * 0.35).sin() + 4.0 * (hf * 0.9).cos();
            HourlyPoint {
                x: hourly_base + TimeDelta::hours(h),
                min: (avg - 4.0).max(3.0),
                max: (avg + 5.0).min(29.0),
                avg: avg.clamp(3.0, 29.0),
            }
        })
        .collect();

    let daily_base = DISPLAY_TIMEZONE
        .with_ymd_and_hms(2025, 6, 15, 12, 0, 0)
        .unwrap();
    let daily = (0..30)
        .map(|d| {
            let df = d as f64;
            let avg = 15.0 + 10.0 * (df * 0.4).sin() + 3.0 * (df * 1.1).cos();
            DailyPoint {
                x: daily_base + TimeDelta::days(d),
                min: (avg - 6.0).max(3.0),
                max: (avg + 7.0).min(29.0),
                avg: avg.clamp(3.0, 29.0),
            }
        })
        .collect();

    Scenario {
        file_name: "high_variability.png",
        title: "Bergbach Muottas",
        hourly,
        daily,
    }
}

/// A dataset whose adjacent samples jump around sharply with no underlying
/// smooth trend — the kind of series the spline interpolation is meant to
/// visibly soften.
fn jagged() -> Scenario {
    let hourly_base = DISPLAY_TIMEZONE
        .with_ymd_and_hms(2025, 7, 15, 0, 0, 0)
        .unwrap();
    let hourly = (0..24)
        .map(|h| {
            let avg = 17.0 + 2.8 * noise(h as usize, 1);
            HourlyPoint {
                x: hourly_base + TimeDelta::hours(h),
                min: avg - (0.4 + 0.9 * noise(h as usize, 11).abs()),
                max: avg + (0.4 + 0.9 * noise(h as usize, 21).abs()),
                avg,
            }
        })
        .collect();

    let daily_base = DISPLAY_TIMEZONE
        .with_ymd_and_hms(2025, 6, 15, 12, 0, 0)
        .unwrap();
    let daily = (0..30)
        .map(|d| {
            let avg = 16.0 + 3.5 * noise(d as usize, 2);
            DailyPoint {
                x: daily_base + TimeDelta::days(d),
                min: avg - (0.8 + 1.2 * noise(d as usize, 12).abs()),
                max: avg + (0.8 + 1.2 * noise(d as usize, 22).abs()),
                avg,
            }
        })
        .collect();

    Scenario {
        file_name: "jagged.png",
        title: "Noisy Brook",
        hourly,
        daily,
    }
}
