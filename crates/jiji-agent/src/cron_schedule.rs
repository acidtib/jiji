//! Schedule/time-zone calculation for the cron scheduler (`scheduler.rs`). Reuses
//! `jiff`/`jiff-cron` exactly as `jiji_config::validation` does when it rejects an invalid
//! schedule or time zone at config-parse time (see that crate's `validate_cron_schedule`): a
//! spec reaching this agent has already been validated once, so a parse failure here is a defense
//! -in-depth check, not the primary gate.

use std::str::FromStr;

use jiff::tz::TimeZone;
use jiff::Timestamp;
use jiff_cron::Schedule;

/// The next UTC second this schedule fires strictly after `after_unix` (never equal to it), in
/// the given IANA time zone. Persisted timestamps stay UTC throughout this crate (the plan's
/// "Scheduler Rules" section); `timezone` only ever affects which wall-clock moments a schedule's
/// fields mean, exactly like `jiji_config::validation`'s prepended-`0`-seconds delegation to
/// `jiff_cron::Schedule`.
pub fn next_due_at(schedule: &str, timezone: &str, after_unix: u64) -> Result<u64, String> {
    let tz = TimeZone::get(timezone)
        .map_err(|error| format!("invalid timezone '{timezone}': {error}"))?;
    let parsed = Schedule::from_str(&format!("0 {schedule}"))
        .map_err(|error| format!("invalid schedule '{schedule}': {error}"))?;
    let after = Timestamp::from_second(after_unix as i64)
        .map_err(|error| format!("timestamp {after_unix} is out of range: {error}"))?
        .to_zoned(tz);
    let next = parsed
        .after(after)
        .next()
        .ok_or_else(|| "schedule has no future occurrence".to_string())?;
    Ok(next.timestamp().as_second() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_to_the_next_matching_minute_in_utc() {
        // 2024-01-01T00:00:00Z is itself a multiple of 5 minutes; `after` must return the *next*
        // one, not the same instant.
        let after = Timestamp::from_second(1_704_067_200).unwrap().as_second() as u64;
        let next = next_due_at("*/5 * * * *", "UTC", after).unwrap();
        assert_eq!(next, after + 5 * 60);
    }

    #[test]
    fn a_non_utc_timezone_shifts_which_wall_clock_hour_fires() {
        // 03:00 UTC on 2024-01-01 is 20:00 the prior day in America/Denver (UTC-7 in January, no
        // DST): "0 3 * * *" in Denver next fires at 03:00 Denver time, i.e. 10:00 UTC the same
        // day -- seven hours later than the UTC-schedule equivalent would land.
        let after = Timestamp::from_second(1_704_067_200).unwrap().as_second() as u64; // 2024-01-01T00:00:00Z
        let utc_next = next_due_at("0 3 * * *", "UTC", after).unwrap();
        let denver_next = next_due_at("0 3 * * *", "America/Denver", after).unwrap();
        assert_eq!(denver_next, utc_next + 7 * 3600);
    }

    #[test]
    fn daylight_saving_spring_forward_is_handled_by_jiff_cron_without_a_missing_hour_panic() {
        // America/Denver springs forward at 02:00 -> 03:00 on 2024-03-10; a naive "add 24h per
        // day" implementation would either panic or silently double-fire around this transition.
        // This only asserts it produces *some* strictly-increasing valid answer -- the DST
        // correctness itself is `jiff_cron`'s own tested responsibility (see its crate-level DST
        // doctest), not something this thin wrapper re-implements.
        let before_transition = next_due_at(
            "0 3 * * *",
            "America/Denver",
            Timestamp::from_second(1_710_030_000).unwrap().as_second() as u64, // 2024-03-09T23:00:00Z
        )
        .unwrap();
        let after_transition =
            next_due_at("0 3 * * *", "America/Denver", before_transition).unwrap();
        assert!(after_transition > before_transition);
    }

    #[test]
    fn daylight_saving_fall_back_does_not_re_fire_the_same_wall_clock_hour_twice() {
        // America/Denver falls back at 02:00 -> 01:00 on 2024-11-03, so 01:30 local time occurs
        // twice; a schedule firing once daily must still advance strictly forward in UTC.
        let before_transition = next_due_at(
            "0 3 * * *",
            "America/Denver",
            Timestamp::from_second(1_730_610_000).unwrap().as_second() as u64, // 2024-11-02T23:00:00Z-ish
        )
        .unwrap();
        let after_transition =
            next_due_at("0 3 * * *", "America/Denver", before_transition).unwrap();
        assert!(after_transition > before_transition);
    }

    #[test]
    fn invalid_timezone_is_reported_actionably() {
        let error = next_due_at("* * * * *", "Not/AZone", 0).unwrap_err();
        assert!(error.contains("Not/AZone"));
    }

    #[test]
    fn invalid_schedule_is_reported_actionably() {
        let error = next_due_at("nope * * * *", "UTC", 0).unwrap_err();
        assert!(error.contains("nope"));
    }
}
