//! The CSS-wide keywords `inherit`, `initial` and `unset` on ordinary properties, checked on
//! the computed style.

use sghtmltopdf::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf::style::{
    compute_styles, parse_stylesheet, user_agent_stylesheet, BoxSizing, ComputedStyle, Display,
    FontWeight, LengthPercentageOrAuto, RgbaColor, TextAlign, WhiteSpace,
};

fn find_by_id(dom: &Dom, node: NodeId, id: &str) -> Option<NodeId> {
    if let NodeData::Element { attrs, .. } = &dom.node(node).data {
        if attrs
            .iter()
            .any(|a| &*a.name.local == "id" && &*a.value == id)
        {
            return Some(node);
        }
    }
    dom.children(node)
        .find_map(|child| find_by_id(dom, child, id))
}

/// The computed styles of the elements with the given ids, in order.
fn styles_of<const N: usize>(body: &str, css: &str, ids: [&str; N]) -> [ComputedStyle; N] {
    let dom = html::parse(body.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    ids.map(|id| {
        let node = find_by_id(&dom, dom.document(), id).unwrap_or_else(|| panic!("no #{id}"));
        (*styles[&node]).clone()
    })
}

const RED: RgbaColor = RgbaColor {
    red: 255,
    green: 0,
    blue: 0,
    alpha: 1.0,
};
const BLUE: RgbaColor = RgbaColor {
    red: 0,
    green: 0,
    blue: 255,
    alpha: 1.0,
};

const NESTED: &str = r#"<div id="p"><div id="c">x</div></div>"#;

#[test]
fn inherit_takes_the_parents_value_of_a_non_inherited_property() {
    // The report's case.
    let [c] = styles_of(
        NESTED,
        "#p { box-sizing: border-box; } #c { box-sizing: inherit; width: 300px; padding: 50px; }",
        ["c"],
    );
    assert_eq!(c.box_sizing, BoxSizing::BorderBox);

    // Without the parent setting it, the parent's value is the initial one.
    let [c] = styles_of(NESTED, "#c { box-sizing: inherit; }", ["c"]);
    assert_eq!(c.box_sizing, BoxSizing::ContentBox);
}

#[test]
fn the_box_sizing_reset_reaches_every_descendant() {
    // html { border-box } with * { inherit }, as in Bootstrap's reboot.
    let [a, b, c] = styles_of(
        r#"<section id="a"><p id="b"><span id="c">x</span></p></section>"#,
        "html { box-sizing: border-box; } *, *::before, *::after { box-sizing: inherit; }",
        ["a", "b", "c"],
    );
    for style in [a, b, c] {
        assert_eq!(style.box_sizing, BoxSizing::BorderBox);
    }

    // A component that opts out passes its own value down.
    let [b, c] = styles_of(
        r#"<section id="a"><p id="b"><span id="c">x</span></p></section>"#,
        "html { box-sizing: border-box; } * { box-sizing: inherit; } #b { box-sizing: content-box; }",
        ["b", "c"],
    );
    assert_eq!(b.box_sizing, BoxSizing::ContentBox);
    assert_eq!(c.box_sizing, BoxSizing::ContentBox);
}

#[test]
fn a_keyword_takes_part_in_the_cascade_like_any_other_value() {
    // The keyword wins over an earlier declaration.
    let [c] = styles_of(
        NESTED,
        "#c { box-sizing: border-box; } #c { box-sizing: inherit; }",
        ["c"],
    );
    assert_eq!(c.box_sizing, BoxSizing::ContentBox);

    // A later declaration wins over the keyword.
    let [c] = styles_of(
        NESTED,
        "#p { box-sizing: border-box; } #c { box-sizing: inherit; } #c { box-sizing: content-box; }",
        ["c"],
    );
    assert_eq!(c.box_sizing, BoxSizing::ContentBox);

    // Specificity decides, not the order.
    let [c] = styles_of(
        NESTED,
        "#p { color: red; } #c { color: inherit; } div { color: blue; }",
        ["c"],
    );
    assert_eq!(c.color, RED);

    // The `style` attribute.
    let [c] = styles_of(
        r#"<div id="p"><div id="c" style="box-sizing: inherit">x</div></div>"#,
        "#p { box-sizing: border-box; }",
        ["c"],
    );
    assert_eq!(c.box_sizing, BoxSizing::BorderBox);
}

#[test]
fn inherit_copies_lengths_colours_and_shorthands() {
    let [p, c] = styles_of(
        NESTED,
        "#p { width: 50%; margin: 1px 2px 3px 4px; border: 2em solid red; background: blue; \
              font-size: 10px; } \
         #c { width: inherit; margin: inherit; border: inherit; background: inherit; \
              font-size: 30px; }",
        ["p", "c"],
    );
    // A percentage stays a percentage.
    assert_eq!(c.width, p.width);
    assert_ne!(c.width, LengthPercentageOrAuto::Auto);
    assert_eq!(
        (c.margin_top, c.margin_right, c.margin_bottom, c.margin_left),
        (p.margin_top, p.margin_right, p.margin_bottom, p.margin_left)
    );
    // The computed value is inherited: 2em of the parent's 10px, not of the child's 30px.
    assert_eq!(c.border_left_width.0, 20.0);
    assert_eq!(c.border_top_style, p.border_top_style);
    assert_eq!(c.border_bottom_color, RED);
    assert_eq!(c.background_color, BLUE);
}

#[test]
fn inherit_on_an_inherited_property_overrides_the_elements_own_rules() {
    // The UA sheet colours links and makes `th` bold and centred.
    let [a, th] = styles_of(
        r#"<p id="p"><a id="a" href="x">x</a></p><table><tr><th id="th">h</th></tr></table>"#,
        "#p { color: red; } a { color: inherit; } \
         table { text-align: right; } th { font-weight: inherit; text-align: inherit; }",
        ["a", "th"],
    );
    assert_eq!(a.color, RED);
    assert_eq!(th.font_weight, FontWeight::NORMAL);
    assert_eq!(th.text_align, TextAlign::Right);
}

#[test]
fn initial_puts_an_inherited_property_back_to_its_initial_value() {
    let initial = ComputedStyle::default();
    let [c] = styles_of(
        NESTED,
        "#p { color: red; font-size: 40px; white-space: nowrap; text-align: center; } \
         #c { color: initial; font-size: initial; white-space: initial; text-align: initial; \
              width: 2em; border: 1px solid; }",
        ["c"],
    );
    assert_eq!(c.color, initial.color);
    assert_eq!(c.font_size, initial.font_size);
    assert_eq!(c.white_space, WhiteSpace::Normal);
    assert_eq!(c.text_align, initial.text_align);
    // `em` and `currentcolor` follow the element's own values.
    let [reference] = styles_of(NESTED, "#c { width: 2em; }", ["c"]);
    assert_eq!(c.width, reference.width);
    assert_eq!(c.border_top_color, initial.color);

    // The children inherit the initial value in turn.
    let [g] = styles_of(
        r#"<div id="p"><div id="c"><span id="g">x</span></div></div>"#,
        "#p { color: red; } #c { color: initial; }",
        ["g"],
    );
    assert_eq!(g.color, initial.color);
}

#[test]
fn initial_puts_a_non_inherited_property_back_to_its_initial_value() {
    let [c] = styles_of(
        NESTED,
        "#c { width: 300px; background: red; } #c { display: initial; width: initial; background: initial; }",
        ["c"],
    );
    // The UA sheet makes a `div` a block; the initial value is `inline`.
    assert_eq!(c.display, Display::Inline);
    assert_eq!(c.width, LengthPercentageOrAuto::Auto);
    assert_eq!(c.background_color, RgbaColor::TRANSPARENT);
}

#[test]
fn unset_inherits_or_resets_depending_on_the_property() {
    let [c] = styles_of(
        NESTED,
        "#p { color: red; width: 100px; } #c { color: blue; width: 300px; } \
         #c { color: unset; width: unset; }",
        ["c"],
    );
    assert_eq!(c.color, RED);
    assert_eq!(c.width, LengthPercentageOrAuto::Auto);
}

#[test]
fn a_keyword_can_come_from_a_var_fallback() {
    // A custom property cannot hold a CSS-wide keyword (`--k: inherit` makes `--k` itself
    // inherit), but a `var()` fallback can supply one.
    let [c] = styles_of(
        NESTED,
        "#p { box-sizing: border-box; color: red; } \
         #c { box-sizing: var(--none, inherit); color: blue; } \
         #c { color: var(--none, inherit); }",
        ["c"],
    );
    assert_eq!(c.box_sizing, BoxSizing::BorderBox);
    assert_eq!(c.color, RED);
}

#[test]
fn display_inherit_still_blockifies_a_float() {
    let [c] = styles_of(
        r#"<span id="p"><span id="c">x</span></span>"#,
        "#c { display: inherit; float: left; }",
        ["c"],
    );
    assert_eq!(c.display, Display::Block);
}

#[test]
fn the_root_element_and_unknown_properties_are_handled() {
    // The root has no parent: `inherit` is the initial value.
    let [root] = styles_of(
        r#"<html id="root"><body>x</body></html>"#,
        "html { box-sizing: inherit; color: inherit; nonsense: inherit; }",
        ["root"],
    );
    assert_eq!(root.box_sizing, BoxSizing::ContentBox);
    assert_eq!(root.color, ComputedStyle::default().color);
}

#[test]
fn text_emphasis_color_initial_is_the_elements_own_colour() {
    let [c] = styles_of(
        NESTED,
        "#p { color: red; text-emphasis-color: blue; } #c { color: red; text-emphasis-color: initial; }",
        ["c"],
    );
    assert_eq!(c.text_emphasis_color, RED);
    let [c] = styles_of(
        NESTED,
        "#p { text-emphasis-color: blue; } #c { text-emphasis-color: inherit; }",
        ["c"],
    );
    assert_eq!(c.text_emphasis_color, BLUE);
}

#[test]
fn revert_is_not_supported_and_the_declaration_is_ignored() {
    let [c] = styles_of(NESTED, "#c { color: red; } #c { color: revert; }", ["c"]);
    assert_eq!(c.color, RED);
}

#[test]
fn a_keyword_followed_by_anything_else_is_invalid() {
    let [c] = styles_of(
        NESTED,
        "#p { box-sizing: border-box; } #c { box-sizing: inherit inherit; margin: inherit 0; }",
        ["c"],
    );
    assert_eq!(c.box_sizing, BoxSizing::ContentBox);
}

#[test]
fn a_keyword_on_content_removes_the_generated_box() {
    let [kept, removed] = styles_of(
        r#"<p id="kept">x</p><p id="removed">x</p>"#,
        "p::before { content: 'B'; } #removed::before { content: initial; } \
         *::before { box-sizing: inherit; }",
        ["kept", "removed"],
    );
    assert_eq!(kept.pseudo_before_content.as_deref(), Some("B"));
    assert_eq!(removed.pseudo_before_content, None);
}
