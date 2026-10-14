//! Image comparison for golden-image tests (ADR-0010).
//!
//! Two images match when no more than [`Tolerance::outlier_fraction`] of their
//! pixels differ by more than [`Tolerance::max_channel_delta`] levels in some
//! channel. The default is the M1 golden tolerance: 2 levels, 0.1% outliers.

use re_core::Diagnostic;

use crate::capture::Image;
use crate::codes;

/// How different two images may be and still match.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerance {
    /// A pixel is an outlier when some channel differs by more than this.
    pub max_channel_delta: u8,
    /// The share of outlier pixels allowed, from 0 to 1.
    pub outlier_fraction: f64,
}

impl Default for Tolerance {
    /// 2 levels per channel, 0.1% outliers.
    fn default() -> Self {
        Tolerance {
            max_channel_delta: 2,
            outlier_fraction: 0.001,
        }
    }
}

/// The result of [`compare`].
#[derive(Clone, Debug, PartialEq)]
pub struct DiffReport {
    /// Width of both images.
    pub width: u32,
    /// Height of both images.
    pub height: u32,
    /// Pixels that differ at all.
    pub differing: usize,
    /// Pixels that differ by more than the tolerance (outliers).
    pub outliers: usize,
    /// The largest difference in any channel of any pixel.
    pub max_delta: u8,
    /// The tolerance used.
    pub tolerance: Tolerance,
}

impl DiffReport {
    /// The number of pixels.
    pub fn pixels(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// The share of outlier pixels, from 0 to 1.
    pub fn outlier_fraction(&self) -> f64 {
        if self.pixels() == 0 {
            0.0
        } else {
            self.outliers as f64 / self.pixels() as f64
        }
    }

    /// True when the outliers stay within the tolerance's budget.
    pub fn matches(&self) -> bool {
        self.outlier_fraction() <= self.tolerance.outlier_fraction
    }

    /// One line describing the comparison.
    pub fn summary(&self) -> String {
        format!(
            "{}x{}: {} differing pixels, {} over {} levels ({:.4}%, budget {:.4}%), max channel delta {}: {}",
            self.width,
            self.height,
            self.differing,
            self.outliers,
            self.tolerance.max_channel_delta,
            100.0 * self.outlier_fraction(),
            100.0 * self.tolerance.outlier_fraction,
            self.max_delta,
            if self.matches() { "match" } else { "MISMATCH" }
        )
    }
}

/// Compares two images. Fails with `RE6023` when their sizes differ.
pub fn compare(a: &Image, b: &Image, tolerance: Tolerance) -> Result<DiffReport, Diagnostic> {
    if (a.width, a.height) != (b.width, b.height) {
        return Err(Diagnostic::error(
            codes::IMAGE_SIZE_MISMATCH,
            format!(
                "the images have different sizes: {}x{} and {}x{}",
                a.width, a.height, b.width, b.height
            ),
        ));
    }
    let mut report = DiffReport {
        width: a.width,
        height: a.height,
        differing: 0,
        outliers: 0,
        max_delta: 0,
        tolerance,
    };
    for (pa, pb) in a
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.rgba.as_chunks::<4>().0)
    {
        let delta = pixel_delta(*pa, *pb);
        if delta > 0 {
            report.differing += 1;
        }
        if delta > tolerance.max_channel_delta {
            report.outliers += 1;
        }
        report.max_delta = report.max_delta.max(delta);
    }
    Ok(report)
}

fn pixel_delta(a: [u8; 4], b: [u8; 4]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(y))
        .max()
        .unwrap_or(0)
}

/// A picture of the differences: matching pixels as a faded grey copy of `a`,
/// pixels within tolerance in yellow and outliers in red. Both images must have
/// the same size (see [`compare`]).
pub fn diff_image(a: &Image, b: &Image, tolerance: Tolerance) -> Result<Image, Diagnostic> {
    compare(a, b, tolerance)?;
    let rgba = a
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.rgba.as_chunks::<4>().0)
        .flat_map(|(pa, pb)| {
            let delta = pixel_delta(*pa, *pb);
            if delta == 0 {
                let grey = (crate::capture::luma(*pa) * 0.3 + 160.0) as u8;
                [grey, grey, grey, 255]
            } else if delta <= tolerance.max_channel_delta {
                [255, 210, 0, 255]
            } else {
                [230, 20, 20, 255]
            }
        })
        .collect();
    Ok(Image {
        width: a.width,
        height: a.height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, value: u8) -> Image {
        Image {
            width,
            height,
            rgba: vec![value; (width * height * 4) as usize],
        }
    }

    #[test]
    fn tolerance_counts_outliers_against_the_budget() {
        let a = image(100, 100, 100);
        let mut b = a.clone();
        b.rgba[0] = 102; // within 2 levels
        b.rgba[4] = 110; // one outlier: 1 of 10,000 pixels = 0.01%
        let report = compare(&a, &b, Tolerance::default()).unwrap();
        assert_eq!(
            (report.differing, report.outliers, report.max_delta),
            (2, 1, 10)
        );
        assert!(report.matches(), "{}", report.summary());
        for i in 0..20 {
            b.rgba[i * 4 + 400] = 0; // 21 outliers = 0.21%
        }
        let report = compare(&a, &b, Tolerance::default()).unwrap();
        assert!(!report.matches(), "{}", report.summary());
        let diff = diff_image(&a, &b, Tolerance::default()).unwrap();
        assert_eq!(diff.pixel(0, 0), [255, 210, 0, 255]);
        assert_eq!(diff.pixel(1, 0), [230, 20, 20, 255]);
    }

    #[test]
    fn size_mismatch_is_a_diagnostic() {
        let err = compare(&image(2, 2, 0), &image(2, 3, 0), Tolerance::default()).unwrap_err();
        assert_eq!(err.code, codes::IMAGE_SIZE_MISMATCH);
    }
}
