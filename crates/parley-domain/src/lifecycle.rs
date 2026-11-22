//! Contact thread lifecycle and civil deadline resolution (RFC-0002 BE-08).
//!
//! This module is pure decision policy. It owns the thread transition table,
//! the opportunity consequence rules and the exclusive civil expiry resolver.
//! Deadlines are resolved through the owner's IANA timezone so an inclusive
//! date means "through that local date": the expiry is the first instant of
//! the *following* local date, never `now + 24h`. A civil transition is
//! resolved by the timezone library, and an explicit UTC timestamp is used
//! verbatim as the exclusive expiry.

use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, TimeZone, Timelike, Utc};
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
            // resolves to the earliest instant.
            let next_local_midnight = date
                .succ_opt()
                .expect("the day after any civil date exists")
                .and_hms_opt(0, 0, 0)
                .expect("midnight");
            let expiry = spec
                .zone
                .from_local_datetime(&next_local_midnight)
                .earliest()
                .or_else(|| spec.zone.from_local_datetime(&next_local_midnight).latest())
                .expect("a local midnight always maps to an instant")
                .with_timezone(&Utc);
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
            let expiry = spec
                .zone
                .from_local_datetime(&target_local)
                .earliest()
                .or_else(|| spec.zone.from_local_datetime(&target_local).latest())
                .expect("local time maps to an instant")
                .with_timezone(&Utc);
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
    fn rung_only_advances_upward() {
        assert_eq!(advance_rung(1, 3), 3);
        assert_eq!(advance_rung(3, 1), 3);
        assert_eq!(advance_rung(0, 1), 1);
        assert_eq!(advance_rung(2, 2), 2);
    }
}
