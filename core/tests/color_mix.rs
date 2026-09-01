//! E2E tests for `color-mix()`.
//!
//! The expected values match what a browser (Chrome) computes. The unit tests for the mixing
//! itself are in `core/src/style/color_mix.rs`; here we look at the path from CSS syntax
//! through the cascade down to the computed style.

use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

fn first_div(dom: &Dom, id: NodeId) -> Option<NodeId> {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == "div" {
            return Some(id);
        }
    }
    dom.children(id).find_map(|c| first_div(dom, c))
}

/// Return the `div`'s `color` as `(r, g, b, alpha)`.
fn color_of(value: &str) -> (u8, u8, u8, f32) {
    color_of_with(&format!("div {{ color: {value} }}"))
}

fn color_of_with(css: &str) -> (u8, u8, u8, f32) {
    let dom = html::parse(b"<body><div>x</div></body>");
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let div = first_div(&dom, dom.document()).expect("no div");
    let c = styles.get(&div).expect("no style").color;
    (c.red, c.green, c.blue, c.alpha)
}

/// The colour when the declaration was dropped (the inherited initial value).
const DROPPED: (u8, u8, u8, f32) = (0, 0, 0, 1.0);

#[test]
fn mixes_in_srgb() {
    assert_eq!(
        color_of("color-mix(in srgb, red, blue)"),
        (128, 0, 128, 1.0)
    );
}

#[test]
fn a_percentage_shifts_the_balance() {
    assert_eq!(
        color_of("color-mix(in srgb, red 25%, blue)"),
        (64, 0, 191, 1.0)
    );
    // With only one written, the other takes the remainder.
    assert_eq!(
        color_of("color-mix(in srgb, red, blue 25%)"),
        (191, 0, 64, 1.0)
    );
}

/// The percentage may be written before the colour.
#[test]
fn a_percentage_may_come_before_the_color() {
    assert_eq!(
        color_of("color-mix(in srgb, 25% red, blue)"),
        (64, 0, 191, 1.0)
    );
}

/// When they add up to over 100%, only the ratio matters.
#[test]
fn weights_over_one_hundred_percent_are_normalised() {
    assert_eq!(
        color_of("color-mix(in srgb, red 50%, blue 150%)"),
        color_of("color-mix(in srgb, red 25%, blue 75%)")
    );
}

/// When they add up to under 100%, the result is transparent by the shortfall.
#[test]
fn weights_under_one_hundred_percent_make_the_result_transparent() {
    assert_eq!(
        color_of("color-mix(in srgb, red 25%, blue 25%)"),
        (128, 0, 128, 0.5)
    );
}

#[test]
fn both_weights_at_zero_is_invalid() {
    assert_eq!(color_of("color-mix(in srgb, red 0%, blue 0%)"), DROPPED);
}

/// In a perceptually uniform colour space the result differs from the arithmetic mean in sRGB.
#[test]
fn perceptual_spaces_give_a_different_midpoint() {
    let srgb = color_of("color-mix(in srgb, white, black)");
    let lab = color_of("color-mix(in lab, white, black)");
    assert_eq!(srgb, (128, 128, 128, 1.0));
    assert_eq!(lab, (119, 119, 119, 1.0));
}

#[test]
fn supports_the_polar_spaces() {
    // The midpoint of red (0 degrees) and blue (240) takes the shorter arc to 300 (magenta).
    assert_eq!(color_of("color-mix(in hsl, red, blue)"), (255, 0, 255, 1.0));
    assert_eq!(
        color_of("color-mix(in hsl longer hue, red, blue)"),
        (0, 255, 0, 1.0)
    );
}

#[test]
fn alpha_is_premultiplied() {
    assert_eq!(
        color_of("color-mix(in srgb, rgba(255, 0, 0, 0.5), blue)"),
        (85, 0, 170, 0.75)
    );
}

#[test]
fn color_mix_can_be_nested() {
    // The inside is purple (128, 0, 128). This is its midpoint with white.
    assert_eq!(
        color_of("color-mix(in srgb, color-mix(in srgb, red, blue), white)"),
        (192, 128, 192, 1.0)
    );
}

#[test]
fn works_for_other_color_properties() {
    let css = "div { background-color: color-mix(in srgb, red, blue) }";
    let dom = html::parse(b"<body><div>x</div></body>");
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let div = first_div(&dom, dom.document()).unwrap();
    let bg = styles.get(&div).unwrap().background_color;
    assert_eq!((bg.red, bg.green, bg.blue), (128, 0, 128));
}

// ===== Invalid forms =====

/// A colour space wider than sRGB means nothing with DeviceRGB as the destination, so it is unsupported.
#[test]
fn wide_gamut_spaces_are_not_supported() {
    for space in ["display-p3", "a98-rgb", "prophoto-rgb", "rec2020"] {
        assert_eq!(
            color_of(&format!("color-mix(in {space}, red, blue)")),
            DROPPED,
            "{space}"
        );
    }
}

#[test]
fn an_unknown_color_space_is_invalid() {
    assert_eq!(color_of("color-mix(in bogus, red, blue)"), DROPPED);
}

/// `currentcolor` is resolved after the cascade, so it cannot be mixed at this point.
#[test]
fn currentcolor_as_an_operand_is_not_supported() {
    assert_eq!(color_of("color-mix(in srgb, currentcolor, blue)"), DROPPED);
}

#[test]
fn malformed_syntax_is_invalid() {
    for value in [
        "color-mix(red, blue)",                     // no `in <space>`
        "color-mix(in srgb, red)",                  // only one colour
        "color-mix(in srgb red, blue)",             // no comma
        "color-mix(in srgb, red -10%, blue)",       // a negative percentage
        "color-mix(in oklch bogus hue, red, blue)", // an unknown hue interpolation
    ] {
        assert_eq!(color_of(value), DROPPED, "{value}");
    }
}

/// Even when the declaration is dropped, the other declarations in the same rule and the rules that follow survive.
#[test]
fn an_invalid_color_mix_only_drops_its_own_declaration() {
    let css = "div { color: color-mix(in bogus, red, blue); background-color: red }";
    let dom = html::parse(b"<body><div>x</div></body>");
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let div = first_div(&dom, dom.document()).unwrap();
    let style = styles.get(&div).unwrap();
    assert_eq!(
        (
            style.background_color.red,
            style.background_color.green,
            style.background_color.blue
        ),
        (255, 0, 0)
    );
}
