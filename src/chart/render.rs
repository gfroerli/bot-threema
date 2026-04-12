//! Top-level chart rendering entry point and its drawing helpers.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use plotters::{
    backend::{BitMapBackend, DrawingBackend},
    chart::ChartBuilder,
    coord::{Shift, ranged1d::AsRangedCoord},
    drawing::{DrawingArea, IntoDrawingArea},
    element::{Polygon, Text},
    series::LineSeries,
    style::{
        AsRelative, Color, FontStyle, IntoFont, ShapeStyle, TextStyle, WHITE,
        text_anchor::{HPos, Pos, VPos},
    },
};

use super::{
    ChartPoint, DailyPoint, HourlyPoint,
    interpolation::{LinearInterpolate, interpolate_line},
    style::{
        CAPTION_FONT_SIZE, CHART_MARGIN, FOOTER_AREA_HEIGHT, FOOTER_FONT_SIZE,
        FOOTER_PADDING_RIGHT, HEIGHT, LABEL_FONT_SIZE, LINE_WIDTH, RENDER_HEIGHT, RENDER_SCALE,
        RENDER_WIDTH, TITLE_AREA_HEIGHT, TITLE_FONT_SIZE, TITLE_TOP_PADDING, WIDTH, X_LABEL_AREA,
        Y_LABEL_AREA, avg_color, band_color, ensure_fonts_registered,
    },
};

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

        // Top padding + title + two stacked chart areas + footer
        let (_top_pad, below_pad) = root.split_vertically(TITLE_TOP_PADDING);
        let (title_area, body_area) = below_pad.split_vertically(TITLE_AREA_HEIGHT);
        title_area
            .titled(
                title,
                ("sans-serif", TITLE_FONT_SIZE, FontStyle::Bold).into_font(),
            )
            .context("failed to draw title")?;

        let body_height = body_area.dim_in_pixel().1 as i32;
        let (charts_area, footer_area) =
            body_area.split_vertically(body_height - FOOTER_AREA_HEIGHT);
        let (top, bottom) = charts_area.split_vertically((50).percent());

        draw_temperature_chart(&top, "Last 24 hours", hourly, |dt: &DateTime<Utc>| {
            dt.format("%H:%M").to_string()
        })?;
        draw_temperature_chart(&bottom, "Last 30 days", daily, |d: &DateTime<Utc>| {
            d.format("%d.%m.").to_string()
        })?;

        draw_footer(&footer_area)?;

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
    X: Clone + Ord + LinearInterpolate + 'static,
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

    // Average line — interpolate with a Catmull-Rom spline so that plotters'
    // straight-line rendering visually becomes a smooth curve. The line still
    // passes exactly through every original data point.
    let line = interpolate_line(points);
    chart
        .draw_series(LineSeries::new(
            line,
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
    body.draw(&Text::new(
        "no data".to_string(),
        (w as i32 / 2 - 40 * RENDER_SCALE as i32, h as i32 / 2),
        TextStyle::from(("sans-serif", LABEL_FONT_SIZE).into_font())
            .color(&plotters::style::BLACK.mix(0.6)),
    ))
    .map_err(|e| anyhow::anyhow!("failed to draw empty label: {e}"))?;
    Ok(())
}

/// Draw the footer text (project name + URL) right-aligned at the bottom.
fn draw_footer<DB>(area: &DrawingArea<DB, Shift>) -> Result<()>
where
    DB: DrawingBackend,
    DB::ErrorType: 'static,
{
    let (w, h) = area.dim_in_pixel();
    let right = w as i32 - FOOTER_PADDING_RIGHT;
    let line_gap = (FOOTER_FONT_SIZE as f32 * 1.3) as i32;
    let center_y = h as i32 / 2;
    let anchor = Pos::new(HPos::Right, VPos::Center);
    let name_color = plotters::style::BLACK.mix(0.75);
    let url_color = plotters::style::BLACK.mix(0.55);

    area.draw(&Text::new(
        "Gfrörli – Swiss Water Temperatures".to_string(),
        (right, center_y - line_gap / 2),
        TextStyle::from(("sans-serif", FOOTER_FONT_SIZE, FontStyle::Bold).into_font())
            .color(&name_color)
            .pos(anchor),
    ))
    .map_err(|e| anyhow::anyhow!("failed to draw footer name: {e}"))?;

    area.draw(&Text::new(
        "https://gfrör.li/".to_string(),
        (right, center_y + line_gap / 2),
        TextStyle::from(("sans-serif", FOOTER_FONT_SIZE).into_font())
            .color(&url_color)
            .pos(anchor),
    ))
    .map_err(|e| anyhow::anyhow!("failed to draw footer url: {e}"))?;

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
            let base = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
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
                x: Utc.with_ymd_and_hms(2025, 7, 15, 12, 0, 0).unwrap(),
                min: 18.0,
                max: 18.0,
                avg: 18.0,
            }];
            let png = render_sensor_charts("Flat", &hourly, &daily).unwrap();
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        }
    }
}
