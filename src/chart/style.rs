//! Constants, fonts, and colors used across chart rendering.

use std::sync::Once;

use plotters::style::{Color, FontStyle, RGBAColor, RGBColor, register_font};

/// Embedded regular sans-serif font (Noto Sans).
const FONT_REGULAR: &[u8] = include_bytes!("../../assets/NotoSans-Regular.ttf");
/// Embedded bold sans-serif font (Noto Sans).
const FONT_BOLD: &[u8] = include_bytes!("../../assets/NotoSans-Bold.ttf");

/// Final PNG width in pixels.
///
/// Sized for phone chat display — small enough that the built-in font sizes
/// below remain readable after Threema scales the image to the chat width.
pub(super) const WIDTH: u32 = 720;
/// Final PNG height in pixels.
pub(super) const HEIGHT: u32 = 1000;

/// Supersampling factor: render into a buffer this many times larger than the
/// final image, then downscale with a Lanczos filter. This smooths plotters'
/// jagged thick-line rendering and compensates for the ab_glyph text backend
/// producing poorly-kerned glyphs at the target font sizes.
pub(super) const RENDER_SCALE: u32 = 2;

/// Internal render width (before downscaling).
pub(super) const RENDER_WIDTH: u32 = WIDTH * RENDER_SCALE;
/// Internal render height (before downscaling).
pub(super) const RENDER_HEIGHT: u32 = HEIGHT * RENDER_SCALE;

/// Font size of the top-level sensor name title.
pub(super) const TITLE_FONT_SIZE: u32 = 42 * RENDER_SCALE;
/// Font size of each chart's caption (e.g. "Last 24 hours").
pub(super) const CAPTION_FONT_SIZE: u32 = 26 * RENDER_SCALE;
/// Font size of axis tick labels.
pub(super) const LABEL_FONT_SIZE: u32 = 20 * RENDER_SCALE;
/// Stroke width of the average temperature line.
pub(super) const LINE_WIDTH: u32 = 3 * RENDER_SCALE;
/// Blank padding above the title.
pub(super) const TITLE_TOP_PADDING: i32 = 24 * RENDER_SCALE as i32;
/// Height of the title area at the top of the chart.
pub(super) const TITLE_AREA_HEIGHT: i32 = 80 * RENDER_SCALE as i32;
/// Outer margin around each chart panel.
pub(super) const CHART_MARGIN: i32 = 15 * RENDER_SCALE as i32;
/// Height of the x-axis label area below each chart.
pub(super) const X_LABEL_AREA: i32 = 50 * RENDER_SCALE as i32;
/// Width of the y-axis label area to the left of each chart.
pub(super) const Y_LABEL_AREA: i32 = 80 * RENDER_SCALE as i32;
/// Height reserved at the bottom of the image for the footer.
pub(super) const FOOTER_AREA_HEIGHT: i32 = 80 * RENDER_SCALE as i32;
/// Right-side padding of the footer text.
pub(super) const FOOTER_PADDING_RIGHT: i32 = 24 * RENDER_SCALE as i32;
/// Font size of the footer text.
pub(super) const FOOTER_FONT_SIZE: u32 = 18 * RENDER_SCALE;

/// Ensure fonts are registered exactly once for the plotters `ab_glyph` backend.
static REGISTER_FONTS: Once = Once::new();

/// Register the bundled Noto Sans fonts with plotters' ab_glyph backend. Safe
/// to call repeatedly; registration only happens on the first call.
pub(super) fn ensure_fonts_registered() {
    REGISTER_FONTS.call_once(|| {
        // Errors here indicate a malformed font file bundled into the binary,
        // which is a build-time defect. Ignore registration errors at runtime;
        // plotters will fall back to empty labels if the font fails to parse
        let _ = register_font("sans-serif", FontStyle::Normal, FONT_REGULAR);
        let _ = register_font("sans-serif", FontStyle::Bold, FONT_BOLD);
    });
}

/// Line color for the average temperature series.
pub(super) fn avg_color() -> RGBColor {
    RGBColor(0x1f, 0x77, 0xb4)
}

/// Fill color for the min/max temperature band.
pub(super) fn band_color() -> RGBAColor {
    RGBColor(0x1f, 0x77, 0xb4).mix(0.2)
}
