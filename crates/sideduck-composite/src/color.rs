//! Linear-light colour conversion and alpha conventions.
//!
//! Transfer curves and D65 primary matrices follow CSS Color 4's conversion
//! definitions: <https://www.w3.org/TR/css-color-4/#color-conversion-code>.
//! Display P3 shares the sRGB transfer curve, but has different primaries.
//! Matrices preserve finite out-of-gamut values; gamut clipping belongs at the
//! destination encoding boundary. RGB transfer curves never operate on alpha.

use crate::blend::{unit, Pixel};

/// Decode one straight sRGB (or Display P3) component to linear light.
/// The signed extension preserves out-of-gamut finite values for conversions.
/// Nonfinite input or overflow produces zero.
pub fn srgb_to_linear(value: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    let absolute = value.abs();
    let result = if absolute <= 0.04045 {
        value / 12.92
    } else {
        value.signum() * ((absolute + 0.055) / 1.055).powf(2.4)
    };
    finite(result)
}

/// Encode one straight linear component using the sRGB/Display P3 transfer curve.
/// Values outside [0, 1] remain extended; this function performs no gamut clipping.
pub fn linear_to_srgb(value: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    let absolute = value.abs();
    let result = if absolute <= 0.003_130_8 {
        12.92 * value
    } else {
        value.signum() * (1.055 * absolute.powf(1.0 / 2.4) - 0.055)
    };
    finite(result)
}

/// Premultiply straight RGBA. Alpha is normalized; finite RGB may be out of gamut.
/// Multiplication by zero yields canonical transparent black.
pub fn premultiply(straight: Pixel) -> Pixel {
    let alpha = unit(straight[3]);
    if alpha == 0.0 {
        return [0.0; 4];
    }
    [
        finite(straight[0]) * alpha,
        finite(straight[1]) * alpha,
        finite(straight[2]) * alpha,
        alpha,
    ]
}

/// Unpremultiply RGBA, preserving finite out-of-gamut RGB and canonicalizing zero alpha.
/// Hidden RGB at zero alpha is discarded; it has no defined colour contribution.
pub fn unpremultiply(premultiplied: Pixel) -> Pixel {
    let alpha = unit(premultiplied[3]);
    if alpha == 0.0 {
        return [0.0; 4];
    }
    [
        finite(premultiplied[0] / alpha),
        finite(premultiplied[1] / alpha),
        finite(premultiplied[2] / alpha),
        alpha,
    ]
}

/// Convert straight, linear sRGB components to straight, linear Display P3.
/// Both spaces use D65, so no chromatic adaptation is needed.
pub fn linear_srgb_to_display_p3(rgb: [f32; 3]) -> [f32; 3] {
    multiply(SRGB_TO_P3, rgb)
}

/// Convert straight, linear Display P3 to linear sRGB without discarding gamut.
/// Saturated P3 primaries legitimately return negative or greater-than-one values.
pub fn linear_display_p3_to_srgb(rgb: [f32; 3]) -> [f32; 3] {
    multiply(P3_TO_SRGB, rgb)
}

/// Decode unassociated, encoded sRGB bytes into the compositor's linear premultiplied pixel.
pub fn rgba8_srgb_to_linear_premultiplied(bytes: [u8; 4]) -> Pixel {
    premultiply([
        srgb_to_linear(f32::from(bytes[0]) / 255.0),
        srgb_to_linear(f32::from(bytes[1]) / 255.0),
        srgb_to_linear(f32::from(bytes[2]) / 255.0),
        f32::from(bytes[3]) / 255.0,
    ])
}

/// Encode linear premultiplied sRGB to unassociated sRGB bytes.
/// This output boundary clips the destination gamut and rounds to nearest byte.
/// Zero-alpha output is canonical transparent black.
pub fn linear_premultiplied_to_rgba8_srgb(pixel: Pixel) -> [u8; 4] {
    let straight = unpremultiply(pixel);
    let alpha = to_byte(straight[3]);
    if alpha == 0 {
        return [0; 4];
    }
    [
        to_byte(linear_to_srgb(straight[0])),
        to_byte(linear_to_srgb(straight[1])),
        to_byte(linear_to_srgb(straight[2])),
        alpha,
    ]
}

/// Convert linear premultiplied sRGB to encoded, unassociated Display P3 RGBA.
/// This applies the P3 gamut matrix and its transfer curve, preserving alpha.
/// The caller must tag or configure the destination surface as Display P3;
/// presenting these values on an sRGB surface would change their appearance.
pub fn linear_premultiplied_srgb_to_display_p3(pixel: Pixel) -> Pixel {
    let straight = unpremultiply(pixel);
    let rgb = linear_srgb_to_display_p3([straight[0], straight[1], straight[2]]);
    [
        unit(linear_to_srgb(rgb[0])),
        unit(linear_to_srgb(rgb[1])),
        unit(linear_to_srgb(rgb[2])),
        straight[3],
    ]
}

/// Decode encoded, straight Display P3 RGBA to extended linear premultiplied sRGB.
/// Gamut conversion is intentionally unclipped, making an in-memory round trip possible.
pub fn display_p3_to_linear_premultiplied_srgb(pixel: Pixel) -> Pixel {
    let rgb = linear_display_p3_to_srgb([
        srgb_to_linear(pixel[0]),
        srgb_to_linear(pixel[1]),
        srgb_to_linear(pixel[2]),
    ]);
    premultiply([rgb[0], rgb[1], rgb[2], pixel[3]])
}

fn finite(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn to_byte(value: f32) -> u8 {
    (unit(value) * 255.0).round() as u8
}

// Rational D65 matrices from CSS Color 4, retained at f64 precision during
// coefficient composition. Matrix rows multiply the RGB/XYZ column vector.
const SRGB_TO_XYZ: [[f64; 3]; 3] = [
    [506752.0 / 1228815.0, 87881.0 / 245763.0, 12673.0 / 70218.0],
    [87098.0 / 409605.0, 175762.0 / 245763.0, 12673.0 / 175545.0],
    [7918.0 / 409605.0, 87881.0 / 737289.0, 1001167.0 / 1053270.0],
];
const XYZ_TO_SRGB: [[f64; 3]; 3] = [
    [12831.0 / 3959.0, -329.0 / 214.0, -1974.0 / 3959.0],
    [
        -851781.0 / 878810.0,
        1648619.0 / 878810.0,
        36519.0 / 878810.0,
    ],
    [705.0 / 12673.0, -2585.0 / 12673.0, 705.0 / 667.0],
];
const P3_TO_XYZ: [[f64; 3]; 3] = [
    [
        608311.0 / 1250200.0,
        189793.0 / 714400.0,
        198249.0 / 1000160.0,
    ],
    [
        35783.0 / 156275.0,
        247089.0 / 357200.0,
        198249.0 / 2500400.0,
    ],
    [0.0, 32229.0 / 714400.0, 5220557.0 / 5000800.0],
];
const XYZ_TO_P3: [[f64; 3]; 3] = [
    [
        446124.0 / 178915.0,
        -333277.0 / 357830.0,
        -72051.0 / 178915.0,
    ],
    [-14852.0 / 17905.0, 63121.0 / 35810.0, 423.0 / 17905.0],
    [11844.0 / 330415.0, -50337.0 / 660830.0, 316169.0 / 330415.0],
];
const SRGB_TO_P3: [[f64; 3]; 3] = matrix_product(XYZ_TO_P3, SRGB_TO_XYZ);
const P3_TO_SRGB: [[f64; 3]; 3] = matrix_product(XYZ_TO_SRGB, P3_TO_XYZ);

const fn matrix_product(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut result = [[0.0; 3]; 3];
    let mut row = 0;
    while row < 3 {
        let mut column = 0;
        while column < 3 {
            let mut inner = 0;
            while inner < 3 {
                result[row][column] += a[row][inner] * b[inner][column];
                inner += 1;
            }
            column += 1;
        }
        row += 1;
    }
    result
}

fn multiply(matrix: [[f64; 3]; 3], rgb: [f32; 3]) -> [f32; 3] {
    let rgb = rgb.map(|component| f64::from(finite(component)));
    matrix.map(|row| finite((row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]) as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close<const N: usize>(actual: [f32; N], expected: [f32; N], tolerance: f32) {
        for channel in 0..N {
            assert!(
                (actual[channel] - expected[channel]).abs() <= tolerance,
                "channel {channel}: {actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn srgb_transfer_matches_known_reference_values_and_preserves_alpha() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert_eq!(linear_to_srgb(0.0), 0.0);
        assert!((srgb_to_linear(0.5) - 0.214_041_14).abs() < 5e-8);
        assert!((linear_to_srgb(0.214_041_14) - 0.5).abs() < 1e-7);
        assert!((srgb_to_linear(0.04045) - 0.003_130_805).abs() < 1e-8);
        assert!((linear_to_srgb(0.003_130_8) - 0.040_449_936).abs() < 1e-8);
        let pixel = rgba8_srgb_to_linear_premultiplied([128, 64, 255, 128]);
        assert_eq!(pixel[3], 128.0 / 255.0);
        assert!(pixel[0] < 128.0 / 255.0 * pixel[3]);
    }

    #[test]
    fn color_transfer_round_trips_extended_negative_and_in_gamut_values() {
        for step in -500..=1500 {
            let encoded = step as f32 / 1000.0;
            assert!((linear_to_srgb(srgb_to_linear(encoded)) - encoded).abs() < 5e-7);
        }
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(srgb_to_linear(invalid), 0.0);
            assert_eq!(linear_to_srgb(invalid), 0.0);
        }
    }

    #[test]
    fn linear_srgb_display_p3_matrices_match_primary_and_white_reference_values() {
        close(
            linear_srgb_to_display_p3([1.0, 0.0, 0.0]),
            [0.822_462, 0.033_194_2, 0.017_082_63],
            1e-7,
        );
        close(
            linear_display_p3_to_srgb([1.0, 0.0, 0.0]),
            [1.224_940_2, -0.042_056_96, -0.019_637_55],
            1e-7,
        );
        close(linear_srgb_to_display_p3([1.0; 3]), [1.0; 3], 1e-7);
        close(linear_display_p3_to_srgb([1.0; 3]), [1.0; 3], 1e-7);
    }

    #[test]
    fn linear_srgb_display_p3_round_trips_without_gamut_clipping() {
        for red in 0..=8 {
            for green in 0..=8 {
                for blue in 0..=8 {
                    let p3 = [red as f32 / 8.0, green as f32 / 8.0, blue as f32 / 8.0];
                    close(
                        linear_srgb_to_display_p3(linear_display_p3_to_srgb(p3)),
                        p3,
                        2e-7,
                    );
                }
            }
        }
        let saturated = linear_display_p3_to_srgb([1.0, 0.0, 0.0]);
        assert!(saturated[0] > 1.0 && saturated[1] < 0.0);
    }

    #[test]
    fn premultiplied_alpha_round_trips_and_zero_alpha_is_canonical() {
        for alpha in [0.00001, 0.1, 0.5, 1.0] {
            let straight = [0.1, 0.5, 0.9, alpha];
            close(unpremultiply(premultiply(straight)), straight, 1e-7);
            let extended = [-0.1, 1.2, 0.5, alpha];
            close(unpremultiply(premultiply(extended)), extended, 1e-7);
        }
        assert_eq!(premultiply([1.0, 2.0, 3.0, 0.0]), [0.0; 4]);
        assert_eq!(unpremultiply([1.0, 2.0, 3.0, 0.0]), [0.0; 4]);
        assert_eq!(
            premultiply([f32::NAN, 1.0, f32::INFINITY, f32::NAN]),
            [0.0; 4]
        );
    }

    #[test]
    fn rgba8_srgb_round_trips_all_channel_values_at_multiple_alpha_levels() {
        for alpha in [1, 64, 128, 255] {
            for channel in 0..=255 {
                let encoded = [channel, 255 - channel, 123, alpha];
                assert_eq!(
                    linear_premultiplied_to_rgba8_srgb(rgba8_srgb_to_linear_premultiplied(encoded)),
                    encoded
                );
            }
        }
        assert_eq!(
            linear_premultiplied_to_rgba8_srgb([0.9, 0.8, 0.7, 0.0]),
            [0; 4]
        );
    }

    #[test]
    fn encoded_display_p3_round_trip_preserves_srgb_colors_and_alpha() {
        for alpha in [0.1, 0.5, 1.0] {
            for rgb in [[0.0, 0.5, 1.0], [1.0, 0.0, 0.0], [0.25, 0.3, 0.4]] {
                let pixel = premultiply([rgb[0], rgb[1], rgb[2], alpha]);
                close(
                    display_p3_to_linear_premultiplied_srgb(
                        linear_premultiplied_srgb_to_display_p3(pixel),
                    ),
                    pixel,
                    4e-7,
                );
            }
        }
        assert_eq!(
            linear_premultiplied_srgb_to_display_p3([1.0, 1.0, 1.0, 0.0]),
            [0.0; 4]
        );
    }
}
