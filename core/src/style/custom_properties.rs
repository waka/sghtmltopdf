//! Support for CSS Custom Properties (`--foo`/`var()`) by text substitution before parsing.
//!
//! It uses the same pattern as `style/import.rs::resolve_imports` (`@import` expansion):
//! locate byte ranges by scanning tokens, then rebuild the substituted string from the
//! original text. Note that this is not a cascade- or inheritance-based implementation, but
//! a simple text substitution over one flat namespace across the whole document.
//! `parse_stylesheet` itself receives the text after it has been through this module, so it
//! never has to know about `var()` or custom properties at all.

use std::collections::HashMap;

use cssparser::{ParseError, Parser, ParserInput, Token};

/// Iteration cap for repeating until both the `var()`s inside `declared` itself and the
/// application across the document settle (the same idea as `MAX_IMPORT_DEPTH`).
const MAX_SUBSTITUTION_ITERATIONS: u32 = 8;

/// Return the CSS text after resolving, as text, every `--foo: value;` declaration and
/// every `var(--foo, fallback)` call in `css`.
pub fn substitute_custom_properties(css: &str) -> String {
    let mut declared = collect_custom_properties(css);

    // Resolve until it settles, so a custom property referring to another (`--b: var(--a)`)
    // works regardless of declaration order.
    for _ in 0..MAX_SUBSTITUTION_ITERATIONS {
        let mut changed = false;
        let next: HashMap<String, String> = declared
            .iter()
            .map(|(name, value)| {
                let substituted = substitute_var_calls(value, &declared);
                changed |= substituted != *value;
                (name.clone(), substituted)
            })
            .collect();
        declared = next;
        if !changed {
            break;
        }
    }

    // Apply across the whole document. Repeat until it settles so a `var()` left inside a
    // fallback value (`var(--a, var(--b))` where `--a` is undefined) is resolved too.
    let mut result = css.to_string();
    for _ in 0..MAX_SUBSTITUTION_ITERATIONS {
        let next = substitute_var_calls(&result, &declared);
        if next == result {
            break;
        }
        result = next;
    }
    result
}

/// If a token starts a block (`{`/`(`/`[`/`func(`), then by cssparser's rules its contents
/// are invisible unless entered explicitly with `Parser::parse_nested_block` (the next
/// `next()` call skips automatically to the end of the block).
/// `--foo: value` only ever appears inside a `{ }` rule body (including at-rule blocks such
/// as `@page` and `@media`), so both collection and substitution have to descend
/// recursively into every block type.
fn is_block_start(token: &Token) -> bool {
    matches!(
        token,
        Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock
    )
}

/// Scan `css` and collect its `--foo: value;` declarations (token scan, no I/O).
/// With several declarations of the same name, the last in text order wins (selector
/// specificity and origin are not considered).
fn collect_custom_properties(css: &str) -> HashMap<String, String> {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut declared = HashMap::new();
    collect_custom_properties_in_scope(&mut parser, css, &mut declared);
    declared
}

/// Scan `parser`'s current scope (the whole document, or the inside of a block entered via
/// `parse_nested_block`). On a block-start token, recurse into the contents as well.
fn collect_custom_properties_in_scope(
    parser: &mut Parser,
    css: &str,
    declared: &mut HashMap<String, String>,
) {
    loop {
        match parser.next() {
            Ok(Token::Ident(name)) if name.starts_with("--") => {
                let name = name.to_string();
                if parser.try_parse(|input| input.expect_colon()).is_err() {
                    continue;
                }
                let value_start = parser.position().byte_index();
                let value_end = loop {
                    let state = parser.state();
                    match parser.next() {
                        Ok(Token::Semicolon) => break state.position().byte_index(),
                        Ok(Token::CloseCurlyBracket) => {
                            // Do not consume this block's closing `}`; hand it back to the outer loop.
                            parser.reset(&state);
                            break state.position().byte_index();
                        }
                        Ok(token) if is_block_start(token) => {
                            // Consume it right here (deferring with `continue` would make
                            // the `state()` captured on the next iteration land at an
                            // inaccurate position, before the pending block skip). The
                            // contents are just part of a value, so simply skip to the end.
                            let _ = parser.parse_nested_block(
                                |input| -> Result<(), ParseError<'_, ()>> {
                                    while input.next().is_ok() {}
                                    Ok(())
                                },
                            );
                            continue;
                        }
                        Ok(_) => continue,
                        Err(_) => break parser.position().byte_index(),
                    }
                };
                let value = css[value_start..value_end].trim();
                if !value.is_empty() {
                    declared.insert(name, value.to_string());
                }
            }
            Ok(token) if is_block_start(token) => {
                let _ = parser.parse_nested_block(|input| -> Result<(), ParseError<'_, ()>> {
                    collect_custom_properties_in_scope(input, css, declared);
                    Ok(())
                });
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

/// Substitute `var(--foo)`/`var(--foo, fallback)` in `css` once, using `declared`
/// (resolving `var()` inside a nested fallback is left to the iteration in
/// [`substitute_custom_properties`]).
fn substitute_var_calls(css: &str, declared: &HashMap<String, String>) -> String {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut result = String::with_capacity(css.len());
    let mut cursor = 0usize;
    substitute_var_calls_in_scope(&mut parser, css, declared, &mut result, &mut cursor);
    result.push_str(&css[cursor..]);
    result
}

/// Scan `parser`'s current scope and write the substituted text of each `var()` found into
/// `result` (`cursor` is the start of what has not been written yet in `css`).
/// On a block-start token, recurse into the contents as well (the `{`/`(`/`[` itself is not
/// written here; leaving `cursor` where it is lets the original text flow through together
/// at the next write).
fn substitute_var_calls_in_scope(
    parser: &mut Parser,
    css: &str,
    declared: &HashMap<String, String>,
    result: &mut String,
    cursor: &mut usize,
) {
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

                result.push_str(&css[*cursor..call_start]);
                let replacement = match var_name {
                    Ok(name) => match declared.get(&name) {
                        Some(value) => value.clone(),
                        None => match fallback_range {
                            Some((start, end)) => css[start..end].trim().to_string(),
                            // Undefined and with no fallback: leave the original text in
                            // place (the property parser downstream silently ignores it as
                            // an unknown token).
                            None => css[call_start..call_end].to_string(),
                        },
                    },
                    Err(_) => css[call_start..call_end].to_string(),
                };
                result.push_str(&replacement);
                *cursor = call_end;
            }
            Ok(token) if is_block_start(token) => {
                let _ = parser.parse_nested_block(|input| -> Result<(), ParseError<'_, ()>> {
                    substitute_var_calls_in_scope(input, css, declared, result, cursor);
                    Ok(())
                });
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_a_simple_custom_property() {
        let css = ":root { --main-color: red; } p { color: var(--main-color); }";
        let out = substitute_custom_properties(css);
        assert!(out.contains("color: red"), "{out}");
        assert!(!out.contains("var("), "{out}");
    }

    #[test]
    fn uses_fallback_when_the_custom_property_is_undefined() {
        let css = "p { color: var(--undefined, blue); }";
        let out = substitute_custom_properties(css);
        assert!(out.contains("color: blue"), "{out}");
    }

    #[test]
    fn leaves_unresolved_var_untouched_when_no_fallback_exists() {
        let css = "p { color: var(--undefined); }";
        let out = substitute_custom_properties(css);
        assert!(out.contains("var(--undefined)"), "{out}");
    }

    #[test]
    fn resolves_a_custom_property_that_references_another_one() {
        let css = ":root { --base: 8px; --gap: var(--base); } div { margin: var(--gap); }";
        let out = substitute_custom_properties(css);
        assert!(out.contains("margin: 8px"), "{out}");
    }

    #[test]
    fn later_declaration_wins_regardless_of_selector_scope() {
        let css = ".a { --x: 1px; } .b { --x: 2px; } p { width: var(--x); }";
        let out = substitute_custom_properties(css);
        assert!(out.contains("width: 2px"), "{out}");
    }

    #[test]
    fn preserves_surrounding_text_and_whitespace() {
        let css = "p {\n  color: var(--c, green);\n  font-size: 12px;\n}";
        let out = substitute_custom_properties(css);
        assert!(out.contains("font-size: 12px"), "{out}");
        assert!(out.contains("color: green"), "{out}");
    }

    #[test]
    fn resolves_nested_var_inside_a_fallback() {
        let css = ":root { --b: navy; } p { color: var(--a, var(--b)); }";
        let out = substitute_custom_properties(css);
        assert!(out.contains("color: navy"), "{out}");
    }

    #[test]
    fn ignores_custom_property_like_text_inside_string_literals_and_comments() {
        let css = "p { content: \"--not-a-var: x;\"; /* --also-not: y; */ color: red; }";
        let out = substitute_custom_properties(css);
        assert!(out.contains("\"--not-a-var: x;\""), "{out}");
        assert!(out.contains("color: red"), "{out}");
    }
}
