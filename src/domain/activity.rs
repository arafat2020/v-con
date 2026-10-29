//! Connection history used to rank and group hosts.
//!
//! The state file stores the raw counts and timestamps. These functions turn
//! that history into a frecency score and a short age label for the table.

use chrono::{DateTime, TimeDelta, Utc};

/// How many hosts the `recent` group keeps. Newest connections come first.
pub const RECENT_LIMIT: usize = 20;

/// Rank a host by how often and how recently it was opened.
///
/// The weight is 100 within four hours, 70 within a day, 50 within a week,
/// 30 within 30 days, and 10 after that. The score is `connect_count * weight`.
/// A host that has never been opened scores 0.
pub fn frecency(
    connect_count: u64,
    last_connected: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> u64 {
    let Some(last_connected) = last_connected else {
        return 0;
    };
    if connect_count == 0 {
        return 0;
    }
    let age = now.signed_duration_since(last_connected);
    let weight = if age < TimeDelta::zero() || age < TimeDelta::hours(4) {
        100
    } else if age < TimeDelta::days(1) {
        70
    } else if age < TimeDelta::days(7) {
        50
    } else if age < TimeDelta::days(30) {
        30
    } else {
        10
    };
    connect_count.saturating_mul(weight)
}

/// A four-character age such as `now`, `12m`, `3h`, `6d`, or `2mo`.
pub fn compact_age(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = now.signed_duration_since(then).num_seconds().max(0);
    if seconds < 60 {
        "now".to_string()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else if seconds < 86_400 * 30 {
        format!("{}d", seconds / 86_400)
    } else if seconds < 86_400 * 365 {
        format!("{}mo", seconds / (86_400 * 30))
    } else {
        format!("{}y", seconds / (86_400 * 365))
    }
}

/// Parse an RFC3339 timestamp from the state file.
pub fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|stamp| stamp.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        parse_timestamp(text).unwrap()
    }

    #[test]
    fn recent_use_outranks_old_use() {
        let now = at("2026-10-26T12:00:00Z");
        let fresh = frecency(1, Some(at("2026-10-26T11:00:00Z")), now);
        let week = frecency(1, Some(at("2026-10-20T12:00:00Z")), now);
        let stale = frecency(4, Some(at("2026-01-01T12:00:00Z")), now);
        assert!(fresh > week);
        assert_eq!(week, 50);
        assert_eq!(stale, 40);
        assert_eq!(frecency(0, Some(now), now), 0);
        assert_eq!(frecency(3, None, now), 0);
    }

    #[test]
    fn ages_stay_short_enough_for_the_table() {
        let now = at("2026-10-26T12:00:00Z");
        assert_eq!(compact_age(at("2026-10-26T11:59:30Z"), now), "now");
        assert_eq!(compact_age(at("2026-10-26T11:00:00Z"), now), "1h");
        assert_eq!(compact_age(at("2026-10-24T12:00:00Z"), now), "2d");
        assert!(compact_age(at("2026-01-01T12:00:00Z"), now).len() <= 4);
    }
}
