//! Monotone cubic (Fritsch-Carlson) interpolation for chart line series.
//!
//! The min/max temperature bands are drawn from raw data, but the average
//! line is pre-interpolated to many intermediate points so that plotters'
//! straight-line rendering visually becomes a smooth curve passing exactly
//! through every original data point.
//!
//! We use monotone cubic interpolation rather than a Catmull-Rom spline so
//! that the smoothed line never overshoots its neighbors — the interpolated
//! y value between two adjacent data points is always bounded by their
//! individual y values. This is important because the average line is drawn
//! on top of a min/max band: if the line overshot, it could visually escape
//! the band and misrepresent the data.

use chrono::{DateTime, TimeDelta, TimeZone};

use super::ChartPoint;

/// Number of intermediate samples generated between each pair of adjacent
/// input points during line interpolation.
pub(super) const INTERPOLATION_SUBDIVISIONS: usize = 12;

/// Linearly interpolate between two values of the chart's x-axis type at
/// fraction `t` in `[0, 1]`.
///
/// Implemented as a trait so that chart x axes (currently `DateTime<Utc>`)
/// can be interpolated without exposing their internal representation.
pub trait LinearInterpolate: Clone {
    /// Return the value at fraction `t` along the straight line from `self`
    /// (at `t = 0`) to `other` (at `t = 1`).
    fn linear_interpolate(&self, other: &Self, t: f64) -> Self;
}

impl<Tz: TimeZone> LinearInterpolate for DateTime<Tz> {
    fn linear_interpolate(&self, other: &Self, t: f64) -> Self {
        let delta_ns = other
            .clone()
            .signed_duration_since(self.clone())
            .num_nanoseconds()
            .unwrap_or(0);
        self.clone() + TimeDelta::nanoseconds((delta_ns as f64 * t) as i64)
    }
}

/// Compute per-point tangents for monotone cubic interpolation using the
/// Fritsch-Carlson method. Assumes uniformly-spaced x values (our hourly and
/// daily data both have fixed spacing), so tangents are expressed in
/// "y per segment" units.
///
/// The returned tangents guarantee that the resulting Hermite cubic stays
/// monotone on any interval where the input data is monotone, and in
/// particular never overshoots the surrounding data points.
fn monotone_tangents(ys: &[f64]) -> Vec<f64> {
    let n = ys.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0.0];
    }

    // Secant slopes between adjacent points (unit x spacing).
    let d: Vec<f64> = (0..n - 1).map(|i| ys[i + 1] - ys[i]).collect();

    // Initial tangents: endpoints copy the neighboring secant, interior
    // points average the two surrounding secants — unless the sign of the
    // slope changes, which marks a local extremum and forces a zero
    // tangent so the curve is flat there.
    let mut m = vec![0.0f64; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        if d[i - 1] * d[i] <= 0.0 {
            m[i] = 0.0;
        } else {
            m[i] = (d[i - 1] + d[i]) / 2.0;
        }
    }

    // Fritsch-Carlson monotonicity clamp: if a tangent is too large relative
    // to its segment's secant the curve can overshoot, so rescale both
    // endpoints of the offending segment.
    for i in 0..n - 1 {
        if d[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let alpha = m[i] / d[i];
        let beta = m[i + 1] / d[i];
        let s = alpha * alpha + beta * beta;
        if s > 9.0 {
            let tau = 3.0 / s.sqrt();
            m[i] = tau * alpha * d[i];
            m[i + 1] = tau * beta * d[i];
        }
    }

    m
}

/// Evaluate a Hermite cubic at parameter `t` in `[0, 1]` between `y0` and
/// `y1` with tangents `m0` and `m1`. Assumes a unit segment width, so the
/// tangents should already be scaled accordingly.
fn hermite(y0: f64, y1: f64, m0: f64, m1: f64, t: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    h00 * y0 + h10 * m0 + h01 * y1 + h11 * m1
}

/// Produce a denser sequence of `(x, y)` pairs using monotone cubic
/// (Fritsch-Carlson) interpolation for the y values and linear interpolation
/// for the x values.
///
/// The returned series passes exactly through every original point — no
/// min/max values are altered, only additional in-between points are added
/// so that plotters' straight-line rendering looks like a smooth curve. The
/// interpolated y value between two adjacent data points is bounded by their
/// own y values (no overshoot).
pub(super) fn interpolate_line<X: LinearInterpolate>(points: &[ChartPoint<X>]) -> Vec<(X, f64)> {
    if points.len() < 2 {
        return points.iter().map(|p| (p.x.clone(), p.avg)).collect();
    }
    let ys: Vec<f64> = points.iter().map(|p| p.avg).collect();
    let tangents = monotone_tangents(&ys);
    let n = points.len();
    let mut out: Vec<(X, f64)> = Vec::with_capacity((n - 1) * INTERPOLATION_SUBDIVISIONS + 1);
    for i in 0..n - 1 {
        let y0 = ys[i];
        let y1 = ys[i + 1];
        let m0 = tangents[i];
        let m1 = tangents[i + 1];
        for s in 0..INTERPOLATION_SUBDIVISIONS {
            let t = s as f64 / INTERPOLATION_SUBDIVISIONS as f64;
            let y = hermite(y0, y1, m0, m1, t);
            let x = points[i].x.linear_interpolate(&points[i + 1].x, t);
            out.push((x, y));
        }
    }
    out.push((points[n - 1].x.clone(), points[n - 1].avg));
    out
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::chart::{DISPLAY_TIMEZONE, HourlyPoint};

    mod monotone_tangents {
        use super::*;

        #[test]
        fn empty_input() {
            assert!(monotone_tangents(&[]).is_empty());
        }

        #[test]
        fn single_point() {
            assert_eq!(monotone_tangents(&[5.0]), vec![0.0]);
        }

        #[test]
        fn constant_series_has_zero_tangents() {
            let out = monotone_tangents(&[3.0, 3.0, 3.0, 3.0]);
            for v in out {
                assert!(v.abs() < 1e-9);
            }
        }

        #[test]
        fn linear_series_has_uniform_slope_tangents() {
            // A linear increase of 2 per step should give tangents of 2 everywhere.
            let out = monotone_tangents(&[0.0, 2.0, 4.0, 6.0, 8.0]);
            for v in out {
                assert!((v - 2.0).abs() < 1e-9);
            }
        }

        #[test]
        fn local_extremum_gets_zero_tangent() {
            // Peak at index 2 (values go up then down): tangent should be 0.
            let out = monotone_tangents(&[0.0, 2.0, 5.0, 2.0, 0.0]);
            assert!(out[2].abs() < 1e-9);
        }
    }

    mod hermite {
        use super::*;

        #[test]
        fn endpoints() {
            assert!((hermite(1.0, 5.0, 0.0, 0.0, 0.0) - 1.0).abs() < 1e-9);
            assert!((hermite(1.0, 5.0, 0.0, 0.0, 1.0) - 5.0).abs() < 1e-9);
        }

        #[test]
        fn linear_segment_with_matching_tangents() {
            // y0=0, y1=1, tangents=1 → straight line y = t.
            for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let v = hermite(0.0, 1.0, 1.0, 1.0, t);
                assert!((v - t).abs() < 1e-9, "t={t}: {v}");
            }
        }
    }

    mod interpolate_line {
        use super::*;

        fn point(ts: i64, avg: f64) -> HourlyPoint {
            HourlyPoint {
                x: Utc
                    .timestamp_opt(ts, 0)
                    .unwrap()
                    .with_timezone(&DISPLAY_TIMEZONE),
                min: avg - 0.5,
                max: avg + 0.5,
                avg,
            }
        }

        #[test]
        fn empty_input() {
            let empty: Vec<HourlyPoint> = vec![];
            assert!(interpolate_line(&empty).is_empty());
        }

        #[test]
        fn single_point_returned_as_is() {
            let input = vec![point(0, 10.0)];
            let out = interpolate_line(&input);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].1, 10.0);
        }

        #[test]
        fn passes_through_every_original_point() {
            let input: Vec<HourlyPoint> = (0..6).map(|i| point(i * 3600, i as f64 * 2.0)).collect();
            let out = interpolate_line(&input);
            // The first subdivision of each segment is the segment's start point.
            for (i, original) in input.iter().enumerate() {
                let idx = i * INTERPOLATION_SUBDIVISIONS;
                let idx = idx.min(out.len() - 1);
                assert!(
                    (out[idx].1 - original.avg).abs() < 1e-9,
                    "point {i}: {} vs {}",
                    out[idx].1,
                    original.avg
                );
            }
        }

        #[test]
        fn generates_n_minus_one_times_subdivisions_plus_one_points() {
            let input: Vec<HourlyPoint> = (0..4).map(|i| point(i * 3600, i as f64)).collect();
            let out = interpolate_line(&input);
            assert_eq!(
                out.len(),
                (input.len() - 1) * INTERPOLATION_SUBDIVISIONS + 1
            );
        }

        #[test]
        fn no_overshoot_on_spiky_input() {
            // Alternating up/down pattern — the classic Catmull-Rom overshoot
            // case. Monotone cubic must keep every interpolated y within the
            // range of the two surrounding original values.
            let ys = [10.0, 20.0, 5.0, 25.0, 8.0, 22.0, 12.0];
            let input: Vec<HourlyPoint> = ys
                .iter()
                .enumerate()
                .map(|(i, &y)| point(i as i64 * 3600, y))
                .collect();
            let out = interpolate_line(&input);

            for seg in 0..ys.len() - 1 {
                let lo = ys[seg].min(ys[seg + 1]);
                let hi = ys[seg].max(ys[seg + 1]);
                let start = seg * INTERPOLATION_SUBDIVISIONS;
                let end = (seg + 1) * INTERPOLATION_SUBDIVISIONS;
                for (k, (_, y)) in out[start..=end].iter().enumerate() {
                    assert!(
                        *y >= lo - 1e-9 && *y <= hi + 1e-9,
                        "segment {seg} sample {k}: y={y} out of [{lo}, {hi}]"
                    );
                }
            }
        }
    }

    mod linear_interpolate {
        use super::*;

        #[test]
        fn datetime_midpoint() {
            let a = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
            let b = Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
            let m = a.linear_interpolate(&b, 0.5);
            assert_eq!(m, Utc.with_ymd_and_hms(2025, 1, 1, 5, 0, 0).unwrap());
        }

        #[test]
        fn datetime_endpoints() {
            let a = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
            let b = Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
            assert_eq!(a.linear_interpolate(&b, 0.0), a);
            assert_eq!(a.linear_interpolate(&b, 1.0), b);
        }
    }
}
