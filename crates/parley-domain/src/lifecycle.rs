//! Contact thread lifecycle and civil deadline resolution (RFC-0002 BE-08).
//!
//! This module is pure decision policy. It owns the thread transition table,
//! the opportunity consequence rules and the exclusive civil expiry resolver.
//! Deadlines are resolved through the owner's IANA timezone so an inclusive
//! date means "through that local date": the expiry is the first instant of
//! the *following* local date, never `now + 24h`. A civil transition is
//! resolved by the timezone library, and an explicit UTC timestamp is used
//! verbatim as the exclusive expiry.

use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, LocalResult, NaiveDate, NaiveDateTime, Offset,
    TimeZone, Timelike, Utc,
};
use chrono_tz::Tz;

/// The default reply window: seven calendar days in the owner's timezone.
pub const DEFAULT_REPLY_WINDOW_DAYS: i64 = 7;

/// Default capacity ledger identity used by the scarcity gate and the send
/// charge (initial calibration, RFC-0002 BE-08 / RFC-0001 §10.7).
pub const DEFAULT_CAPACITY_PERIOD: &str = "quarter";
pub const DEFAULT_CAPACITY_UNIT: &str = "project slots";

/// Contact thread states (BE-08 thread result column).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThreadState {
    Waiting,
    Conversing,
    TimedOut,
    ClosedDeclined,
    Resolved,
    Dormant,
}

/// Events that move a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadEvent {
    Send,
    NonDecliningReply,
    Untimed,
    Decline,
    Resolve,
    Park,
    Reopen,
}

/// Apply one thread event. `None` means the event is not allowed in that
/// state (the caller must reject it).
pub fn apply_thread_event(state: ThreadState, event: ThreadEvent) -> Option<ThreadState> {
    Some(match (state, event) {
        (ThreadState::Waiting | ThreadState::Conversing, ThreadEvent::Send) => ThreadState::Waiting,
        (ThreadState::Waiting | ThreadState::Conversing, ThreadEvent::NonDecliningReply) => {
            ThreadState::Conversing
        }
        (ThreadState::Waiting, ThreadEvent::Untimed) => ThreadState::TimedOut,
        (ThreadState::Waiting | ThreadState::Conversing, ThreadEvent::Decline) => {
            ThreadState::ClosedDeclined
        }
        (ThreadState::Waiting | ThreadState::Conversing, ThreadEvent::Resolve) => {
            ThreadState::Resolved
        }
        (ThreadState::Waiting | ThreadState::Conversing, ThreadEvent::Park) => ThreadState::Dormant,
        (
            ThreadState::TimedOut | ThreadState::ClosedDeclined | ThreadState::Resolved,
            ThreadEvent::Reopen,
        ) => ThreadState::Waiting,
        _ => return None,
    })
}

/// Opportunity consequence of a set of live thread states (BE-08 opportunity
/// consequence column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpportunityConsequence {
    Active,
    CloseTimedOut,
    CloseDeclined,
}

pub fn opportunity_consequence(thread_states: &[ThreadState]) -> OpportunityConsequence {
    if thread_states
        .iter()
        .any(|state| matches!(state, ThreadState::Waiting | ThreadState::Conversing))
    {
        return OpportunityConsequence::Active;
    }
    if thread_states.contains(&ThreadState::TimedOut) {
        return OpportunityConsequence::CloseTimedOut;
    }
    if thread_states.contains(&ThreadState::ClosedDeclined) {
        return OpportunityConsequence::CloseDeclined;
    }
    OpportunityConsequence::Active
}

/// How the response deadline is expressed by the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeadlineKind {
    /// A whole number of calendar days from the send instant in the owner's
    /// timezone.
    RelativeDays { days: i64 },
    /// An inclusive local date: through that local date, exclusive expiry is
    /// the first instant of the following local date.
    Date { date: NaiveDate },
    /// An explicit exclusive UTC timestamp.
    Timestamp { at: DateTime<Utc> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadlineSpec {
    pub kind: DeadlineKind,
    pub zone: Tz,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDeadline {
    /// Exclusive UTC expiry instant.
    pub expiry: DateTime<Utc>,
    /// The entered civil date, when the owner supplied one.
    pub entered_date: Option<NaiveDate>,
    /// IANA zone used for the resolution.
    pub zone: Tz,
}

/// Resolve a local civil time to a UTC instant (RFC-0002 BQ5/BE-08).
///
/// A fall-back overlap (fold) resolves to its first instant. A non-existent
/// civil time inside a spring-forward gap resolves to the first representable
/// instant on or after it: the instant at which the wall clock reads the
/// target civil time under the pre-transition offset, which is exactly where
/// the gap closes (e.g. America/Santiago midnight on 2026-09-06 → 04:00Z).
fn local_to_instant(zone: &Tz, local: &NaiveDateTime) -> DateTime<Utc> {
    match zone.from_local_datetime(local) {
        LocalResult::Single(dt) => dt.with_timezone(&Utc),
        LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
        LocalResult::None => {
            // Spring-forward gap: `local` never occurs. Resolve to the
            // equivalent instant under the offset that applied before the
            // gap — the wall clock reads `local` right after the gap
            // closes, which is the convention of mainstream time libraries
            // and keeps minute/second shape. Find that offset by walking
            // back to a representable civil time (real-world gaps are at
            // most two hours).
            let mut probe = *local;
            let mut offset = None;
            for _ in 0..240 {
                probe -= ChronoDuration::minutes(1);
                if let LocalResult::Single(o) = zone.offset_from_local_datetime(&probe) {
                    offset = Some(o);
                    break;
                }
            }
            let offset = offset.expect("a representable civil time exists within four hours");
            Utc.from_utc_datetime(&(*local - offset.fix()))
        }
    }
}

pub fn resolve_deadline(spec: &DeadlineSpec, now_utc: DateTime<Utc>) -> ResolvedDeadline {
    match &spec.kind {
        DeadlineKind::Timestamp { at } => ResolvedDeadline {
            expiry: *at,
            entered_date: None,
            zone: spec.zone,
        },
        DeadlineKind::Date { date } => {
            // "Inclusive through that local date": expiry is the first
            // instant of the following local date. Civil date arithmetic
            // (`succ_opt`) is used, never a fixed 24h addition. A DST fold
            // resolves to the earliest instant; a spring-forward gap resolves
            // to the first representable instant after it.
            let next_local_midnight = date
                .succ_opt()
                .expect("the day after any civil date exists")
                .and_hms_opt(0, 0, 0)
                .expect("midnight");
            let expiry = local_to_instant(&spec.zone, &next_local_midnight);
            ResolvedDeadline {
                expiry,
                entered_date: Some(*date),
                zone: spec.zone,
            }
        }
        DeadlineKind::RelativeDays { days } => {
            let local_now = spec.zone.from_utc_datetime(&now_utc.naive_utc());
            let target_date = local_now
                .date_naive()
                .checked_add_signed(ChronoDuration::days(*days));
            let target_local = match target_date {
                Some(date) => date
                    .and_hms_opt(local_now.hour(), local_now.minute(), local_now.second())
                    .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).expect("midnight")),
                None => local_now.naive_local(),
            };
            let expiry = local_to_instant(&spec.zone, &target_local);
            ResolvedDeadline {
                expiry,
                entered_date: None,
                zone: spec.zone,
            }
        }
    }
}

/// Advance the ask ladder only upward; replaying an outcome can never move the
/// rung backwards or double-increment from the same value.
pub fn advance_rung(current: u32, accepted: u32) -> u32 {
    current.max(accepted)
}

/// Whole business days (Monday–Friday) between two civil dates. The count is
/// the number of business days strictly after `from` and on or before `to`.
pub fn business_days_between(from: NaiveDate, to: NaiveDate) -> i64 {
    let mut days = 0i64;
    let mut cursor = from;
    while cursor < to {
        cursor = cursor.succ_opt().expect("civil date successor");
        if !matches!(
            cursor.weekday(),
            chrono::Weekday::Sat | chrono::Weekday::Sun
        ) {
            days += 1;
        }
    }
    days
}

/// Add `days` business days to a civil date, skipping weekends. A non-positive
/// count returns the input date unchanged.
pub fn add_business_days(date: NaiveDate, days: i64) -> NaiveDate {
    let mut remaining = days.max(0);
    let mut cursor = date;
    while remaining > 0 {
        cursor = cursor.succ_opt().expect("civil date successor");
        if !matches!(
            cursor.weekday(),
            chrono::Weekday::Sat | chrono::Weekday::Sun
        ) {
            remaining -= 1;
        }
    }
    cursor
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, s).single().unwrap()
    }

    #[test]
    fn thread_transition_table_is_exhaustive_and_precedence_correct() {
        assert_eq!(
            apply_thread_event(ThreadState::Waiting, ThreadEvent::Send),
            Some(ThreadState::Waiting)
        );
        assert_eq!(
            apply_thread_event(ThreadState::Conversing, ThreadEvent::Send),
            Some(ThreadState::Waiting)
        );
        assert_eq!(
            apply_thread_event(ThreadState::Waiting, ThreadEvent::NonDecliningReply),
            Some(ThreadState::Conversing)
        );
        assert_eq!(
            apply_thread_event(ThreadState::Waiting, ThreadEvent::Untimed),
            Some(ThreadState::TimedOut)
        );
        assert_eq!(
            apply_thread_event(ThreadState::Conversing, ThreadEvent::Untimed),
            None
        );
        assert_eq!(
            apply_thread_event(ThreadState::Waiting, ThreadEvent::Decline),
            Some(ThreadState::ClosedDeclined)
        );
        assert_eq!(
            apply_thread_event(ThreadState::TimedOut, ThreadEvent::Decline),
            None
        );
        assert_eq!(
            apply_thread_event(ThreadState::TimedOut, ThreadEvent::Reopen),
            Some(ThreadState::Waiting)
        );
        assert_eq!(
            apply_thread_event(ThreadState::ClosedDeclined, ThreadEvent::Reopen),
            Some(ThreadState::Waiting)
        );
        assert_eq!(
            apply_thread_event(ThreadState::Resolved, ThreadEvent::Reopen),
            Some(ThreadState::Waiting)
        );
        assert_eq!(
            apply_thread_event(ThreadState::Waiting, ThreadEvent::Reopen),
            None
        );
        assert_eq!(
            apply_thread_event(ThreadState::Dormant, ThreadEvent::Send),
            None
        );
    }

    #[test]
    fn opportunity_consequence_prefers_active_then_timed_out_then_declined() {
        assert_eq!(
            opportunity_consequence(&[ThreadState::Waiting, ThreadState::TimedOut]),
            OpportunityConsequence::Active
        );
        assert_eq!(
            opportunity_consequence(&[ThreadState::Conversing, ThreadState::ClosedDeclined]),
            OpportunityConsequence::Active
        );
        assert_eq!(
            opportunity_consequence(&[ThreadState::TimedOut, ThreadState::ClosedDeclined]),
            OpportunityConsequence::CloseTimedOut
        );
        assert_eq!(
            opportunity_consequence(&[ThreadState::ClosedDeclined]),
            OpportunityConsequence::CloseDeclined
        );
        assert_eq!(
            opportunity_consequence(&[ThreadState::Resolved]),
            OpportunityConsequence::Active
        );
    }

    #[test]
    fn date_deadline_is_first_instant_of_following_local_date() {
        // 2026-11-21 inclusive → expiry 2026-11-22T00:00:00Z in UTC.
        let spec = DeadlineSpec {
            kind: DeadlineKind::Date {
                date: NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
            },
            zone: chrono_tz::UTC,
        };
        let resolved = resolve_deadline(&spec, utc(2026, 11, 21, 12, 0, 0));
        assert_eq!(resolved.expiry, utc(2026, 11, 22, 0, 0, 0));
        assert_eq!(
            resolved.entered_date,
            Some(NaiveDate::from_ymd_opt(2026, 11, 21).unwrap())
        );
        assert_eq!(resolved.zone, chrono_tz::UTC);
    }

    #[test]
    fn date_deadline_respects_zone_not_24h_arithmetic() {
        // New York is UTC-5 in November: 2026-11-21 inclusive expires at
        // 2026-11-22T05:00:00Z (local midnight in New York), which is NOT
        // 24h after any instant of the 21st.
        let spec = DeadlineSpec {
            kind: DeadlineKind::Date {
                date: NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
            },
            zone: chrono_tz::America::New_York,
        };
        let resolved = resolve_deadline(&spec, utc(2026, 11, 21, 12, 0, 0));
        assert_eq!(resolved.expiry, utc(2026, 11, 22, 5, 0, 0));
    }

    #[test]
    fn dst_spring_forward_uses_tz_resolution_never_24h() {
        // 2026-03-08 is the US spring-forward date. Inclusive through
        // 2026-03-08 local → expiry is 2026-03-09T04:00:00Z (Eastern DST).
        let spec = DeadlineSpec {
            kind: DeadlineKind::Date {
                date: NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(),
            },
            zone: chrono_tz::America::New_York,
        };
        let resolved = resolve_deadline(&spec, utc(2026, 3, 1, 0, 0, 0));
        assert_eq!(resolved.expiry, utc(2026, 3, 9, 4, 0, 0));
    }

    #[test]
    fn relative_days_cross_month_boundary_in_civil_days() {
        // 2026-01-30 + 3 calendar days = 2026-02-02 (not 72 hours).
        let spec = DeadlineSpec {
            kind: DeadlineKind::RelativeDays { days: 3 },
            zone: chrono_tz::UTC,
        };
        let resolved = resolve_deadline(&spec, utc(2026, 1, 30, 10, 0, 0));
        assert_eq!(resolved.expiry, utc(2026, 2, 2, 10, 0, 0));
    }

    #[test]
    fn timestamp_is_used_verbatim_as_exclusive_expiry() {
        let at = utc(2026, 11, 21, 23, 59, 59);
        let spec = DeadlineSpec {
            kind: DeadlineKind::Timestamp { at },
            zone: chrono_tz::America::New_York,
        };
        let resolved = resolve_deadline(&spec, utc(2026, 11, 20, 0, 0, 0));
        assert_eq!(resolved.expiry, at);
        assert_eq!(resolved.entered_date, None);
    }

    #[test]
    fn date_deadline_in_spring_forward_gap_resolves_to_first_instant_not_panic() {
        // Both zones spring-forward at local midnight: the following local
        // date's 00:00 never occurs. RFC-0002 BQ5/BE-08: resolve to the
        // earliest representable instant on or after (never panic).
        // America/Santiago: gap at midnight -> 2026-09-06; pre-gap offset
        // UTC-4, so the first representable instant is 2026-09-06T04:00Z
        // (01:00 local).
        let santiago = DeadlineSpec {
            kind: DeadlineKind::Date {
                date: NaiveDate::from_ymd_opt(2026, 9, 5).unwrap(),
            },
            zone: chrono_tz::America::Santiago,
        };
        let resolved = resolve_deadline(&santiago, utc(2026, 9, 1, 0, 0, 0));
        assert_eq!(resolved.expiry, utc(2026, 9, 6, 4, 0, 0));
        // Asia/Beirut: gap at midnight -> 2026-03-29; pre-gap offset EET
        // (+2), so the first representable instant is 2026-03-28T22:00Z
        // (01:00 EEST).
        let beirut = DeadlineSpec {
            kind: DeadlineKind::Date {
                date: NaiveDate::from_ymd_opt(2026, 3, 28).unwrap(),
            },
            zone: chrono_tz::Asia::Beirut,
        };
        let resolved = resolve_deadline(&beirut, utc(2026, 3, 1, 0, 0, 0));
        assert_eq!(resolved.expiry, utc(2026, 3, 28, 22, 0, 0));
    }

    #[test]
    fn relative_days_across_spring_forward_gap_resolves_not_panic() {
        // The target time-of-day lands inside the Santiago gap (00:30 on
        // 2026-09-06). Same rule: resolve to the instant the missing local
        // time would have occupied under the pre-transition offset.
        let spec = DeadlineSpec {
            kind: DeadlineKind::RelativeDays { days: 4 },
            zone: chrono_tz::America::Santiago,
        };
        // Local 2026-09-02 00:30 (UTC-4) = 2026-09-02T04:30:00Z.
        let resolved = resolve_deadline(&spec, utc(2026, 9, 2, 4, 30, 0));
        assert_eq!(resolved.expiry, utc(2026, 9, 6, 4, 30, 0));
    }

    #[test]
    fn rung_only_advances_upward() {
        assert_eq!(advance_rung(1, 3), 3);
        assert_eq!(advance_rung(3, 1), 3);
        assert_eq!(advance_rung(0, 1), 1);
        assert_eq!(advance_rung(2, 2), 2);
    }

    #[test]
    fn business_days_skip_weekends() {
        // 2026-11-21 is a Saturday; 2026-11-23 is a Monday.
        let sat = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();
        let mon = NaiveDate::from_ymd_opt(2026, 11, 23).unwrap();
        assert_eq!(business_days_between(sat, mon), 1);
        assert_eq!(add_business_days(sat, 1), mon);
        // Friday -> next Monday is one business day.
        let fri = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        assert_eq!(add_business_days(fri, 1), mon);
        assert_eq!(business_days_between(fri, mon), 1);
        // Two business days from Friday lands on Tuesday.
        let tue = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
        assert_eq!(add_business_days(fri, 2), tue);
        // A full calendar week is five business days.
        let next_fri = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        assert_eq!(add_business_days(fri, 5), next_fri);
        // Non-positive stays put.
        assert_eq!(add_business_days(fri, 0), fri);
    }
}
