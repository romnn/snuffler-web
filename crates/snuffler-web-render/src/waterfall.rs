//! Raw waterfall image construction shared by canvas, SVG, and native backends.

use snuffler_web_core::bridge::{ViewData, ViewSpan, ViewTrack};

use crate::draw::RasterImage;

/// Reinterprets one span's packed colormap cells as a row-major RGBA image.
///
/// Pyrocko computes the colors and packs one RGBA pixel into each sample `f32`; this function
/// only flips the frequency axis and exposes the bytes to rendering backends.
///
/// The frequency bins are a property of the track and the time bins of the span, so a
/// channel that arrives in several pieces yields one image per piece.
#[expect(
    clippy::indexing_slicing,
    reason = "the data and destination bounds follow from the checked frequency-by-time bin dimensions"
)]
#[must_use]
pub fn build_image(view: &ViewData, track: &ViewTrack, span: &ViewSpan) -> Option<RasterImage> {
    if track.frequency_bin_count == 0 || span.count == 0 {
        return None;
    }
    let frequency_bins = usize::try_from(track.frequency_bin_count).ok()?;
    let time_bins = usize::try_from(span.count.checked_div(track.frequency_bin_count)?).ok()?;
    if time_bins == 0 {
        return None;
    }
    let data = view.samples(span);
    if data.len() < frequency_bins.saturating_mul(time_bins) {
        return None;
    }
    let mut rgba = vec![0_u8; time_bins.saturating_mul(frequency_bins).saturating_mul(4)];
    for column in 0..time_bins {
        for row in 0..frequency_bins {
            let frequency = frequency_bins - 1 - row;
            let pixel = data[column * frequency_bins + frequency].to_le_bytes();
            let offset = (row * time_bins + column) * 4;
            rgba[offset] = pixel[0];
            rgba[offset + 1] = pixel[1];
            rgba[offset + 2] = pixel[2];
            rgba[offset + 3] = pixel[3];
        }
    }
    Some(RasterImage {
        width: u32::try_from(time_bins).ok()?,
        height: u32::try_from(frequency_bins).ok()?,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use snuffler_web_core::bridge::{ViewData, ViewMeta, ViewSpan, ViewTrack};

    use super::build_image;

    #[test]
    fn image_rows_run_from_high_to_low_frequency() {
        let pixels = [
            [1, 2, 3, 4],
            [5, 6, 7, 8],
            [9, 10, 11, 12],
            [13, 14, 15, 16],
        ];
        let view = ViewData {
            meta: ViewMeta::default(),
            ys: pixels.into_iter().map(f32::from_le_bytes).collect(),
        };
        let span = ViewSpan {
            offset: 0,
            count: 4,
            ..ViewSpan::default()
        };
        let track = ViewTrack {
            frequency_bin_count: 2,
            ..ViewTrack::default()
        };

        let image = build_image(&view, &track, &span);
        assert!(image.is_some());
        let Some(image) = image else {
            return;
        };
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(
            image.rgba,
            [5, 6, 7, 8, 13, 14, 15, 16, 1, 2, 3, 4, 9, 10, 11, 12]
        );
    }
}
