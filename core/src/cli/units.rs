//! Parsing of numbers with units (`10mm`/`0.5in`/`72pt`/`96px`/`1cm`).
//!
//! Equivalent to wkhtmltopdf's `<unitreal>`. A missing unit is read as mm,
//! matching wkhtmltopdf.
//!
//! The value returned is CSS px (based on 96dpi), the engine's internal unit.

/// 1 inch = 96 CSS px.
const PX_PER_IN: f32 = 96.0;
/// 1 inch = 25.4mm.
const MM_PER_IN: f32 = 25.4;
/// 1 inch = 72pt.
const PT_PER_IN: f32 = 72.0;

/// Convert a length with a unit to CSS px.
///
/// ```text
/// "10mm" -> 37.795 (10 / 25.4 * 96)
/// "1in"  -> 96
/// "72pt" -> 96
/// "96px" -> 96
/// "1cm"  -> 37.795
/// "20"   -> 75.59  (no unit means mm)
/// ```
pub fn parse_length_px(input: &str) -> Result<f32, String> {
    let text = input.trim();
    if text.is_empty() {
        return Err("length is empty".to_string());
    }

    let lower = text.to_ascii_lowercase();
    let (number_part, unit) = split_unit(&lower);

    let value: f32 = number_part
        .trim()
        .parse()
        .map_err(|_| format!("cannot be read as a length: {input}"))?;
    if !value.is_finite() {
        return Err(format!("cannot be read as a length: {input}"));
    }

    let px = match unit {
        // A missing unit means mm, matching wkhtmltopdf.
        "" | "mm" => value / MM_PER_IN * PX_PER_IN,
        "cm" => value * 10.0 / MM_PER_IN * PX_PER_IN,
        "in" => value * PX_PER_IN,
        "pt" => value / PT_PER_IN * PX_PER_IN,
        "px" => value,
        other => return Err(format!("unknown unit: {other} (use one of mm/cm/in/pt/px)")),
    };

    if px < 0.0 {
        return Err(format!("a length cannot be negative: {input}"));
    }
    Ok(px)
}

/// Split off the trailing run of letters as the unit.
fn split_unit(lower: &str) -> (&str, &str) {
    let unit_len = lower
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    lower.split_at(lower.len() - unit_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.01,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn parses_each_supported_unit() {
        approx(parse_length_px("1in").unwrap(), 96.0);
        approx(parse_length_px("72pt").unwrap(), 96.0);
        approx(parse_length_px("96px").unwrap(), 96.0);
        approx(parse_length_px("25.4mm").unwrap(), 96.0);
        approx(parse_length_px("2.54cm").unwrap(), 96.0);
    }

    #[test]
    fn a_bare_number_is_millimeters_like_wkhtmltopdf() {
        approx(parse_length_px("25.4").unwrap(), 96.0);
        approx(parse_length_px("10").unwrap(), 37.795);
    }

    #[test]
    fn accepts_uppercase_and_surrounding_spaces() {
        approx(parse_length_px(" 1IN ").unwrap(), 96.0);
        approx(parse_length_px("10 Mm").unwrap(), 37.795);
    }

    #[test]
    fn zero_is_allowed() {
        approx(parse_length_px("0").unwrap(), 0.0);
        approx(parse_length_px("0mm").unwrap(), 0.0);
    }

    #[test]
    fn rejects_unknown_units_and_garbage() {
        assert!(parse_length_px("10em").is_err());
        assert!(parse_length_px("abc").is_err());
        assert!(parse_length_px("").is_err());
        assert!(parse_length_px("mm").is_err());
    }

    #[test]
    fn rejects_negative_lengths() {
        assert!(parse_length_px("-1mm").is_err());
    }
}
