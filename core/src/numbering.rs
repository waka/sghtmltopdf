//! Conversion to Roman numerals and alphabetic notation.
//! Shared logic used both by `layout::box_tree` (markers for `list-style-type`)
//! and by `style::computed` (`content: counter`).

/// Convert an Arabic number to an uppercase Roman numeral. Values outside 1-3999,
/// the range CSS 2.1 gives meaning to, are returned as Arabic digits.
pub fn to_roman(n: usize) -> String {
    const VALUES: [(usize, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    if n == 0 || n > 3999 {
        return n.to_string();
    }
    let mut remaining = n;
    let mut out = String::new();
    for (value, symbol) in VALUES {
        while remaining >= value {
            out.push_str(symbol);
            remaining -= value;
        }
    }
    out
}

/// Convert an Arabic number to letters (uppercase, 1-indexed: 1 -> A, 26 -> Z, 27 -> AA).
pub fn to_alpha(n: usize) -> String {
    if n == 0 {
        return n.to_string();
    }
    let mut remaining = n;
    let mut letters = Vec::new();
    while remaining > 0 {
        remaining -= 1;
        letters.push((b'A' + (remaining % 26) as u8) as char);
        remaining /= 26;
    }
    letters.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_roman_handles_typical_values_and_out_of_range_fallback() {
        assert_eq!(to_roman(4), "IV");
        assert_eq!(to_roman(1994), "MCMXCIV");
        assert_eq!(to_roman(3999), "MMMCMXCIX");
        assert_eq!(to_roman(0), "0");
        assert_eq!(to_roman(4000), "4000");
    }

    #[test]
    fn to_alpha_handles_typical_values_and_wraps_past_z() {
        assert_eq!(to_alpha(1), "A");
        assert_eq!(to_alpha(26), "Z");
        assert_eq!(to_alpha(27), "AA");
        assert_eq!(to_alpha(0), "0");
    }
}
