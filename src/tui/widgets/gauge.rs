//! Inline bar for a percentage.

/// Block characters from empty to full.
const BLOCKS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// Render `pct` as a bar of `width` cells, for example `████░░░░`.
pub fn bar(pct: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let pct = pct.clamp(0.0, 100.0);
    let filled = (pct / 100.0) * width as f64;
    let full = filled.floor() as usize;
    let frac = filled - full as f64;
    let mut out = String::with_capacity(width);
    for _ in 0..full.min(width) {
        out.push('█');
    }
    if full < width {
        let idx = (frac * 8.0).round() as usize;
        out.push(BLOCKS[idx.min(8)]);
        for _ in (full + 1)..width {
            out.push('░');
        }
    }
    out
}

/// Compact sparkline using `▁▂▃▄▅▆▇█`. Values are scaled to the max in the slice.
pub fn sparkline(values: &[f64], width: usize) -> String {
    const TICKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if width == 0 {
        return String::new();
    }
    if values.is_empty() {
        return " ".repeat(width);
    }
    let max = values.iter().cloned().fold(0.0_f64, f64::max).max(1.0);
    let start = values.len().saturating_sub(width);
    let mut out = String::with_capacity(width);
    for index in 0..width {
        let value = values.get(start + index).copied().unwrap_or(0.0).max(0.0);
        let level = ((value / max) * 7.0).round() as usize;
        out.push(TICKS[level.min(7)]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_and_sparkline_fill_the_width() {
        assert_eq!(bar(0.0, 4).chars().count(), 4);
        assert_eq!(bar(100.0, 4), "████");
        assert_eq!(sparkline(&[1.0, 2.0, 3.0, 4.0], 4).chars().count(), 4);
        assert_eq!(sparkline(&[], 3), "   ");
    }
}
