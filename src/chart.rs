//! Rendering of sensor detail charts as PNG images.
//!
//! Produces a single PNG that stacks a "Last 24 hours" hourly chart on top of
//! a "Last 30 days" daily chart. Each chart shows the min/max temperature
//! envelope as a shaded band and the average as a line.

use std::sync::Once;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use plotters::{
    backend::{BitMapBackend, DrawingBackend},
    chart::ChartBuilder,
    coord::{Shift, ranged1d::AsRangedCoord},
    drawing::{DrawingArea, IntoDrawingArea},
    element::Polygon,
    series::LineSeries,
    style::{
        AsRelative, Color, FontStyle, IntoFont, RGBAColor, RGBColor, ShapeStyle, TextStyle, WHITE,
        register_font,
    },
};

/// Embedded regular sans-serif font (Noto Sans).
const FONT_REGULAR: &[u8] = include_bytes!("../assets/NotoSans-Regular.ttf");
/// Embedded bold sans-serif font (Noto Sans).
const FONT_BOLD: &[u8] = include_bytes!("../assets/NotoSans-Bold.ttf");

/// Final PNG width in pixels.
///
/// Sized for phone chat display — small enough that the built-in font sizes
/// below remain readable after Threema scales the image to the chat width.
const WIDTH: u32 = 720;
/// Final PNG height in pixels.
const HEIGHT: u32 = 1000;

/// Supersampling factor: render into a buffer this many times larger than the
/// final image, then downscale with a Lanczos filter. This smooths plotters'
/// jagged thick-line rendering and compensates for the ab_glyph text backend
/// producing poorly-kerned glyphs at the target font sizes.
const RENDER_SCALE: u32 = 2;

/// Internal render width (before downscaling).
const RENDER_WIDTH: u32 = WIDTH * RENDER_SCALE;
/// Internal render height (before downscaling).
const RENDER_HEIGHT: u32 = HEIGHT * RENDER_SCALE;

/// Font size of the top-level sensor name title.
const TITLE_FONT_SIZE: u32 = 42 * RENDER_SCALE;
/// Font size of each chart's caption (e.g. "Last 24 hours").
const CAPTION_FONT_SIZE: u32 = 26 * RENDER_SCALE;
/// Font size of axis tick labels.
const LABEL_FONT_SIZE: u32 = 20 * RENDER_SCALE;
/// Stroke width of the average temperature line.
const LINE_WIDTH: u32 = 3 * RENDER_SCALE;
/// Height of the title area at the top of the chart.
const TITLE_AREA_HEIGHT: i32 = 70 * RENDER_SCALE as i32;
/// Outer margin around each chart panel.
const CHART_MARGIN: i32 = 15 * RENDER_SCALE as i32;
/// Height of the x-axis label area below each chart.
const X_LABEL_AREA: i32 = 50 * RENDER_SCALE as i32;
/// Width of the y-axis label area to the left of each chart.
const Y_LABEL_AREA: i32 = 80 * RENDER_SCALE as i32;

/// Ensure fonts are registered exactly once for the plotters `ab_glyph` backend.
static REGISTER_FONTS: Once = Once::new();
fn ensure_fonts_registered() {
    REGISTER_FONTS.call_once(|| {
        // Errors here indicate a malformed font file bundled into the binary,
        // which is a build-time defect. Ignore registration errors at runtime;
        // plotters will fall back to empty labels if the font fails to parse
        let _ = register_font("sans-serif", FontStyle::Normal, FONT_REGULAR);
        let _ = register_font("sans-serif", FontStyle::Bold, FONT_BOLD);
    });
}

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

/// Daily measurement point keyed by a naive date.
pub type DailyPoint = ChartPoint<NaiveDate>;

/// Render sensor detail charts as a PNG image and return the encoded bytes.
pub fn render_sensor_charts(
    title: &str,
    hourly: &[HourlyPoint],
    daily: &[DailyPoint],
) -> Result<Vec<u8>> {
    ensure_fonts_registered();

    // Render into a supersampled RGB buffer. We'll downscale afterwards.
    let mut buffer = vec![0u8; (RENDER_WIDTH * RENDER_HEIGHT * 3) as usize];
    {
        let backend = BitMapBackend::with_buffer(&mut buffer, (RENDER_WIDTH, RENDER_HEIGHT));
        let root = backend.into_drawing_area();
        root.fill(&WHITE).context("failed to fill background")?;

        // Title area + two stacked chart areas
        let (title_area, charts_area) = root.split_vertically(TITLE_AREA_HEIGHT);
        title_area
            .titled(
                title,
                ("sans-serif", TITLE_FONT_SIZE, FontStyle::Bold).into_font(),
            )
            .context("failed to draw title")?;
        let (top, bottom) = charts_area.split_vertically((50).percent());

        draw_temperature_chart(&top, "Last 24 hours", hourly, |dt: &DateTime<Utc>| {
            dt.format("%H:%M").to_string()
        })?;
        draw_temperature_chart(&bottom, "Last 30 days", daily, |d: &NaiveDate| {
            d.format("%b %d").to_string()
        })?;

        root.present().context("failed to present drawing")?;
    }

    // Downscale from the supersampled buffer to the final size with a Lanczos
    // filter — this is what makes lines look even and text render cleanly.
    let rendered = image::RgbImage::from_raw(RENDER_WIDTH, RENDER_HEIGHT, buffer)
        .context("render buffer size mismatch")?;
    let final_image = image::imageops::resize(
        &rendered,
        WIDTH,
        HEIGHT,
        image::imageops::FilterType::Lanczos3,
    );

    // Encode the downscaled image as PNG
    let mut png = Vec::with_capacity((WIDTH * HEIGHT) as usize / 4);
    {
        use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
        PngEncoder::new(&mut png)
            .write_image(final_image.as_raw(), WIDTH, HEIGHT, ExtendedColorType::Rgb8)
            .context("failed to encode PNG")?;
    }
    Ok(png)
}

/// Line color for the average temperature series.
fn avg_color() -> RGBColor {
    RGBColor(0x1f, 0x77, 0xb4)
}

/// Fill color for the min/max temperature band.
fn band_color() -> RGBAColor {
    RGBColor(0x1f, 0x77, 0xb4).mix(0.2)
}

/// Compute a y-axis range from a slice of `(min, max)` pairs with padding.
fn y_range<I>(iter: I) -> (f64, f64)
where
    I: IntoIterator<Item = (f64, f64)>,
{
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for (mn, mx) in iter {
        if mn < lo {
            lo = mn;
        }
        if mx > hi {
            hi = mx;
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return (0.0, 1.0);
    }
    // Pad the range so the series is not flush against the axes
    let pad = ((hi - lo).abs() * 0.1).max(0.5);
    (lo - pad, hi + pad)
}

/// Draw a temperature chart (min/max band + average line) into a drawing area.
fn draw_temperature_chart<DB, X, XF>(
    area: &DrawingArea<DB, Shift>,
    caption: &str,
    points: &[ChartPoint<X>],
    x_label_formatter: XF,
) -> Result<()>
where
    DB: DrawingBackend,
    DB::ErrorType: 'static,
    X: Clone + Ord + 'static,
    std::ops::Range<X>: AsRangedCoord<Value = X>,
    <std::ops::Range<X> as AsRangedCoord>::CoordDescType:
        plotters::coord::ranged1d::ValueFormatter<X>,
    XF: Fn(&X) -> String,
{
    if points.is_empty() {
        draw_empty_panel(area, caption)?;
        return Ok(());
    }

    let x_min = points.iter().map(|p| p.x.clone()).min().unwrap();
    let x_max = points.iter().map(|p| p.x.clone()).max().unwrap();
    let (y_lo, y_hi) = y_range(points.iter().map(|p| (p.min, p.max)));

    let mut chart = ChartBuilder::on(area)
        .caption(
            caption,
            ("sans-serif", CAPTION_FONT_SIZE, FontStyle::Bold).into_font(),
        )
        .margin(CHART_MARGIN)
        .x_label_area_size(X_LABEL_AREA)
        .y_label_area_size(Y_LABEL_AREA)
        .build_cartesian_2d(x_min..x_max, y_lo..y_hi)
        .map_err(|e| anyhow::anyhow!("failed to build chart '{caption}': {e}"))?;

    chart
        .configure_mesh()
        .x_labels(5)
        .x_label_formatter(&x_label_formatter)
        .y_labels(5)
        .y_label_formatter(&|v| format!("{v:.1}°C"))
        .label_style(("sans-serif", LABEL_FONT_SIZE).into_font())
        .draw()
        .map_err(|e| anyhow::anyhow!("failed to draw mesh for '{caption}': {e}"))?;

    // Min/max band (forward min, reverse max polygon)
    let mut band: Vec<(X, f64)> = points.iter().map(|p| (p.x.clone(), p.min)).collect();
    band.extend(points.iter().rev().map(|p| (p.x.clone(), p.max)));
    chart
        .draw_series(std::iter::once(Polygon::new(band, band_color().filled())))
        .map_err(|e| anyhow::anyhow!("failed to draw band for '{caption}': {e}"))?;

    // Average line
    chart
        .draw_series(LineSeries::new(
            points.iter().map(|p| (p.x.clone(), p.avg)),
            ShapeStyle::from(avg_color()).stroke_width(LINE_WIDTH),
        ))
        .map_err(|e| anyhow::anyhow!("failed to draw line for '{caption}': {e}"))?;

    Ok(())
}

/// Draw a simple caption + "no data" placeholder when a series is empty.
fn draw_empty_panel<DB>(area: &DrawingArea<DB, Shift>, caption: &str) -> Result<()>
where
    DB: DrawingBackend,
    DB::ErrorType: 'static,
{
    let (cap_area, body) = area.split_vertically(40 * RENDER_SCALE as i32);
    cap_area
        .titled(
            caption,
            ("sans-serif", CAPTION_FONT_SIZE, FontStyle::Bold).into_font(),
        )
        .map_err(|e| anyhow::anyhow!("failed to draw empty caption: {e}"))?;
    let (w, h) = body.dim_in_pixel();
    body.draw(&plotters::element::Text::new(
        "no data".to_string(),
        (w as i32 / 2 - 40 * RENDER_SCALE as i32, h as i32 / 2),
        TextStyle::from(("sans-serif", LABEL_FONT_SIZE).into_font())
            .color(&plotters::style::BLACK.mix(0.6)),
    ))
    .map_err(|e| anyhow::anyhow!("failed to draw empty label: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, TimeZone};

    use super::*;

    mod y_range {
        use super::*;

        #[test]
        fn pads_normal_range() {
            let (lo, hi) = y_range([(10.0, 20.0), (12.0, 18.0)]);
            assert!(lo < 10.0);
            assert!(hi > 20.0);
        }

        #[test]
        fn pads_flat_range() {
            let (lo, hi) = y_range([(15.0, 15.0)]);
            assert!(lo < 15.0);
            assert!(hi > 15.0);
        }

        #[test]
        fn empty_yields_default() {
            let (lo, hi) = y_range(std::iter::empty());
            assert_eq!((lo, hi), (0.0, 1.0));
        }
    }

    mod render_sensor_charts {
        use super::*;

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

        #[test]
        fn produces_png_bytes() {
            let png = render_sensor_charts("Aare Bern", &sample_hourly(), &sample_daily()).unwrap();
            assert!(
                png.len() > 1024,
                "PNG suspiciously small: {} bytes",
                png.len()
            );
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "missing PNG magic");
        }

        #[test]
        fn handles_empty_series() {
            let png = render_sensor_charts("Empty", &[], &[]).unwrap();
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        }

        #[test]
        fn handles_single_point() {
            let base = Utc.with_ymd_and_hms(2025, 7, 15, 12, 0, 0).unwrap();
            let hourly = vec![HourlyPoint {
                x: base,
                min: 18.0,
                max: 18.0,
                avg: 18.0,
            }];
            let daily = vec![DailyPoint {
                x: NaiveDate::from_ymd_opt(2025, 7, 15).unwrap(),
                min: 18.0,
                max: 18.0,
                avg: 18.0,
            }];
            let png = render_sensor_charts("Flat", &hourly, &daily).unwrap();
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        }
    }
}
