//! E2E tests for CSS Nesting (nested style rules) (waka/sghtmltopdf#25).
//!
//! Substituting `&` and resolving the nesting use the `selectors` crate's implementation, so
//! what is pinned here is that "a nested rule is not discarded and reaches the cascade" and
//! that "specificity and source order are reflected in the cascade as the spec requires".

use std::collections::HashMap;
use std::rc::Rc;

use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::style::{
    compute_styles, parse_stylesheet, user_agent_stylesheet, ComputedStyle, LengthPercentage,
    LengthPercentageOrAuto,
};

/// Find an element by its `id` attribute.
fn find_by_id(dom: &Dom, from: NodeId, id: &str) -> Option<NodeId> {
    if let NodeData::Element { attrs, .. } = &dom.node(from).data {
        if attrs
            .iter()
            .any(|a| &*a.name.local == "id" && &*a.value == id)
        {
            return Some(from);
        }
    }
    dom.children(from).find_map(|c| find_by_id(dom, c, id))
}

fn styles_of(html_src: &str, css: &str) -> (Dom, HashMap<NodeId, Rc<ComputedStyle>>) {
    let dom = html::parse(format!("<body>{html_src}</body>").as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    (dom, styles)
}

fn style_of(html_src: &str, css: &str) -> Rc<ComputedStyle> {
    let (dom, styles) = styles_of(html_src, css);
    let target = find_by_id(&dom, dom.document(), "target").expect("no #target");
    Rc::clone(styles.get(&target).expect("no style"))
}

/// Return `#target`'s `color` in `#rrggbb` form.
fn color_of(html_src: &str, css: &str) -> String {
    let c = style_of(html_src, css).color;
    format!("#{:02x}{:02x}{:02x}", c.red, c.green, c.blue)
}

/// Return `#target`'s `margin-left` in px.
fn margin_left_of(html_src: &str, css: &str) -> f32 {
    match style_of(html_src, css).margin_left {
        LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(px)) => px,
        other => panic!("margin-left is not a length: {other:?}"),
    }
}

const RED: &str = "#ff0000";
const BLACK: &str = "#000000";
const BLUE: &str = "#0000ff";

const NESTED_PROBE: &str = r#"<div class="wrap"><div class="probe" id="target">X</div></div>"#;

// ===== Reproducing issue #25 =====

#[test]
fn flat_control_rule_applies() {
    let css = ".wrap .probe { margin-left: 90px }";
    assert_eq!(margin_left_of(NESTED_PROBE, css), 90.0);
}

#[test]
fn nested_rule_with_explicit_parent_selector_applies() {
    let css = ".wrap { & .probe { margin-left: 90px } }";
    assert_eq!(margin_left_of(NESTED_PROBE, css), 90.0);
}

#[test]
fn nested_rule_with_implicit_parent_selector_applies() {
    let css = ".wrap { .probe { margin-left: 90px } }";
    assert_eq!(margin_left_of(NESTED_PROBE, css), 90.0);
}

#[test]
fn nested_compound_parent_selector_applies() {
    let css = ".wrap { &.probe { margin-left: 90px } }";
    assert_eq!(
        margin_left_of(r#"<div class="wrap probe" id="target">X</div>"#, css),
        90.0
    );
    assert_eq!(
        margin_left_of(NESTED_PROBE, css),
        0.0,
        "`&.probe` is `.wrap.probe`, not `.wrap .probe`"
    );
}

#[test]
fn nested_rule_with_leading_combinator_applies() {
    // `margin-left` is not inherited, so a grandchild never takes the value from its parent.
    let css = ".list { > li { margin-left: 90px } }";
    assert_eq!(
        margin_left_of(r#"<ul class="list"><li id="target">a</li></ul>"#, css),
        90.0
    );
    assert_eq!(
        margin_left_of(
            r#"<ul class="list"><li><ul><li id="target">a</li></ul></li></ul>"#,
            css
        ),
        0.0,
        "a grandchild does not match `> li`"
    );
}

#[test]
fn nested_type_selector_is_not_mistaken_for_a_declaration() {
    // `p {` starts with an ident just as a declaration (`p:`) does, so if it cannot be read
    // as a declaration it has to be re-read as a rule.
    let css = ".wrap { p { color: red } }";
    assert_eq!(
        color_of(r#"<div class="wrap"><p id="target">a</p></div>"#, css),
        RED
    );
}

#[test]
fn nested_pseudo_class_selector_is_not_mistaken_for_a_declaration() {
    // `a:link { }` also looks like the declaration `a: link { }`.
    let css = ".wrap { a:link { color: red } }";
    assert_eq!(
        color_of(
            r##"<div class="wrap"><a href="#" id="target">a</a></div>"##,
            css
        ),
        RED
    );
}

#[test]
fn nesting_can_be_deeper_than_one_level() {
    let css = ".a { .b { .c { color: red } } }";
    assert_eq!(
        color_of(
            r#"<div class="a"><div class="b"><div class="c" id="target">x</div></div></div>"#,
            css
        ),
        RED
    );
    assert_eq!(
        color_of(
            r#"<div class="a"><div class="c" id="target">x</div></div>"#,
            css
        ),
        BLACK,
        "it does not match an element that skipped `.b`"
    );
}

#[test]
fn nested_rule_under_a_selector_list_applies_to_every_parent() {
    let css = ".a, .b { .c { color: red } }";
    assert_eq!(
        color_of(
            r#"<div class="b"><div class="c" id="target">x</div></div>"#,
            css
        ),
        RED
    );
}

#[test]
fn nested_rule_does_not_match_outside_its_parent() {
    let css = ".wrap { .probe { color: red } }";
    assert_eq!(
        color_of(r#"<div class="probe" id="target">X</div>"#, css),
        BLACK
    );
}

// ===== Coexisting with the parent rule's declarations =====

#[test]
fn declarations_before_a_nested_rule_still_apply_to_the_parent() {
    let css = ".wrap { color: red; .probe { margin-left: 90px } }";
    assert_eq!(
        color_of(r#"<div class="wrap" id="target">X</div>"#, css),
        RED
    );
}

#[test]
fn declarations_after_a_nested_rule_still_apply_to_the_parent() {
    let css = ".wrap { .probe { margin-left: 90px } color: red }";
    assert_eq!(
        color_of(r#"<div class="wrap" id="target">X</div>"#, css),
        RED
    );
}

#[test]
fn declarations_after_a_nested_rule_cascade_after_it() {
    // Per the spec (CSSNestedDeclarations), declarations after a nested rule join the cascade
    // at a position after that rule. They are not hoisted to the front.
    let css = ".probe { & { color: red } color: blue }";
    assert_eq!(
        color_of(r#"<div class="probe" id="target">X</div>"#, css),
        BLUE
    );
}

// ===== Specificity =====

#[test]
fn nested_selector_takes_the_parent_specificity() {
    // `#wrap { & .probe }` = (1,1,0) beats the later `.wrap .probe` = (0,2,0).
    let css = "#wrap { & .probe { color: red } } .wrap .probe { color: blue }";
    assert_eq!(
        color_of(
            r#"<div id="wrap" class="wrap"><div class="probe" id="target">X</div></div>"#,
            css
        ),
        RED
    );
}

#[test]
fn equal_specificity_falls_back_to_source_order() {
    let css = ".wrap { & .probe { color: red } } .wrap .probe { color: blue }";
    assert_eq!(color_of(NESTED_PROBE, css), BLUE);
}

// ===== Error recovery =====

#[test]
fn an_invalid_nested_rule_does_not_take_its_siblings_with_it() {
    // `::first-line` is unsupported, so only that nested rule is discarded and the
    // declarations and sibling nested rules that follow survive.
    let css = ".wrap { .probe::first-line { color: blue } color: red; .probe { color: red } }";
    assert_eq!(
        color_of(r#"<div class="wrap" id="target">X</div>"#, css),
        RED
    );
    assert_eq!(color_of(NESTED_PROBE, css), RED);
}

// ===== A top-level `&` =====

#[test]
fn a_top_level_parent_selector_acts_as_scope() {
    // An `&` with no parent to substitute is `:scope` as the spec says, which in a stylesheet
    // is the root element. `color` is inherited, so its effect on `html` is observed on a descendant.
    let css = "& { color: red }";
    assert_eq!(color_of(r#"<div id="target">X</div>"#, css), RED);
    assert_eq!(
        margin_left_of(r#"<div id="target">X</div>"#, "& { margin-left: 90px }"),
        0.0,
        "it does not match an element other than the root"
    );
}
