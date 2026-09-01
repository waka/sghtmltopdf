//! The `SelectorImpl` implementation for the `selectors` crate.
//!
//! `selectors` requires implementations of various traits ([`cssparser::ToCss`],
//! [`PrecomputedHash`] and so on), which `html5ever`'s atom types (`LocalName`/`Namespace`)
//! do not provide, so they are wrapped in thin newtypes modelled on the `scraper` crate.

use std::fmt;

use cssparser::{match_ignore_ascii_case, CowRcStr, SourceLocation, ToCss};
use html5ever::{LocalName, Namespace};
use precomputed_hash::PrecomputedHash;
use selectors::parser::{self, SelectorImpl, SelectorParseErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SgSelectorImpl;

impl SelectorImpl for SgSelectorImpl {
    type ExtraMatchingData<'a> = ();
    type AttrValue = CssString;
    type Identifier = CssLocalName;
    type LocalName = CssLocalName;
    type NamespacePrefix = CssLocalName;
    type NamespaceUrl = Namespace;
    type BorrowedNamespaceUrl = Namespace;
    type BorrowedLocalName = CssLocalName;
    type NonTSPseudoClass = NonTSPseudoClass;
    type PseudoElement = PseudoElement;
}

/// The selector parser itself.
#[derive(Clone, Copy, Debug)]
pub struct SelectorParser;

impl<'i> parser::Parser<'i> for SelectorParser {
    type Impl = SgSelectorImpl;
    type Error = parser::SelectorParseErrorKind<'i>;

    /// Enable `:is()`/`:where()`.
    ///
    /// Both are treated as forgiving selector lists (an unsupported selector mixed in drops
    /// only that item and keeps the list alive). For specificity, `:is()` takes the highest
    /// of its arguments and `:where()` is always 0.
    fn parse_is_and_where(&self) -> bool {
        true
    }

    /// Enable `:has()`.
    ///
    /// Matching uses the `selectors` crate's relational selector implementation as-is
    /// (it walks the subject's subtree and later siblings).
    fn parse_has(&self) -> bool {
        true
    }

    /// Enable `&` (the parent selector) in nested rules (CSS Nesting).
    ///
    /// [`super::stylesheet`] replaces `&` with the parent's selector list immediately after
    /// parsing a nested rule's selector, so it never survives to matching. Only an `&`
    /// written in a top-level rule has no parent to substitute, and is treated as `:scope`
    /// (the root element), as the spec requires.
    fn parse_parent_selector(&self) -> bool {
        true
    }

    /// Non-structural pseudo-classes such as `:hover` are supported at parse time (treating
    /// them as never matching is [`super::element_ref::ElementRef::match_non_ts_pseudo_class`]'s job).
    ///
    /// Leaving them unsupported (with `NonTSPseudoClass` an empty enum) would be worse:
    /// the `selectors` crate's `SelectorList::parse` is unforgiving (one invalid selector
    /// makes the whole list an `Err`), so in `.foo, .bar:hover { ... }`, where only part of
    /// the comma-separated list contains `:hover`, the declarations for the unrelated `.foo`
    /// would vanish along with the rule. Parsing successfully avoids that collateral damage.
    fn parse_non_ts_pseudo_class(
        &self,
        location: SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<NonTSPseudoClass, cssparser::ParseError<'i, Self::Error>> {
        Ok(match_ignore_ascii_case! { &name,
            "hover" => NonTSPseudoClass::Hover,
            "active" => NonTSPseudoClass::Active,
            "focus" => NonTSPseudoClass::Focus,
            "focus-within" => NonTSPseudoClass::FocusWithin,
            "focus-visible" => NonTSPseudoClass::FocusVisible,
            "visited" => NonTSPseudoClass::Visited,
            "link" => NonTSPseudoClass::Link,
            "any-link" => NonTSPseudoClass::AnyLink,
            "target" => NonTSPseudoClass::Target,
            "enabled" => NonTSPseudoClass::Enabled,
            "disabled" => NonTSPseudoClass::Disabled,
            "checked" => NonTSPseudoClass::Checked,
            _ => {
                return Err(location.new_custom_error(
                    SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name),
                ))
            }
        })
    }

    /// Supports `::before`/`::after`/`::first-letter`. `::first-line` is not supported
    fn parse_pseudo_element(
        &self,
        location: SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<PseudoElement, cssparser::ParseError<'i, Self::Error>> {
        Ok(match_ignore_ascii_case! { &name,
            "before" => PseudoElement::Before,
            "after" => PseudoElement::After,
            "first-letter" => PseudoElement::FirstLetter,
            _ => {
                return Err(location.new_custom_error(
                    SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name),
                ))
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssString(pub String);

impl<'a> From<&'a str> for CssString {
    fn from(value: &'a str) -> Self {
        Self(value.to_owned())
    }
}

impl AsRef<str> for CssString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl ToCss for CssString {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_string(&self.0, dest)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct CssLocalName(pub LocalName);

impl<'a> From<&'a str> for CssLocalName {
    fn from(value: &'a str) -> Self {
        Self(value.into())
    }
}

impl PrecomputedHash for CssLocalName {
    fn precomputed_hash(&self) -> u32 {
        self.0.precomputed_hash()
    }
}

impl ToCss for CssLocalName {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(&self.0)
    }
}

/// Non-structural (state-dependent) pseudo-classes. A PDF is non-interactive output, so all
/// of these are supported at parse time but treated as never matching during actual matching
/// (see [`super::element_ref::ElementRef::match_non_ts_pseudo_class`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonTSPseudoClass {
    Hover,
    Active,
    Focus,
    FocusWithin,
    FocusVisible,
    Visited,
    Link,
    AnyLink,
    Target,
    Enabled,
    Disabled,
    Checked,
}

impl parser::NonTSPseudoClass for NonTSPseudoClass {
    type Impl = SgSelectorImpl;

    fn is_active_or_hover(&self) -> bool {
        matches!(self, Self::Active | Self::Hover)
    }

    fn is_user_action_state(&self) -> bool {
        matches!(
            self,
            Self::Active | Self::Hover | Self::Focus | Self::FocusWithin | Self::FocusVisible
        )
    }
}

impl ToCss for NonTSPseudoClass {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(match self {
            Self::Hover => ":hover",
            Self::Active => ":active",
            Self::Focus => ":focus",
            Self::FocusWithin => ":focus-within",
            Self::FocusVisible => ":focus-visible",
            Self::Visited => ":visited",
            Self::Link => ":link",
            Self::AnyLink => ":any-link",
            Self::Target => ":target",
            Self::Enabled => ":enabled",
            Self::Disabled => ":disabled",
            Self::Checked => ":checked",
        })
    }
}

/// Pseudo-elements. Supports `::before`/`::after` (generated content, combined with a
/// `content` declaration) and `::first-letter` (an override style for a limited set of
/// properties). `::first-line` is not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
    FirstLetter,
}

impl parser::PseudoElement for PseudoElement {
    type Impl = SgSelectorImpl;

    fn is_before_or_after(&self) -> bool {
        matches!(self, Self::Before | Self::After)
    }
}

impl ToCss for PseudoElement {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(match self {
            Self::Before => "::before",
            Self::After => "::after",
            Self::FirstLetter => "::first-letter",
        })
    }
}
