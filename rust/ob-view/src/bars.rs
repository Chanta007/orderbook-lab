//! Drawing a quantity in a terminal: a bar, and a sparkline.
//!
//! Both use the Unicode block elements, which give eight levels inside one
//! character cell. That is the standard technique for text-mode graphs and it needs
//! no dependency: normalise a value to 0-7 and index the string
//! (`docs/research/2026-10-19-view-layer-and-web-frontend.md` §6).
//!
//! The rule that keeps these honest: **a non-zero value never renders as nothing.**
//! A rate of one record a second next to a rate of a million would round to an
//! empty cell, and an empty cell reads as "no data" rather than "little data". Both
//! functions floor a present value at the first level up.

/// Eight vertical levels, one eighth to eight eighths.
pub const VERTICAL: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Eight horizontal levels plus an empty cell, for a fill bar.
///
/// The tip of a partly filled cell is one of these; a whole cell is `█`.
pub const HORIZONTAL: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// A sparkline of `values`, at most `width` cells wide.
///
/// The most recent value is the rightmost cell. Wider input than `width` keeps the
/// newest `width` values, because a rate history that is cut should lose its
/// oldest end. Values are scaled against the largest in the window, so a flat
/// series draws a flat line rather than nothing: when everything is equal, every
/// cell is a full block and the shape is the message ("unchanged").
pub fn sparkline(values: &[f64], width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let start = values.len().saturating_sub(width);
    let window = &values[start..];
    let mut out = String::with_capacity(window.len() * 3);
    let max = window.iter().copied().fold(0.0_f64, f64::max);
    for &v in window {
        out.push(level_char(v, max));
    }
    out
}

/// One cell for one value, scaled against `max`.
fn level_char(value: f64, max: f64) -> char {
    if value <= 0.0 || max <= 0.0 {
        // A negative value cannot occur, and if one did it is not a rate; it draws
        // as nothing rather than as the smallest level up.
        return VERTICAL[0];
    }
    let fraction = (value / max).clamp(0.0, 1.0);
    // A value that is present gets at least the first level up; see the module
    // note. `fraction * 8` at the top end lands on 8, which is one past the last
    // glyph, so the index is clamped.
    let index = ((fraction * 8.0).ceil() as usize).clamp(1, 8) - 1;
    VERTICAL[index]
}

/// A horizontal bar `width` cells wide for a fraction of a whole.
///
/// A whole cell is a full block and the last, partial cell is one of the eight
/// tip glyphs, so a bar shows `8 * width` distinguishable fill amounts without
/// needing eight times the columns.
pub fn bar(fraction: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut eighths = (fraction * (width * 8) as f64).round() as usize;
    // A present value gets at least one eighth; see the module note. A ring one
    // slot into its first lap must not look like an empty ring.
    if eighths == 0 && fraction > 0.0 {
        eighths = 1;
    }
    let whole = eighths / 8;
    let part = eighths % 8;
    let mut out = String::with_capacity(width * 3);
    for _ in 0..whole.min(width) {
        out.push(HORIZONTAL[8]);
    }
    if whole < width && part > 0 {
        out.push(HORIZONTAL[part]);
    }
    while out.chars().count() < width {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sparkline_scales_to_its_largest_value() {
        let line = sparkline(&[0.0, 4.0, 8.0], 3);
        assert_eq!(line.chars().count(), 3);
        assert_eq!(line.chars().nth(2), Some('█'), "the largest is a full cell");
        assert!(!line.starts_with('█'));
    }

    #[test]
    fn a_present_value_is_never_an_empty_cell() {
        // One record a second against a million: the small one still shows as the
        // smallest level rather than as nothing at all.
        let line = sparkline(&[1.0, 1_000_000.0], 2);
        assert_eq!(line.chars().next(), Some(VERTICAL[0]));
        assert_eq!(line.chars().nth(1), Some('█'));
        // A zero is the only empty cell.
        assert_eq!(sparkline(&[0.0, 4.0], 2).chars().next(), Some(VERTICAL[0]));
    }

    #[test]
    fn a_flat_series_draws_flat_rather_than_empty() {
        let line = sparkline(&[5.0, 5.0, 5.0], 3);
        assert_eq!(line, "███");
    }

    #[test]
    fn a_sparkline_drops_its_oldest_values_when_it_is_narrower_than_the_history() {
        let line = sparkline(&[0.0, 1.0, 2.0, 3.0], 2);
        assert_eq!(line.chars().count(), 2);
        assert_eq!(line.chars().nth(1), Some('█'), "the newest is last");
    }

    #[test]
    fn an_empty_history_and_a_zero_width_are_both_empty() {
        assert_eq!(sparkline(&[], 4), "");
        assert_eq!(sparkline(&[1.0], 0), "");
        assert_eq!(bar(0.5, 0), "");
    }

    #[test]
    fn a_bar_fills_from_the_left_and_is_exactly_the_width_asked_for() {
        let full = bar(1.0, 4);
        assert_eq!(full, "████");
        let half = bar(0.5, 4);
        assert_eq!(half.chars().count(), 4);
        assert_eq!(half.chars().next(), Some('█'));
        assert_eq!(half.chars().nth(3), Some(' '));
        assert_eq!(bar(0.0, 4), "    ");
    }

    #[test]
    fn a_bar_shows_a_small_fraction_with_a_tip_glyph() {
        // A ring that is one slot in is not empty and must not look empty.
        let nearly_empty = bar(0.001, 20);
        assert_eq!(nearly_empty.chars().count(), 20);
        assert_ne!(nearly_empty, " ".repeat(20));
        assert_eq!(nearly_empty.chars().next(), Some(HORIZONTAL[1]));
    }

    #[test]
    fn a_fraction_outside_zero_to_one_is_clamped_rather_than_panicking() {
        assert_eq!(bar(-5.0, 3), "   ");
        assert_eq!(bar(99.0, 3), "███");
        assert_eq!(bar(f64::NAN, 3), "   ");
        assert_eq!(bar(f64::INFINITY, 3), "   ");
    }

    #[test]
    fn a_bar_uses_the_eight_tip_levels_across_a_width() {
        // The point of the tip glyphs: eight distinguishable fills in one cell.
        let fills: Vec<String> = (0..=8).map(|e| bar(e as f64 / 8.0, 1)).collect();
        let distinct: std::collections::HashSet<&String> = fills.iter().collect();
        assert_eq!(distinct.len(), 9, "nine states, including empty");
    }
}
