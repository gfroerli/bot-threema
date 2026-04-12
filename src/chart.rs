//! Rendering of sensor detail charts as PNG images.
//!
//! Produces a single PNG that stacks a "Last 24 hours" hourly chart on top of
//! a "Last 30 days" daily chart. Each chart shows the min/max temperature
//! envelope as a shaded band and the average as a line.
//!
//! Submodules:
//!
//! - [`interpolation`] — Catmull-Rom spline interpolation of line series
//! - [`render`] — the top-level [`render_sensor_charts`] entry point and its
//!   plotters-based drawing helpers
//! - [`style`] — bundled fonts, sizing constants, and color helpers

use chrono::{DateTime, Utc};

mod interpolation;
mod render;
mod style;

pub use interpolation::LinearInterpolate;
pub use render::render_sensor_charts;

/// A single measurement point for a chart, generic over the x-axis type.
#[derive(Debug, Clone, Copy)]
pub struct ChartPoint<X> {
    pub x: X,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
}

/// Hourly measurement point keyed by a UTC datetime.
pub type HourlyPoint = ChartPoint<DateTime<Utc>>;

/// Daily measurement point. Uses `DateTime<Utc>` (conventionally noon UTC)
/// rather than `NaiveDate` so that the line renderer can sub-divide
/// individual days during spline interpolation.
pub type DailyPoint = ChartPoint<DateTime<Utc>>;
