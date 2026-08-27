/// Clamp `value` into `lo..=hi`.
pub fn clamp(value: i64, lo: i64, hi: i64) -> i64 {
    if value < lo {
        lo
    } else if value > hi {
        hi
    } else {
        value
    }
}

/// Whether `value` is within `lo..=hi`.
pub fn in_range(value: i64, lo: i64, hi: i64) -> bool {
    value >= lo && value <= hi
}
