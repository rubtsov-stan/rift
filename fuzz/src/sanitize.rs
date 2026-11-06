//! The terminal interface's line sanitizer alone, `render::sanitize`,
//! through `rift_tui::fuzz_access`.

use rift_tui::fuzz_access::sanitize;

use crate::tui::hidden;

/// Sanitizes the input as one line with the column budget from the first
/// byte (bit 7 of the second makes it unlimited) and the cut flag from
/// bit 0 of the second.
///
/// Properties: the output holds no hidden character (see
/// [`hidden`](crate::tui::hidden)); it takes one column per character,
/// within the budget (an escape that does not fit spends the rest); a smaller budget gives
/// a prefix of the unlimited output; a line of letters, digits, spaces
/// and ASCII punctuation comes out as it went in, without its terminator.
///
/// # Panics
///
/// On any property violation.
pub fn check(data: &[u8]) {
    let (c0, c1, line) = match data {
        [c0, c1, line @ ..] => (*c0, *c1, line),
        _ => return,
    };
    let cut = c1 & 1 != 0;
    let budget = if c1 & 0x80 != 0 {
        usize::MAX
    } else {
        usize::from(c0)
    };
    let mut out = String::new();
    let mut left = budget;
    sanitize(line, cut, &mut out, &mut left);
    if let Some(c) = out.chars().find(|&c| hidden(c)) {
        panic!("output holds U+{:04X}", c as u32);
    }
    assert!(left <= budget, "budget grew");
    // An escape that does not fit ends the text and spends what is left.
    let taken = out.chars().count();
    assert!(
        taken <= budget - left,
        "{taken} characters in {} columns",
        budget - left
    );
    assert!(left == 0 || taken == budget - left, "columns taken");

    let mut full = String::new();
    let mut unlimited = usize::MAX;
    sanitize(line, cut, &mut full, &mut unlimited);
    assert!(
        full.starts_with(&out),
        "{out:?} is not a prefix of {full:?}"
    );

    let body = line.strip_suffix(b"\n").unwrap_or(line);
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    if let Ok(text) = std::str::from_utf8(body)
        && text
            .chars()
            .all(|c| c == ' ' || c.is_ascii_graphic() || c.is_alphanumeric())
    {
        assert_eq!(full, text, "plain text changed");
    }
}
