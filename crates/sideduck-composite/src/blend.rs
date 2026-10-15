//! Scalar reference blending in linear sRGB with premultiplied alpha.
//!
//! The separable and nonseparable functions follow W3C Compositing and Blending
//! Level 1, sections 6 and 10: <https://www.w3.org/TR/compositing-1/#blending>.
//! Blending operates on straight RGB; source-over compositing returns premultiplied
//! RGB. Add and Subtract are clamped channel addition and backdrop-minus-source,
//! respectively, using the same source-over coverage equation as the other modes.
//!
//! This SDR reference sanitizes alpha/opacity to [0, 1] and premultiplied RGB to
//! [0, alpha]. Nonfinite components become zero. Zero-alpha pixels have zero RGB.
//! Transfer curves and gamut conversion are separate operations in `color`.

use serde::{Deserialize, Serialize};

/// A linear-sRGB premultiplied RGBA pixel.
pub type Pixel = [f32; 4];

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[repr(u32)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default]
    Normal = 0,
    Multiply = 1,
    Screen = 2,
    Overlay = 3,
    Darken = 4,
    Lighten = 5,
    ColorDodge = 6,
    ColorBurn = 7,
    HardLight = 8,
    SoftLight = 9,
    Difference = 10,
    Exclusion = 11,
    Hue = 12,
    Saturation = 13,
    Color = 14,
    Luminosity = 15,
    Add = 16,
    Subtract = 17,
}

impl BlendMode {
    pub const ALL: [Self; 18] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::Darken,
        Self::Lighten,
        Self::ColorDodge,
        Self::ColorBurn,
        Self::HardLight,
        Self::SoftLight,
        Self::Difference,
        Self::Exclusion,
        Self::Hue,
        Self::Saturation,
        Self::Color,
        Self::Luminosity,
        Self::Add,
        Self::Subtract,
    ];

    /// Only unmasked, unclipped normal layers can form independent above-stack runs.
    /// Layer/group constraints must be checked separately by the run compiler.
    pub const fn is_backdrop_independent(self) -> bool {
        matches!(self, Self::Normal)
    }
}

pub const ALL: [BlendMode; 18] = BlendMode::ALL;

/// Apply source opacity once, then blend source over destination.
/// Inputs and output use premultiplied linear-sRGB components.
pub fn blend(dst: Pixel, src: Pixel, mode: BlendMode, opacity: f32) -> Pixel {
    let dst = sanitize(dst);
    let src = sanitize(src);
    let opacity = unit(opacity);
    let source_alpha = src[3] * opacity;
    let backdrop_alpha = dst[3];
    if source_alpha == 0.0 {
        return dst;
    }
    if backdrop_alpha == 0.0 {
        return [
            src[0] * opacity,
            src[1] * opacity,
            src[2] * opacity,
            source_alpha,
        ];
    }
    let alpha = source_alpha + backdrop_alpha * (1.0 - source_alpha);
    let mut output = [0.0, 0.0, 0.0, alpha];
    if mode == BlendMode::Normal {
        for channel in 0..3 {
            output[channel] =
                (src[channel] * opacity + dst[channel] * (1.0 - source_alpha)).clamp(0.0, alpha);
        }
        return output;
    }
    let backdrop = [
        dst[0] / backdrop_alpha,
        dst[1] / backdrop_alpha,
        dst[2] / backdrop_alpha,
    ];
    let source = [src[0] / src[3], src[1] / src[3], src[2] / src[3]];
    let mixed = match mode {
        BlendMode::Hue => set_lum(set_sat(source, sat(backdrop)), lum(backdrop)),
        BlendMode::Saturation => set_lum(set_sat(backdrop, sat(source)), lum(backdrop)),
        BlendMode::Color => set_lum(source, lum(backdrop)),
        BlendMode::Luminosity => set_lum(backdrop, lum(source)),
        _ => std::array::from_fn(|channel| blend_channel(backdrop[channel], source[channel], mode)),
    };
    for channel in 0..3 {
        output[channel] = (dst[channel] * (1.0 - source_alpha)
            + src[channel] * opacity * (1.0 - backdrop_alpha)
            + source_alpha * backdrop_alpha * mixed[channel])
            .clamp(0.0, alpha);
    }
    output
}

/// Slow, explicit bottom-to-top single-pixel stack reference, starting transparent.
/// Each tuple is a premultiplied pixel, its blend mode, and its layer opacity.
pub fn composite_stack(layers: impl IntoIterator<Item = (Pixel, BlendMode, f32)>) -> Pixel {
    layers
        .into_iter()
        .fold([0.0; 4], |backdrop, (source, mode, opacity)| {
            blend(backdrop, source, mode, opacity)
        })
}

pub(crate) fn unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

pub(crate) fn sanitize(pixel: Pixel) -> Pixel {
    let alpha = unit(pixel[3]);
    [
        unit(pixel[0]).min(alpha),
        unit(pixel[1]).min(alpha),
        unit(pixel[2]).min(alpha),
        alpha,
    ]
}

fn blend_channel(backdrop: f32, source: f32, mode: BlendMode) -> f32 {
    match mode {
        BlendMode::Normal => source,
        BlendMode::Multiply => backdrop * source,
        BlendMode::Screen => backdrop + source - backdrop * source,
        BlendMode::Overlay => hard_light(source, backdrop),
        BlendMode::Darken => backdrop.min(source),
        BlendMode::Lighten => backdrop.max(source),
        BlendMode::ColorDodge => {
            if backdrop == 0.0 {
                0.0
            } else if source == 1.0 {
                1.0
            } else {
                (backdrop / (1.0 - source)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if backdrop == 1.0 {
                1.0
            } else if source == 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - backdrop) / source).min(1.0)
            }
        }
        BlendMode::HardLight => hard_light(backdrop, source),
        BlendMode::SoftLight => {
            if source <= 0.5 {
                backdrop - (1.0 - 2.0 * source) * backdrop * (1.0 - backdrop)
            } else {
                let d = if backdrop <= 0.25 {
                    ((16.0 * backdrop - 12.0) * backdrop + 4.0) * backdrop
                } else {
                    backdrop.sqrt()
                };
                backdrop + (2.0 * source - 1.0) * (d - backdrop)
            }
        }
        BlendMode::Difference => (backdrop - source).abs(),
        BlendMode::Exclusion => backdrop + source - 2.0 * backdrop * source,
        BlendMode::Add => (backdrop + source).min(1.0),
        BlendMode::Subtract => (backdrop - source).max(0.0),
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            unreachable!("nonseparable modes require all three channels")
        }
    }
}

fn hard_light(backdrop: f32, source: f32) -> f32 {
    if source <= 0.5 {
        2.0 * backdrop * source
    } else {
        1.0 - 2.0 * (1.0 - backdrop) * (1.0 - source)
    }
}

fn lum(rgb: [f32; 3]) -> f32 {
    // These are the blending model's coefficients, not the sRGB-to-XYZ Y row.
    0.3 * rgb[0] + 0.59 * rgb[1] + 0.11 * rgb[2]
}

fn sat(rgb: [f32; 3]) -> f32 {
    rgb[0].max(rgb[1]).max(rgb[2]) - rgb[0].min(rgb[1]).min(rgb[2])
}

fn set_sat(rgb: [f32; 3], saturation: f32) -> [f32; 3] {
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    if maximum > minimum {
        // Equivalent to setting min=0, max=s, mid=(mid-min)*s/(max-min).
        // Equal components stay equal, avoiding tie-dependent channel sorting.
        rgb.map(|channel| (channel - minimum) * saturation / (maximum - minimum))
    } else {
        [0.0; 3]
    }
}

fn set_lum(rgb: [f32; 3], luminosity: f32) -> [f32; 3] {
    let delta = luminosity - lum(rgb);
    clip_color(rgb.map(|channel| channel + delta))
}

fn clip_color(mut rgb: [f32; 3]) -> [f32; 3] {
    let luminosity = lum(rgb);
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    if minimum < 0.0 {
        let denominator = luminosity - minimum;
        rgb = if denominator > 0.0 {
            rgb.map(|channel| luminosity + (channel - luminosity) * luminosity / denominator)
        } else {
            [0.0; 3]
        };
    }
    // W3C retains the original maximum/luminosity for this second condition.
    if maximum > 1.0 {
        let denominator = maximum - luminosity;
        rgb = if denominator > 0.0 {
            rgb.map(|channel| {
                luminosity + (channel - luminosity) * (1.0 - luminosity) / denominator
            })
        } else {
            [1.0; 3]
        };
    }
    rgb.map(unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: Pixel, expected: Pixel, tolerance: f32) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= tolerance,
                "channel {channel}: {actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn every_blend_mode_matches_independently_calculated_opaque_golden() {
        // Hand-calculated from the stated equations for two nonsymmetric colors.
        // Expected values are fixed numerical fixtures, not another call to helpers.
        let expected = [
            [0.6, 0.3, 0.1],
            [0.12, 0.15, 0.08],
            [0.68, 0.65, 0.82],
            [0.24, 0.3, 0.64],
            [0.2, 0.3, 0.1],
            [0.6, 0.5, 0.8],
            [0.5, 5.0 / 7.0, 8.0 / 9.0],
            [0.0, 0.0, 0.0],
            [0.36, 0.3, 0.16],
            [0.2496, 0.4, 0.672],
            [0.4, 0.2, 0.7],
            [0.56, 0.5, 0.74],
            [0.7214, 0.3614, 0.1214],
            [0.2405, 0.4905, 0.7405],
            [0.675, 0.375, 0.175],
            [0.125, 0.425, 0.725],
            [0.8, 0.8, 0.9],
            [0.0, 0.2, 0.7],
        ];
        for (mode, rgb) in ALL.into_iter().zip(expected) {
            close(
                blend([0.2, 0.5, 0.8, 1.0], [0.6, 0.3, 0.1, 1.0], mode, 1.0),
                [rgb[0], rgb[1], rgb[2], 1.0],
                2e-6,
            );
        }
    }

    #[test]
    fn premultiplied_alpha_and_layer_opacity_match_numeric_reference() {
        let dst = [0.1, 0.25, 0.4, 0.5];
        let src = [0.3, 0.15, 0.05, 0.5];
        close(
            blend(dst, src, BlendMode::Normal, 0.5),
            [0.225, 0.2625, 0.325, 0.625],
            1e-7,
        );
        close(
            blend(dst, src, BlendMode::Multiply, 0.5),
            [0.165, 0.24375, 0.3225, 0.625],
            1e-7,
        );
    }

    #[test]
    fn color_dodge_burn_and_soft_light_boundary_branches_match_reference() {
        close(
            blend(
                [0.0, 0.25, 1.0, 1.0],
                [1.0, 1.0, 0.0, 1.0],
                BlendMode::ColorDodge,
                1.0,
            ),
            [0.0, 1.0, 1.0, 1.0],
            0.0,
        );
        close(
            blend(
                [1.0, 0.75, 0.0, 1.0],
                [0.0, 0.5, 0.0, 1.0],
                BlendMode::ColorBurn,
                1.0,
            ),
            [1.0, 0.5, 0.0, 1.0],
            0.0,
        );
        close(
            blend(
                [0.25, 0.81, 0.5, 1.0],
                [1.0, 1.0, 0.5, 1.0],
                BlendMode::SoftLight,
                1.0,
            ),
            [0.5, 0.9, 0.5, 1.0],
            1e-7,
        );
    }

    #[test]
    fn nonseparable_modes_clip_extreme_colors_preserving_luminosity() {
        // Red's model luminosity is .3. Colorizing green (.59) with red must
        // clip its >1 red channel while preserving the target luminosity.
        close(
            blend(
                [0.0, 1.0, 0.0, 1.0],
                [1.0, 0.0, 0.0, 1.0],
                BlendMode::Color,
                1.0,
            ),
            [1.0, 29.0 / 70.0, 29.0 / 70.0, 1.0],
            2e-7,
        );
        close(
            blend(
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                BlendMode::Color,
                1.0,
            ),
            [0.0, 30.0 / 59.0, 0.0, 1.0],
            2e-7,
        );
        close(
            blend(
                [0.4, 0.4, 0.4, 1.0],
                [1.0, 0.0, 0.0, 1.0],
                BlendMode::Saturation,
                1.0,
            ),
            [0.4, 0.4, 0.4, 1.0],
            1e-7,
        );
    }

    #[test]
    fn transparent_pixels_zero_opacity_and_nonfinite_inputs_are_defined() {
        for mode in ALL {
            let dst = [0.1, 0.3, 0.2, 0.5];
            let src = [0.2, 0.1, 0.3, 0.4];
            assert_eq!(blend(dst, src, mode, 0.0), dst);
            assert_eq!(blend(dst, [1.0, 1.0, 1.0, 0.0], mode, 1.0), dst);
            assert_eq!(
                blend([1.0, 1.0, 1.0, 0.0], src, mode, 0.5),
                [0.1, 0.05, 0.15, 0.2]
            );
            let actual = blend(
                [f32::NAN, -1.0, f32::INFINITY, 2.0],
                [0.1, 0.1, 0.1, f32::NAN],
                mode,
                1.0,
            );
            assert_eq!(actual, [0.0, 0.0, 0.0, 1.0]);
            assert_eq!(blend(dst, src, mode, f32::INFINITY), dst);
        }
    }

    #[test]
    fn randomized_every_mode_preserves_premultiplied_bounds_and_transparent_identity() {
        let mut seed = 0x71f0_83b9_8325_4cd1_u64;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / 16_777_215.0
        };
        for _ in 0..1000 {
            let da = random();
            let sa = random();
            let dst = [random() * da, random() * da, random() * da, da];
            let src = [random() * sa, random() * sa, random() * sa, sa];
            let opacity = random();
            for mode in ALL {
                let result = blend(dst, src, mode, opacity);
                assert!(result.into_iter().all(f32::is_finite));
                assert!((0.0..=1.0).contains(&result[3]));
                assert!(result[..3].iter().all(|&v| v >= 0.0 && v <= result[3]));
                let expected_alpha = 1.0 - (1.0 - da) * (1.0 - sa * opacity);
                assert!((result[3] - expected_alpha).abs() < 2e-7);
                assert_eq!(blend(dst, [0.0; 4], mode, opacity), dst);
            }
        }
    }

    #[test]
    fn scalar_reference_stack_matches_known_three_layer_source_over() {
        let result = composite_stack([
            ([0.5, 0.0, 0.0, 0.5], BlendMode::Normal, 1.0),
            ([0.0, 0.5, 0.0, 0.5], BlendMode::Normal, 1.0),
            ([0.0, 0.0, 0.5, 0.5], BlendMode::Normal, 1.0),
        ]);
        assert_eq!(result, [0.125, 0.25, 0.5, 0.875]);
        assert_eq!(composite_stack([]), [0.0; 4]);
        assert_eq!(
            ALL.iter()
                .filter(|mode| mode.is_backdrop_independent())
                .count(),
            1
        );
    }
}
