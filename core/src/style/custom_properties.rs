//! CSS Custom Properties (`--foo`/`var()`), CSS Variables Level 1.
//!
//! Custom properties take part in the cascade and are inherited like any other inherited
//! property. They are resolved per element, and `var()` is substituted at computed-value
//! time:
//!
//! * While parsing a stylesheet, `--foo: value` becomes a [`CustomDeclaration`] holding the
//!   raw text. A declaration of an ordinary property that contains `var()` cannot be parsed
//!   yet, so it is kept as an [`UnparsedDeclaration`] (name plus raw value). A declaration
//!   without `var()` is parsed immediately as before, so the common case pays nothing.
//! * While computing an element's style, [`compute_custom_properties`] runs the cascade over
//!   the element's custom property declarations (specificity and order, `!important`, the
//!   `style` attribute) on top of the parent's computed map, resolving `var()` references
//!   inside the values (with cycle detection). The result is a [`CustomProperties`] stored in
//!   the computed style, shared with the parent through an `Rc` when nothing changed.
//! * Each [`UnparsedDeclaration`] is then substituted against that map and parsed
//!   ([`resolve_unparsed`]). If substitution or parsing fails, the declaration is "invalid at
//!   computed-value time": the property behaves as `unset` (inherited value for an inherited
//!   property, else the initial value).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use cssparser::{CowRcStr, ParseError, Parser, ParserInput, Token};

use super::properties::{parse_declaration, PropertyDeclaration};

/// The maximum nesting of `var()` fallbacks (`var(--a, var(--b, var(--c, ...)))`).
const MAX_FALLBACK_DEPTH: u32 = 16;

/// Computed custom property values, keyed by the full name including `--`. The values never
/// contain `var()`. A name that is absent is the guaranteed-invalid value.
pub type CustomPropertyMap = HashMap<Rc<str>, Rc<str>>;

/// The custom properties in effect on an element. Cloning is cheap (an `Rc`), and an element
/// that does not change anything shares its parent's map.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CustomProperties(Option<Rc<CustomPropertyMap>>);

impl CustomProperties {
    /// The computed value of `name` (including the leading `--`), if it is set.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.as_ref()?.get(name).map(|value| &**value)
    }

    pub fn is_empty(&self) -> bool {
        self.0.as_ref().is_none_or(|map| map.is_empty())
    }

    fn lookup(&self, name: &str) -> Option<Rc<str>> {
        self.0.as_ref()?.get(name).cloned()
    }
}

/// A `--name: value` declaration, with the value kept as written (trimmed).
#[derive(Debug, Clone, PartialEq)]
pub struct CustomDeclaration {
    pub name: Rc<str>,
    pub value: Rc<str>,
    pub important: bool,
}

/// A declaration of an ordinary property whose value contains `var()`. It can only be parsed
/// once the element's custom properties are known.
#[derive(Debug, Clone, PartialEq)]
pub struct UnparsedDeclaration {
    /// The property name, as written.
    pub name: Rc<str>,
    pub value: Rc<str>,
}

/// If a token starts a block (`{`/`(`/`[`/`func(`), its contents are invisible to
/// `Parser::next` unless entered explicitly with `Parser::parse_nested_block`.
fn is_block_start(token: &Token) -> bool {
    matches!(
        token,
        Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock
    )
}

/// Consume the rest of `input`, returning whether it contains a `var()` call anywhere
/// (including inside nested blocks).
fn consume_and_find_var(input: &mut Parser<'_, '_>) -> bool {
    let mut found = false;
    while let Ok(token) = input.next() {
        match token {
            Token::Function(name) if name.eq_ignore_ascii_case("var") => found = true,
            token if is_block_start(token) => {
                found |= input
                    .parse_nested_block(|input| -> Result<bool, ParseError<'_, ()>> {
                        Ok(consume_and_find_var(input))
                    })
                    .unwrap_or(false);
            }
            _ => {}
        }
    }
    found
}

/// Remove a trailing `!important` from `raw`, returning the remaining text and whether it
/// was present.
fn split_important(raw: &str) -> (&str, bool) {
    let raw = raw.trim_end();
    if raw.len() >= "important".len() {
        let (head, tail) = raw.split_at(raw.len() - "important".len());
        if tail.eq_ignore_ascii_case("important") {
            let head = head.trim_end();
            if let Some(head) = head.strip_suffix('!') {
                return (head.trim_end(), true);
            }
        }
    }
    (raw, false)
}

/// Parse one declaration. `--foo` becomes a [`CustomDeclaration`], an ordinary property is
/// parsed right away, and one whose value cannot be parsed because it contains `var()` is
/// deferred as an [`UnparsedDeclaration`].
pub fn parse_declaration_or_defer<'i>(
    name: &CowRcStr<'i>,
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    let start = input.state();
    if name.starts_with("--") {
        while input.next_including_whitespace_and_comments().is_ok() {}
        let raw = input.slice_from(start.position());
        let (value, important) = split_important(raw);
        return Ok(vec![PropertyDeclaration::Custom(CustomDeclaration {
            name: Rc::from(&**name),
            value: Rc::from(value.trim()),
            important,
        })]);
    }
    let error = match parse_declaration(name, input) {
        Ok(declarations) if input.is_exhausted() => return Ok(declarations),
        Ok(_) => input.new_custom_error(()),
        Err(error) => error,
    };
    input.reset(&start);
    if !consume_and_find_var(input) {
        return Err(error);
    }
    let raw = input.slice_from(start.position());
    let (value, important) = split_important(raw);
    if important {
        // `!important` is not supported on ordinary properties (the declaration is ignored).
        return Err(error);
    }
    Ok(vec![PropertyDeclaration::Unparsed(UnparsedDeclaration {
        name: Rc::from(&**name),
        value: Rc::from(value.trim()),
    })])
}

/// Substitute every `var()` in `text`, asking `lookup` for the computed value of each name.
/// Returns `None` when a reference has no value and no fallback (the result is then invalid).
fn substitute_vars(
    text: &str,
    lookup: &mut dyn FnMut(&str) -> Option<Rc<str>>,
    depth: u32,
) -> Option<String> {
    if depth > MAX_FALLBACK_DEPTH {
        return None;
    }
    // Most values have no `var()`; skip the tokenizer for them.
    if !text
        .as_bytes()
        .windows(4)
        .any(|w| w.eq_ignore_ascii_case(b"var("))
    {
        return Some(text.to_string());
    }
    let mut input = ParserInput::new(text);
    let mut parser = Parser::new(&mut input);
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let ok = substitute_in_scope(&mut parser, text, lookup, depth, &mut result, &mut cursor);
    if !ok {
        return None;
    }
    result.push_str(&text[cursor..]);
    Some(result)
}

/// Scan the parser's current scope and write the substituted text of each `var()` into
/// `result` (`cursor` is the start of the text not yet written). Returns `false` when a
/// reference is invalid.
fn substitute_in_scope(
    parser: &mut Parser,
    text: &str,
    lookup: &mut dyn FnMut(&str) -> Option<Rc<str>>,
    depth: u32,
    result: &mut String,
    cursor: &mut usize,
) -> bool {
    loop {
        let start_state = parser.state();
        match parser.next_including_whitespace_and_comments() {
            Ok(Token::Function(fn_name)) if fn_name.eq_ignore_ascii_case("var") => {
                let call_start = start_state.position().byte_index();
                let mut fallback_range: Option<(usize, usize)> = None;
                let var_name =
                    parser.parse_nested_block(|input| -> Result<String, ParseError<'_, ()>> {
                        let name = input.expect_ident()?.as_ref().to_string();
                        if input.try_parse(|input| input.expect_comma()).is_ok() {
                            let start = input.position().byte_index();
                            while input.next().is_ok() {}
                            let end = input.position().byte_index();
                            fallback_range = Some((start, end));
                        }
                        Ok(name)
                    });
                let call_end = parser.position().byte_index();
                let Ok(var_name) = var_name else {
                    return false;
                };
                if !var_name.starts_with("--") {
                    return false;
                }
                let replacement = match lookup(&var_name) {
                    Some(value) => value.to_string(),
                    None => match fallback_range {
                        Some((start, end)) => {
                            match substitute_vars(text[start..end].trim(), lookup, depth + 1) {
                                Some(value) => value,
                                None => return false,
                            }
                        }
                        None => return false,
                    },
                };
                result.push_str(&text[*cursor..call_start]);
                result.push_str(&replacement);
                *cursor = call_end;
            }
            Ok(token) if is_block_start(token) => {
                let ok = parser
                    .parse_nested_block(|input| -> Result<bool, ParseError<'_, ()>> {
                        Ok(substitute_in_scope(
                            input, text, lookup, depth, result, cursor,
                        ))
                    })
                    .unwrap_or(false);
                if !ok {
                    return false;
                }
            }
            Ok(_) => continue,
            Err(_) => return true,
        }
    }
}

/// Resolves the winning custom property declarations of one element against each other and
/// the inherited values.
struct Resolver<'a> {
    inherited: &'a CustomProperties,
    winners: HashMap<Rc<str>, Rc<str>>,
    done: HashMap<Rc<str>, Option<Rc<str>>>,
    stack: Vec<Rc<str>>,
    cyclic: HashSet<Rc<str>>,
}

impl Resolver<'_> {
    fn resolve(&mut self, name: &str) -> Option<Rc<str>> {
        if let Some(done) = self.done.get(name) {
            return done.clone();
        }
        let Some((key, raw)) = self
            .winners
            .get_key_value(name)
            .map(|(k, v)| (k.clone(), v.clone()))
        else {
            return self.inherited.lookup(name);
        };
        if let Some(position) = self.stack.iter().position(|n| **n == *name) {
            // A reference cycle: every property on the cycle is invalid.
            let members: Vec<Rc<str>> = self.stack[position..].to_vec();
            self.cyclic.extend(members);
            return None;
        }

        let value = if raw.eq_ignore_ascii_case("initial") {
            None
        } else if raw.eq_ignore_ascii_case("inherit") || raw.eq_ignore_ascii_case("unset") {
            self.inherited.lookup(name)
        } else {
            self.stack.push(key.clone());
            let substituted = substitute_vars(&raw, &mut |n| self.resolve(n), 0);
            self.stack.pop();
            substituted.map(|s| Rc::from(s.as_str()))
        };
        let value = if self.cyclic.contains(name) {
            None
        } else {
            value
        };
        self.done.insert(key, value.clone());
        value
    }
}

/// Cascade the custom property declarations in `declarations` (given in ascending cascade
/// priority) on top of `inherited`, the parent's computed custom properties.
///
/// Normal declarations are applied in order, then `!important` ones, so an important
/// declaration beats everything, an inline `style` included. When nothing changes the
/// parent's map is shared as is.
pub fn compute_custom_properties<'a, I>(
    inherited: &CustomProperties,
    declarations: I,
) -> CustomProperties
where
    I: Iterator<Item = &'a PropertyDeclaration> + Clone,
{
    let mut winners: HashMap<Rc<str>, Rc<str>> = HashMap::new();
    for important in [false, true] {
        for declaration in declarations.clone() {
            if let PropertyDeclaration::Custom(custom) = declaration {
                if custom.important == important {
                    winners.insert(custom.name.clone(), custom.value.clone());
                }
            }
        }
    }
    if winners.is_empty() {
        return inherited.clone();
    }

    let names: Vec<Rc<str>> = winners.keys().cloned().collect();
    let mut resolver = Resolver {
        inherited,
        winners,
        done: HashMap::new(),
        stack: Vec::new(),
        cyclic: HashSet::new(),
    };
    let resolved: Vec<(Rc<str>, Option<Rc<str>>)> = names
        .into_iter()
        .map(|name| {
            let value = resolver.resolve(&name);
            (name, value)
        })
        .collect();

    let mut map: Option<CustomPropertyMap> = None;
    for (name, value) in resolved {
        let current = inherited.lookup(&name);
        if current == value {
            continue;
        }
        let map = map.get_or_insert_with(|| inherited.0.as_deref().cloned().unwrap_or_default());
        match value {
            Some(value) => {
                map.insert(name, value);
            }
            None => {
                map.remove(&name);
            }
        }
    }
    match map {
        Some(map) => CustomProperties(Some(Rc::new(map))),
        None => inherited.clone(),
    }
}

/// Parse `value` as the value of property `name`, requiring all of it to be consumed.
fn parse_value_text(name: &str, value: &str) -> Option<Vec<PropertyDeclaration>> {
    let mut input = ParserInput::new(value);
    let mut parser = Parser::new(&mut input);
    let declarations = parse_declaration(&CowRcStr::from(name), &mut parser).ok()?;
    parser.is_exhausted().then_some(declarations)
}

/// Declarations covering every longhand `name` expands to, used to reset an ordinary
/// property whose `var()` is invalid at computed-value time. Found by parsing values that
/// are valid for nearly every property, so no table of shorthands is needed. Returns an
/// empty list when none of them parses (the declaration is then ignored).
fn longhand_probe(name: &str) -> Vec<PropertyDeclaration> {
    const PROBES: &[&str] = &[
        "0",
        "none",
        "auto",
        "normal",
        "red",
        "left",
        "static",
        "visible",
        "baseline",
        "solid",
        "serif",
        "block",
        "nowrap",
        "row",
        "stretch",
        "0 none red",
        "0 0",
    ];
    PROBES
        .iter()
        .find_map(|probe| parse_value_text(name, probe))
        .unwrap_or_default()
}

/// Substitute `var()` in `declaration` against `properties`, giving the text to parse.
pub fn substitute_unparsed(
    declaration: &UnparsedDeclaration,
    properties: &CustomProperties,
) -> Option<String> {
    substitute_vars(&declaration.value, &mut |n| properties.lookup(n), 0)
}

/// Substitute `var()` in `declaration` against `properties` and parse the result.
///
/// Returns the declarations it expands to and whether they are to be applied as a reset
/// (`true`: the substitution or parse failed, so the declarations only name the longhands to
/// reset to `unset`).
pub fn resolve_unparsed(
    declaration: &UnparsedDeclaration,
    properties: &CustomProperties,
) -> (Vec<PropertyDeclaration>, bool) {
    let resolved = substitute_unparsed(declaration, properties)
        .and_then(|text| parse_value_text(&declaration.name, &text));
    match resolved {
        Some(declarations) => (declarations, false),
        None => (longhand_probe(&declaration.name), true),
    }
}

/// Replace every [`UnparsedDeclaration`] in `declarations` with what it resolves to against
/// `properties`; custom property declarations are dropped. A declaration that is invalid at
/// computed-value time is dropped as well. For consumers that have no notion of `unset`
/// (`@page` rules, `::first-letter`).
pub fn resolve_declarations<'a>(
    declarations: impl IntoIterator<Item = &'a PropertyDeclaration>,
    properties: &CustomProperties,
) -> Vec<PropertyDeclaration> {
    let mut out = Vec::new();
    for declaration in declarations {
        match declaration {
            PropertyDeclaration::Custom(_) => {}
            PropertyDeclaration::Unparsed(unparsed) => {
                if let (resolved, false) = resolve_unparsed(unparsed, properties) {
                    out.extend(resolved);
                }
            }
            other => out.push(other.clone()),
        }
    }
    out
}

/// Whether any declaration needs the custom property machinery.
pub fn uses_variables<'a>(mut declarations: impl Iterator<Item = &'a PropertyDeclaration>) -> bool {
    declarations.any(|declaration| {
        matches!(
            declaration,
            PropertyDeclaration::Custom(_) | PropertyDeclaration::Unparsed(_)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::parse_stylesheet;

    fn decls(css: &str) -> Vec<PropertyDeclaration> {
        parse_stylesheet(css)
            .rules
            .into_iter()
            .flat_map(|rule| rule.declarations)
            .collect()
    }

    fn compute(
        inherited: &CustomProperties,
        declarations: &[PropertyDeclaration],
    ) -> CustomProperties {
        compute_custom_properties(inherited, declarations.iter())
    }

    #[test]
    fn declarations_without_var_are_parsed_eagerly() {
        let d = decls("p { margin-left: 4px; }");
        assert!(matches!(d[0], PropertyDeclaration::MarginLeft(_)), "{d:?}");
    }

    #[test]
    fn declarations_with_var_are_deferred() {
        let d = decls("p { margin: 1px var(--a) 2px; }");
        assert!(matches!(&d[0], PropertyDeclaration::Unparsed(u) if &*u.name == "margin"));
    }

    #[test]
    fn var_inside_a_function_is_detected() {
        let d = decls("p { width: calc(var(--a) * 2); }");
        assert!(matches!(d[0], PropertyDeclaration::Unparsed(_)), "{d:?}");
    }

    #[test]
    fn invalid_declaration_without_var_is_dropped() {
        assert!(decls("p { margin-left: nonsense; }").is_empty());
    }

    #[test]
    fn important_on_custom_property_is_recorded() {
        let d = decls("p { --a: 1px !important; --b: 2px }");
        assert!(
            matches!(&d[0], PropertyDeclaration::Custom(c) if c.important && &*c.value == "1px")
        );
        assert!(matches!(&d[1], PropertyDeclaration::Custom(c) if !c.important));
    }

    #[test]
    fn cascade_order_and_inheritance() {
        let parent = compute(
            &CustomProperties::default(),
            &decls("p { --a: 1px; --b: 2px }"),
        );
        assert_eq!(parent.get("--a"), Some("1px"));
        let child = compute(&parent, &decls("p { --a: 5px }"));
        assert_eq!(child.get("--a"), Some("5px"));
        assert_eq!(child.get("--b"), Some("2px"));
        assert_eq!(parent.get("--a"), Some("1px"));
    }

    #[test]
    fn unchanged_map_is_shared_with_the_parent() {
        let parent = compute(&CustomProperties::default(), &decls("p { --a: 1px }"));
        let child = compute(&parent, &decls("p { --a: 1px }"));
        assert!(Rc::ptr_eq(
            parent.0.as_ref().unwrap(),
            child.0.as_ref().unwrap()
        ));
    }

    #[test]
    fn important_beats_a_later_normal_declaration() {
        let p = compute(
            &CustomProperties::default(),
            &decls("p { --a: 1px !important; --a: 2px }"),
        );
        assert_eq!(p.get("--a"), Some("1px"));
    }

    #[test]
    fn references_resolve_regardless_of_order_and_against_inherited() {
        let parent = compute(&CustomProperties::default(), &decls("p { --base: 8px }"));
        let child = compute(&parent, &decls("p { --b: var(--a); --a: var(--base) }"));
        assert_eq!(child.get("--a"), Some("8px"));
        assert_eq!(child.get("--b"), Some("8px"));
    }

    #[test]
    fn cycles_are_invalid() {
        let p = compute(
            &CustomProperties::default(),
            &decls("p { --a: var(--b); --b: var(--a, 1px); --c: 2px }"),
        );
        assert_eq!(p.get("--a"), None);
        assert_eq!(p.get("--b"), None);
        assert_eq!(p.get("--c"), Some("2px"));
    }

    #[test]
    fn self_reference_is_invalid_and_hides_the_inherited_value() {
        let parent = compute(&CustomProperties::default(), &decls("p { --a: 1px }"));
        let child = compute(&parent, &decls("p { --a: var(--a) }"));
        assert_eq!(child.get("--a"), None);
    }

    #[test]
    fn a_declaration_may_reference_the_inherited_value_of_its_own_name_through_the_parent_only() {
        // `--a: calc(var(--a) + 1px)` is a self-cycle, not a reference to the parent's value.
        let parent = compute(&CustomProperties::default(), &decls("p { --a: 1px }"));
        let child = compute(&parent, &decls("p { --a: calc(var(--a) + 1px) }"));
        assert_eq!(child.get("--a"), None);
    }

    #[test]
    fn fallbacks_nest() {
        let p = compute(
            &CustomProperties::default(),
            &decls("p { --x: var(--missing, var(--also-missing, 3px)) }"),
        );
        assert_eq!(p.get("--x"), Some("3px"));
    }

    #[test]
    fn initial_and_inherit_keywords() {
        let parent = compute(
            &CustomProperties::default(),
            &decls("p { --a: 1px; --b: 2px }"),
        );
        let child = compute(
            &parent,
            &decls("p { --a: initial; --b: 9px; --b: inherit }"),
        );
        assert_eq!(child.get("--a"), None);
        assert_eq!(child.get("--b"), Some("2px"));
    }

    #[test]
    fn unparsed_declaration_resolves_and_parses() {
        let props = compute(&CustomProperties::default(), &decls("p { --gap: 6px }"));
        let d = decls("p { margin: var(--gap) 2px }");
        let PropertyDeclaration::Unparsed(u) = &d[0] else {
            panic!("{d:?}");
        };
        let (resolved, reset) = resolve_unparsed(u, &props);
        assert!(!reset);
        assert_eq!(resolved.len(), 4);
    }

    #[test]
    fn unresolvable_unparsed_declaration_resets_every_longhand() {
        let d = decls("p { margin: var(--nope) }");
        let PropertyDeclaration::Unparsed(u) = &d[0] else {
            panic!("{d:?}");
        };
        let (resolved, reset) = resolve_unparsed(u, &CustomProperties::default());
        assert!(reset);
        assert_eq!(resolved.len(), 4, "{resolved:?}");
    }

    #[test]
    fn ignores_var_like_text_in_strings_and_comments() {
        let d = decls("p { content: \"var(--x)\"; /* var(--y) */ color: red; }");
        assert!(
            d.iter()
                .all(|d| !matches!(d, PropertyDeclaration::Unparsed(_))),
            "{d:?}"
        );
    }
}
