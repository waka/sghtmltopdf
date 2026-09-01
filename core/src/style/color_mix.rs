//! Colour mixing for `color-mix()` (CSS Color 5 section 3; the interpolation rules are CSS Color 4 section 12).
//!
//! Reading the syntax is [`super::properties`]'s job; this module only takes the colour
//! space, the hue direction, the two colours and their weights, and mixes them.
//!
//! The destination is PDF's DeviceRGB, so the result is always converted back to sRGB.

use palette::{FromColor, Hsl, Hwb, Lab, LinSrgb, Oklab, Oklch, Srgb, Xyz};

/// The colour space used for interpolation.
///
/// `display-p3`/`a98-rgb`/`prophoto-rgb`/`rec2020` are not supported: we cannot handle a
/// gamut wider than sRGB, so accepting them would only round the result back to sRGB and
/// not mean what was written. As the spec says, they are treated as an invalid `<color-space>` and the declaration is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Space {
    Srgb,
    SrgbLinear,
    Lab,
    Oklab,
    Xyz,
    Hsl,
    Hwb,
    Lch,
    Oklch,
}

impl Space {
    pub(super) fn parse(ident: &str) -> Option<Self> {
        Some(match ident.to_ascii_lowercase().as_str() {
            "srgb" => Self::Srgb,
            "srgb-linear" => Self::SrgbLinear,
            "lab" => Self::Lab,
            "oklab" => Self::Oklab,
            // CSS `xyz` is an alias for `xyz-d65`.
            "xyz" | "xyz-d65" => Self::Xyz,
            "hsl" => Self::Hsl,
            "hwb" => Self::Hwb,
            "lch" => Self::Lch,
            "oklch" => Self::Oklch,
            _ => return None,
        })
    }

    /// Whether this is a polar colour space with a hue. If so, return which component index is the hue.
    fn hue_index(self) -> Option<usize> {
        match self {
            // In palette's `Hsl`/`Hwb` the hue comes first.
            Self::Hsl | Self::Hwb => Some(0),
            // In `Lch`/`Oklch` it comes after lightness and chroma.
            Self::Lch | Self::Oklch => Some(2),
            _ => None,
        }
    }

    /// From sRGB to this colour space's components (hue in degrees).
    fn components_of(self, c: Srgb) -> [f32; 3] {
        match self {
            Self::Srgb => [c.red, c.green, c.blue],
            Self::SrgbLinear => {
                let v = LinSrgb::from_color(c);
                [v.red, v.green, v.blue]
            }
            Self::Lab => {
                let v = Lab::from_color(c);
                [v.l, v.a, v.b]
            }
            Self::Oklab => {
                let v = Oklab::from_color(c);
                [v.l, v.a, v.b]
            }
            Self::Xyz => {
                let v = Xyz::from_color(c);
                [v.x, v.y, v.z]
            }
            Self::Hsl => {
                let v = Hsl::from_color(c);
                [v.hue.into_degrees(), v.saturation, v.lightness]
            }
            Self::Hwb => {
                let v = Hwb::from_color(c);
                [v.hue.into_degrees(), v.whiteness, v.blackness]
            }
            Self::Lch => {
                let v = palette::Lch::from_color(c);
                [v.l, v.chroma, v.hue.into_degrees()]
            }
            Self::Oklch => {
                let v = Oklch::from_color(c);
                [v.l, v.chroma, v.hue.into_degrees()]
            }
        }
    }

    /// From this colour space's components back to sRGB.
    fn to_srgb(self, v: [f32; 3]) -> Srgb {
        match self {
            Self::Srgb => Srgb::new(v[0], v[1], v[2]),
            Self::SrgbLinear => Srgb::from_color(LinSrgb::new(v[0], v[1], v[2])),
            Self::Lab => Srgb::from_color(Lab::new(v[0], v[1], v[2])),
            Self::Oklab => Srgb::from_color(Oklab::new(v[0], v[1], v[2])),
            Self::Xyz => Srgb::from_color(Xyz::new(v[0], v[1], v[2])),
            Self::Hsl => Srgb::from_color(Hsl::new(v[0], v[1], v[2])),
            Self::Hwb => Srgb::from_color(Hwb::new(v[0], v[1], v[2])),
            Self::Lch => Srgb::from_color(palette::Lch::new(v[0], v[1], v[2])),
            Self::Oklch => Srgb::from_color(Oklch::new(v[0], v[1], v[2])),
        }
    }
}

/// Which way round the hue is interpolated (CSS Color 4 section 12.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum HueMethod {
    /// The initial value. Takes the shorter arc.
    #[default]
    Shorter,
    /// Takes the longer arc.
    Longer,
    /// Goes in the direction of increasing hue.
    Increasing,
    /// Goes in the direction of decreasing hue.
    Decreasing,
}

impl HueMethod {
    pub(super) fn parse(ident: &str) -> Option<Self> {
        Some(match ident.to_ascii_lowercase().as_str() {
            "shorter" => Self::Shorter,
            "longer" => Self::Longer,
            "increasing" => Self::Increasing,
            "decreasing" => Self::Decreasing,
            _ => return None,
        })
    }

    /// Adjust the pair of hues before interpolating. Linearly interpolating what this
    /// returns gives the requested direction.
    fn adjust(self, h1: f32, h2: f32) -> (f32, f32) {
        let (h1, h2) = (normalize_hue(h1), normalize_hue(h2));
        let diff = h2 - h1;
        match self {
            Self::Shorter => {
                if diff > 180.0 {
                    (h1 + 360.0, h2)
                } else if diff < -180.0 {
                    (h1, h2 + 360.0)
                } else {
                    (h1, h2)
                }
            }
            Self::Longer => {
                if (0.0..180.0).contains(&diff) {
                    (h1 + 360.0, h2)
                } else if diff > -180.0 && diff <= 0.0 {
                    (h1, h2 + 360.0)
                } else {
                    (h1, h2)
                }
            }
            Self::Increasing => {
                if h2 < h1 {
                    (h1, h2 + 360.0)
                } else {
                    (h1, h2)
                }
            }
            Self::Decreasing => {
                if h1 < h2 {
                    (h1 + 360.0, h2)
                } else {
                    (h1, h2)
                }
            }
        }
    }
}

/// Bring degrees into `[0, 360)`.
fn normalize_hue(h: f32) -> f32 {
    let h = h % 360.0;
    if h < 0.0 {
        h + 360.0
    } else {
        h
    }
}

/// A colour and alpha expressed as sRGB values from 0.0 to 1.0.
pub(super) type UnitRgba = (f32, f32, f32, f32);

/// Mix two colours in `space` and return sRGB values from 0.0 to 1.0. `w1` + `w2` must be
/// 1.0 (the caller normalises the weights).
///
/// Alpha is premultiplied before interpolating (CSS Color 4 section 12.3). Only the hue is
/// exempt from the multiplication.
pub(super) fn mix(
    space: Space,
    hue: HueMethod,
    c1: UnitRgba,
    w1: f32,
    c2: UnitRgba,
    w2: f32,
) -> UnitRgba {
    let alpha = c1.3 * w1 + c2.3 * w2;

    let mut v1 = space.components_of(Srgb::new(c1.0, c1.1, c1.2));
    let mut v2 = space.components_of(Srgb::new(c2.0, c2.1, c2.2));

    let hue_index = space.hue_index();
    if let Some(i) = hue_index {
        let (h1, h2) = hue.adjust(v1[i], v2[i]);
        v1[i] = h1;
        v2[i] = h2;
    }
    for i in 0..3 {
        if Some(i) == hue_index {
            continue;
        }
        v1[i] *= c1.3;
        v2[i] *= c2.3;
    }

    let mut mixed = [0.0f32; 3];
    for i in 0..3 {
        mixed[i] = v1[i] * w1 + v2[i] * w2;
        if Some(i) != hue_index && alpha != 0.0 {
            // Undo the premultiplication.
            mixed[i] /= alpha;
        }
    }

    let srgb = space.to_srgb(mixed);
    (srgb.red, srgb.green, srgb.blue, alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: UnitRgba = (1.0, 0.0, 0.0, 1.0);
    const BLUE: UnitRgba = (0.0, 0.0, 1.0, 1.0);

    fn to_u8(v: f32) -> u8 {
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    fn mixed_rgb(space: Space, hue: HueMethod, a: UnitRgba, b: UnitRgba) -> (u8, u8, u8) {
        let (r, g, bl, _) = mix(space, hue, a, 0.5, b, 0.5);
        (to_u8(r), to_u8(g), to_u8(bl))
    }

    #[test]
    fn srgb_midpoint_is_the_arithmetic_mean() {
        assert_eq!(
            mixed_rgb(Space::Srgb, HueMethod::Shorter, RED, BLUE),
            (128, 0, 128)
        );
    }

    /// `hsl` goes round the hue circle, so the midpoint of red (0 degrees) and blue (240)
    /// takes the shorter arc to 300 (magenta), not the arithmetic mean of 120 (green).
    #[test]
    fn a_polar_space_takes_the_shorter_arc_by_default() {
        assert_eq!(
            mixed_rgb(Space::Hsl, HueMethod::Shorter, RED, BLUE),
            (255, 0, 255)
        );
    }

    /// With `longer hue` it goes the other way round and the midpoint is 120 (green).
    #[test]
    fn longer_hue_takes_the_other_arc() {
        assert_eq!(
            mixed_rgb(Space::Hsl, HueMethod::Longer, RED, BLUE),
            (0, 255, 0)
        );
    }

    #[test]
    fn increasing_and_decreasing_hue_pick_a_direction() {
        // Increasing from 0 to 240 gives 120 (green).
        assert_eq!(
            mixed_rgb(Space::Hsl, HueMethod::Increasing, RED, BLUE),
            (0, 255, 0)
        );
        // Decreasing gives 300 (magenta).
        assert_eq!(
            mixed_rgb(Space::Hsl, HueMethod::Decreasing, RED, BLUE),
            (255, 0, 255)
        );
    }

    /// Mixing colours with different alphas premultiplies. The more transparent colour
    /// should show through faintly (a plain average would make it too strong).
    #[test]
    fn alpha_is_premultiplied_before_interpolating() {
        let half_red = (1.0, 0.0, 0.0, 0.5);
        let (r, g, b, a) = mix(Space::Srgb, HueMethod::Shorter, half_red, 0.5, BLUE, 0.5);
        assert_eq!((to_u8(r), to_u8(g), to_u8(b)), (85, 0, 170));
        assert!((a - 0.75).abs() < 1e-6, "alpha={a}");
    }

    #[test]
    fn lab_midpoint_of_white_and_black_is_perceptual_grey() {
        let white = (1.0, 1.0, 1.0, 1.0);
        let black = (0.0, 0.0, 0.0, 1.0);
        // Grey at L=50. Darker than the arithmetic mean in sRGB (128).
        assert_eq!(
            mixed_rgb(Space::Lab, HueMethod::Shorter, white, black),
            (119, 119, 119)
        );
    }

    #[test]
    fn unknown_color_spaces_are_rejected() {
        assert!(Space::parse("display-p3").is_none());
        assert!(Space::parse("rec2020").is_none());
        assert_eq!(Space::parse("OKLCH"), Some(Space::Oklch));
        assert_eq!(Space::parse("xyz-d65"), Some(Space::Xyz));
    }
}
