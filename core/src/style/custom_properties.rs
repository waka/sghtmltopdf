//! CSS Custom Properties (`--foo`/`var()`), CSS Variables Level 1, and `@property`
//! registration from CSS Properties and Values API Level 1.
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
//!   the computed style. Inheriting values share an `Rc` with the parent when nothing changed;
//!   registered non-inheriting values are kept separately for the element that declares them.
//! * Each [`UnparsedDeclaration`] is then substituted against that map and parsed
//!   ([`resolve_unparsed`]). If substitution or parsing fails, the declaration is "invalid at
//!   computed-value time": the property behaves as `unset` (inherited value for an inherited
//!   property, else the initial value).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use cssparser::{
    CowRcStr, DeclarationParser, ParseError, Parser, ParserInput, RuleBodyItemParser,
    RuleBodyParser, Token,
};

use super::css_wide::{CssWideDeclaration, CssWideKeyword};
use super::properties::{parse_declaration, PropertyDeclaration};

/// The maximum nesting of `var()` fallbacks (`var(--a, var(--b, var(--c, ...)))`).
const MAX_FALLBACK_DEPTH: u32 = 16;

/// Computed custom property values, keyed by the full name including `--`. The values never
/// contain `var()`. A name that is absent is the guaranteed-invalid value.
pub type CustomPropertyMap = HashMap<Rc<str>, Rc<str>>;

/// One valid `@property` registration. Syntax checking is deliberately not implemented;
/// `syntax` is parsed only to validate the rule and decide whether `initial-value` is required.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PropertyRegistration {
    pub name: Rc<str>,
    pub inherits: bool,
    pub initial_value: Option<Rc<str>>,
}

/// The last valid registration for each name, shared by every element in the document.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct PropertyRegistrations(Option<Rc<HashMap<Rc<str>, PropertyRegistration>>>);

impl PropertyRegistrations {
    pub fn from_stylesheets(
        ua: &super::stylesheet::Stylesheet,
        author: &super::stylesheet::Stylesheet,
    ) -> Self {
        let mut registrations = HashMap::new();
        for registration in ua
            .property_registrations
            .iter()
            .chain(&author.property_registrations)
        {
            registrations.insert(registration.name.clone(), registration.clone());
        }
        if registrations.is_empty() {
            Self::default()
        } else {
            Self(Some(Rc::new(registrations)))
        }
    }

    fn get(&self, name: &str) -> Option<&PropertyRegistration> {
        self.0.as_ref()?.get(name)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_none()
    }
}

/// The custom properties in effect on an element. Cloning is cheap (an `Rc`), and an element
/// that does not change anything shares its parent's map.
#[derive(Debug, Clone, Default)]
pub struct CustomProperties {
    /// Values that inherit by default: unregistered properties and registered properties
    /// whose `inherits` descriptor is true.
    inherited_values: Option<Rc<CustomPropertyMap>>,
    /// Values of registered non-inheriting properties declared on this element. An absent
    /// value falls back to the registration's initial value instead of the parent.
    local_values: Option<Rc<CustomPropertyMap>>,
    registrations: PropertyRegistrations,
}

impl CustomProperties {
    pub(super) fn with_registrations(registrations: PropertyRegistrations) -> Self {
        Self {
            inherited_values: None,
            local_values: None,
            registrations,
        }
    }

    pub(super) fn set_registrations(&mut self, registrations: PropertyRegistrations) {
        self.registrations = registrations;
    }

    pub(super) fn registrations(&self) -> PropertyRegistrations {
        self.registrations.clone()
    }

    pub(super) fn shares_registrations(&self, registrations: &PropertyRegistrations) -> bool {
        match (&self.registrations.0, &registrations.0) {
            (Some(left), Some(right)) => Rc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        }
    }

    pub(super) fn can_share_with_child(&self) -> bool {
        self.local_values.is_none()
    }

    /// The computed value of `name` (including the leading `--`), if it is set.
    pub fn get(&self, name: &str) -> Option<&str> {
        if self.registration_inherits(name) == Some(false) {
            return self
                .local_values
                .as_ref()
                .and_then(|map| map.get(name))
                .map(|value| &**value)
                .or_else(|| self.registered_initial(name));
        }
        self.inherited_values
            .as_ref()
            .and_then(|map| map.get(name))
            .map(|value| &**value)
            .or_else(|| self.registered_initial(name))
    }

    pub fn is_empty(&self) -> bool {
        self.inherited_values
            .as_ref()
            .is_none_or(|map| map.is_empty())
            && self.local_values.as_ref().is_none_or(|map| map.is_empty())
            && self.registrations.0.as_ref().is_none_or(|map| {
                map.values()
                    .all(|registration| registration.initial_value.is_none())
            })
    }

    fn lookup(&self, name: &str) -> Option<Rc<str>> {
        if self.registration_inherits(name) == Some(false) {
            return self
                .local_values
                .as_ref()
                .and_then(|map| map.get(name))
                .cloned()
                .or_else(|| self.registration_initial(name));
        }
        self.inherited_values
            .as_ref()
            .and_then(|map| map.get(name))
            .cloned()
            .or_else(|| self.registration_initial(name))
    }

    fn registered_initial(&self, name: &str) -> Option<&str> {
        self.registrations.get(name)?.initial_value.as_deref()
    }

    fn registration_initial(&self, name: &str) -> Option<Rc<str>> {
        self.registrations.get(name)?.initial_value.clone()
    }

    fn registration_inherits(&self, name: &str) -> Option<bool> {
        Some(self.registrations.get(name)?.inherits)
    }

    fn inherited_lookup(&self, name: &str) -> Option<Rc<str>> {
        match self.registrations.get(name) {
            Some(registration) if !registration.inherits => registration.initial_value.clone(),
            _ => self.lookup(name),
        }
    }

    fn initial_value(&self, name: &str) -> Option<Rc<str>> {
        self.registration_initial(name)
    }

    fn invalid_value(&self, name: &str) -> Option<Rc<str>> {
        match self.registrations.get(name) {
            Some(registration) if registration.inherits => self.lookup(name),
            Some(registration) => registration.initial_value.clone(),
            None => None,
        }
    }
}

impl PartialEq for CustomProperties {
    fn eq(&self, other: &Self) -> bool {
        same_shared_map(&self.inherited_values, &other.inherited_values)
            && same_shared_map(&self.local_values, &other.local_values)
            && match (&self.registrations.0, &other.registrations.0) {
                (Some(left), Some(right)) => Rc::ptr_eq(left, right) || left == right,
                (None, None) => true,
                _ => false,
            }
    }
}

fn same_shared_map(
    left: &Option<Rc<CustomPropertyMap>>,
    right: &Option<Rc<CustomPropertyMap>>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Rc::ptr_eq(left, right) || left == right,
        (None, None) => true,
        _ => false,
    }
}

enum PropertyDescriptor {
    Syntax(Rc<str>),
    Inherits(bool),
    InitialValue(Rc<str>),
}

struct PropertyDescriptorParser;

impl<'i> DeclarationParser<'i> for PropertyDescriptorParser {
    type Declaration = PropertyDescriptor;
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _declaration_start: &cssparser::ParserState,
    ) -> Result<Self::Declaration, ParseError<'i, Self::Error>> {
        if name.eq_ignore_ascii_case("syntax") {
            let syntax = input.expect_string_cloned()?;
            input.expect_exhausted()?;
            return Ok(PropertyDescriptor::Syntax(Rc::from(&*syntax)));
        }
        if name.eq_ignore_ascii_case("inherits") {
            let inherits = input.expect_ident_cloned()?;
            input.expect_exhausted()?;
            if inherits.eq_ignore_ascii_case("true") {
                return Ok(PropertyDescriptor::Inherits(true));
            }
            if inherits.eq_ignore_ascii_case("false") {
                return Ok(PropertyDescriptor::Inherits(false));
            }
            return Err(input.new_custom_error(()));
        }
        if name.eq_ignore_ascii_case("initial-value") {
            let start = input.state();
            while input.next_including_whitespace_and_comments().is_ok() {}
            let value = input.slice_from(start.position()).trim();
            if value.is_empty() {
                return Err(input.new_custom_error(()));
            }
            return Ok(PropertyDescriptor::InitialValue(Rc::from(value)));
        }
        Err(input.new_custom_error(()))
    }
}

impl<'i> cssparser::QualifiedRuleParser<'i> for PropertyDescriptorParser {
    type Prelude = ();
    type QualifiedRule = PropertyDescriptor;
    type Error = ();
}

impl<'i> cssparser::AtRuleParser<'i> for PropertyDescriptorParser {
    type Prelude = ();
    type AtRule = PropertyDescriptor;
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, PropertyDescriptor, ()> for PropertyDescriptorParser {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

/// Parse the descriptors of one `@property` rule. Invalid and unknown descriptors are
/// ignored; the whole registration is invalid when a required descriptor is absent.
pub(super) fn parse_property_rule_block<'i, 't>(
    name: Rc<str>,
    input: &mut Parser<'i, 't>,
) -> Result<PropertyRegistration, ParseError<'i, ()>> {
    let mut syntax = None;
    let mut inherits = None;
    let mut initial_value = None;
    let mut parser = PropertyDescriptorParser;
    for descriptor in RuleBodyParser::new(input, &mut parser).flatten() {
        match descriptor {
            PropertyDescriptor::Syntax(value) => syntax = Some(value),
            PropertyDescriptor::Inherits(value) => inherits = Some(value),
            PropertyDescriptor::InitialValue(value) => initial_value = Some(value),
        }
    }
    let Some(syntax) = syntax else {
        return Err(input.new_custom_error(()));
    };
    let Some(inherits) = inherits else {
        return Err(input.new_custom_error(()));
    };
    if initial_value.is_none() && &*syntax != "*" {
        return Err(input.new_custom_error(()));
    }
    Ok(PropertyRegistration {
        name,
        inherits,
        initial_value,
    })
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
pub(super) fn split_important(raw: &str) -> (&str, bool) {
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
    if let Some(keyword) = CssWideKeyword::parse(input) {
        // An unknown property is rejected as it is with any other value.
        return css_wide_declaration(name, keyword)
            .map(|declaration| vec![declaration])
            .ok_or_else(|| input.new_custom_error(()));
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
    parent: &'a CustomProperties,
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
            return self.parent.inherited_lookup(name);
        };
        if let Some(position) = self.stack.iter().position(|n| **n == *name) {
            // A reference cycle: every property on the cycle is invalid.
            let members: Vec<Rc<str>> = self.stack[position..].to_vec();
            self.cyclic.extend(members);
            return None;
        }

        let (value, invalid) = if raw.eq_ignore_ascii_case("initial") {
            (self.parent.initial_value(name), false)
        } else if raw.eq_ignore_ascii_case("inherit") {
            (self.parent.lookup(name), false)
        } else if raw.eq_ignore_ascii_case("unset") {
            (self.parent.inherited_lookup(name), false)
        } else {
            self.stack.push(key.clone());
            let substituted = substitute_vars(&raw, &mut |n| self.resolve(n), 0);
            self.stack.pop();
            let invalid = substituted.is_none();
            (substituted.map(|s| Rc::from(s.as_str())), invalid)
        };
        let value = if self.cyclic.contains(name) || invalid {
            self.parent.invalid_value(name)
        } else {
            value
        };
        self.done.insert(key, value.clone());
        value
    }
}

/// Cascade the custom property declarations in `declarations` (given in ascending cascade
/// priority) on top of `parent`, the parent's computed custom properties.
///
/// Normal declarations are applied in order, then `!important` ones, so an important
/// declaration beats everything, an inline `style` included. When nothing changes the
/// parent's map is shared as is.
pub fn compute_custom_properties<'a, I>(
    parent: &CustomProperties,
    declarations: I,
) -> CustomProperties
where
    I: Iterator<Item = &'a PropertyDeclaration> + Clone,
{
    // Fast path: every winning value is one of the declared values, so when none can change
    // the value this element starts with, reuse the parent's inheriting map. Tailwind v3's
    // preflight declares dozens of properties on `*`, which lands here on every element.
    let unchanged = declarations.clone().all(|declaration| {
        let PropertyDeclaration::Custom(custom) = declaration else {
            return true;
        };
        is_plain_value(&custom.value)
            && parent.inherited_lookup(&custom.name).as_deref() == Some(&*custom.value)
    });
    if unchanged {
        return inherited_child(parent);
    }

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
        return inherited_child(parent);
    }

    let names: Vec<Rc<str>> = winners.keys().cloned().collect();
    let mut resolver = Resolver {
        parent,
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

    let mut inherited_map: Option<CustomPropertyMap> = None;
    let mut local_map = CustomPropertyMap::new();
    for (name, value) in resolved {
        if parent.registration_inherits(&name) == Some(false) {
            if parent.registration_initial(&name) != value {
                if let Some(value) = value {
                    local_map.insert(name, value);
                }
            }
            continue;
        }
        if parent.lookup(&name) != value {
            let map = inherited_map.get_or_insert_with(|| {
                parent
                    .inherited_values
                    .as_deref()
                    .cloned()
                    .unwrap_or_default()
            });
            match value {
                Some(value) => {
                    map.insert(name, value);
                }
                None => {
                    map.remove(&name);
                }
            }
        }
    }
    CustomProperties {
        inherited_values: inherited_map
            .map(Rc::new)
            .or_else(|| parent.inherited_values.clone()),
        local_values: (!local_map.is_empty()).then(|| Rc::new(local_map)),
        registrations: parent.registrations.clone(),
    }
}

fn inherited_child(parent: &CustomProperties) -> CustomProperties {
    CustomProperties {
        inherited_values: parent.inherited_values.clone(),
        local_values: None,
        registrations: parent.registrations.clone(),
    }
}

/// Whether a declared value is already its own computed value: no `var()` to substitute and
/// not a CSS-wide keyword to look up.
fn is_plain_value(value: &str) -> bool {
    const KEYWORDS: &[&str] = &["initial", "inherit", "unset", "revert", "revert-layer"];
    !value
        .as_bytes()
        .windows(4)
        .any(|w| w.eq_ignore_ascii_case(b"var("))
        && !KEYWORDS.iter().any(|k| value.eq_ignore_ascii_case(k))
}

/// Parse `value` as the value of property `name`, requiring all of it to be consumed.
fn parse_value_text(name: &str, value: &str) -> Option<Vec<PropertyDeclaration>> {
    let mut input = ParserInput::new(value);
    let mut parser = Parser::new(&mut input);
    let declarations = parse_declaration(&CowRcStr::from(name), &mut parser).ok()?;
    parser.is_exhausted().then_some(declarations)
}

/// Whether `name` is an ordinary property we know, whatever its value (a property that
/// takes a CSS-wide keyword is exactly one that has a longhand probe).
pub(super) fn is_known_property(name: &str) -> bool {
    !longhand_probe(name).is_empty()
}

/// Declarations covering every longhand `name` expands to, used to reset an ordinary
/// property whose `var()` is invalid at computed-value time and to name the longhands of a
/// CSS-wide keyword declaration. Found by parsing values that
/// are valid for nearly every property, so no table of shorthands is needed. Returns an
/// empty list when none of them parses (the declaration is then ignored).
fn longhand_probe(name: &str) -> Vec<PropertyDeclaration> {
    const PROBES: &[&str] = &[
        "0 none red",
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
        "0 0",
        "border-box",
        "repeat",
        "scroll",
        "collapse",
        "top",
        "show",
        "inside",
        "over",
        "clip",
        "2",
        "\"\"",
        "closed",
    ];
    PROBES
        .iter()
        .find_map(|probe| parse_value_text(name, probe))
        .unwrap_or_default()
}

/// The declaration for property `name` set to a CSS-wide keyword, or `None` when `name` is
/// not a property we know.
fn css_wide_declaration(name: &str, keyword: CssWideKeyword) -> Option<PropertyDeclaration> {
    let longhands = longhand_probe(name);
    (!longhands.is_empty()).then(|| {
        PropertyDeclaration::CssWide(CssWideDeclaration {
            keyword,
            longhands: longhands.into(),
        })
    })
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
    let substituted = substitute_unparsed(declaration, properties);
    // A `var()` fallback may supply a CSS-wide keyword (`var(--x, inherit)`).
    if let Some(keyword) = substituted.as_deref().and_then(CssWideKeyword::from_text) {
        return match css_wide_declaration(&declaration.name, keyword) {
            Some(declaration) => (vec![declaration], false),
            None => (Vec::new(), true),
        };
    }
    let resolved = substituted.and_then(|text| parse_value_text(&declaration.name, &text));
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
            PropertyDeclaration::Custom(_) | PropertyDeclaration::CssWide(_) => {}
            PropertyDeclaration::Unparsed(unparsed) => {
                if let (resolved, false) = resolve_unparsed(unparsed, properties) {
                    out.extend(
                        resolved
                            .into_iter()
                            .filter(|d| !matches!(d, PropertyDeclaration::CssWide(_))),
                    );
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
    use crate::style::{parse_stylesheet, Stylesheet};

    #[test]
    fn property_rule_descriptors_are_parsed() {
        let sheet = parse_stylesheet(
            r#"@property --gap { syntax: "<length>"; inherits: false; initial-value: 3px }
               @property --tokens { syntax: "*"; inherits: true }"#,
        );
        assert_eq!(sheet.property_registrations.len(), 2);
        let gap = &sheet.property_registrations[0];
        assert_eq!(&*gap.name, "--gap");
        assert!(!gap.inherits);
        assert_eq!(gap.initial_value.as_deref(), Some("3px"));
        assert_eq!(sheet.property_registrations[1].initial_value, None);
    }

    #[test]
    fn invalid_property_rules_are_dropped() {
        let sheet = parse_stylesheet(
            r#"@property --a { inherits: false; initial-value: 1px }
               @property --b { syntax: "*"; initial-value: 2px }
               @property --c { syntax: "<length>"; inherits: false }
               @property --d { syntax: "<length>"; inherits: false; initial-value: }"#,
        );
        assert!(sheet.property_registrations.is_empty());
    }

    #[test]
    fn every_kind_of_property_has_a_longhand_probe() {
        // One property per probe value, plus the shorthands; a property without a probe
        // could not be reset by an invalid `var()` or take a CSS-wide keyword.
        for (name, longhands) in [
            ("border", 12),
            ("margin", 4),
            ("outline", 3),
            ("border-radius", 4),
            ("width", 1),
            ("display", 1),
            ("color", 1),
            ("font-family", 1),
            ("box-sizing", 1),
            ("background-repeat", 1),
            ("background-attachment", 1),
            ("border-collapse", 1),
            ("caption-side", 1),
            ("empty-cells", 1),
            ("list-style-position", 1),
            ("text-emphasis-position", 1),
            ("text-overflow", 1),
            ("orphans", 1),
            ("widows", 1),
            ("bookmark-level", 1),
            ("bookmark-label", 1),
            ("bookmark-state", 1),
        ] {
            assert_eq!(longhand_probe(name).len(), longhands, "{name}");
        }
        assert!(longhand_probe("no-such-property").is_empty());
    }

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
            parent.inherited_values.as_ref().unwrap(),
            child.inherited_values.as_ref().unwrap()
        ));
    }

    #[test]
    fn redeclaring_the_inherited_values_everywhere_shares_the_map() {
        let css = "* { --a: 1px; --b: 2px !important; --c: solid }";
        let parent = compute(&CustomProperties::default(), &decls(css));
        let child = compute(&parent, &decls(css));
        assert!(Rc::ptr_eq(
            parent.inherited_values.as_ref().unwrap(),
            child.inherited_values.as_ref().unwrap()
        ));
    }

    #[test]
    fn non_inheriting_registrations_keep_the_inheriting_map_shared() {
        let author = parse_stylesheet(
            r#"@property --local { syntax: "*"; inherits: false; initial-value: solid }"#,
        );
        let initial = CustomProperties::with_registrations(
            PropertyRegistrations::from_stylesheets(&Stylesheet::default(), &author),
        );
        let parent = compute(&initial, &decls("p { --shared: 1px; --local: none }"));
        let child = compute(&parent, &[]);

        assert!(Rc::ptr_eq(
            parent.inherited_values.as_ref().unwrap(),
            child.inherited_values.as_ref().unwrap()
        ));
        assert_eq!(parent.get("--local"), Some("none"));
        assert_eq!(child.get("--local"), Some("solid"));
        assert!(child.local_values.is_none());
    }

    #[test]
    fn the_unchanged_fast_path_still_notices_a_difference() {
        let parent = compute(&CustomProperties::default(), &decls("p { --a: 1px }"));
        // One value equals the inherited one, the other does not.
        let child = compute(&parent, &decls("p { --a: 1px; --b: 2px }"));
        assert_eq!(child.get("--a"), Some("1px"));
        assert_eq!(child.get("--b"), Some("2px"));
        // An equal value that an `!important` declaration then overrides.
        let child = compute(&parent, &decls("p { --a: 1px; --a: 3px !important }"));
        assert_eq!(child.get("--a"), Some("3px"));
    }

    #[test]
    fn css_wide_keywords_and_var_never_take_the_fast_path() {
        let parent = compute(&CustomProperties::default(), &decls("p { --a: initial }"));
        // `initial` is not the guaranteed-invalid value's textual form, so it must resolve.
        assert_eq!(parent.get("--a"), None);
        let parent = compute(&CustomProperties::default(), &decls("p { --a: 1px }"));
        let child = compute(&parent, &decls("p { --a: initial }"));
        assert_eq!(child.get("--a"), None);
        let child = compute(&parent, &decls("p { --a: var(--missing) }"));
        assert_eq!(child.get("--a"), None);
        let child = compute(&parent, &decls("p { --a: VAR(--missing, 4px) }"));
        assert_eq!(child.get("--a"), Some("4px"));
    }

    #[test]
    fn an_invalid_border_var_resets_the_colour_too() {
        let probe = longhand_probe("border");
        assert!(
            probe
                .iter()
                .any(|d| matches!(d, PropertyDeclaration::BorderTopColor(_))),
            "{probe:?}"
        );
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
