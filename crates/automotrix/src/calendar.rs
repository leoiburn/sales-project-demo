//! WHAT: appointment availability and booking, behind a provider trait.
//! WHY:  a dealership runs on a calendar. Today that calendar is these tables;
//!       later it may be Google Calendar. The trait keeps the booking flow from
//!       caring which.
//! HOW:  LocalCalendar computes free slots from business_hours minus
//!       availability_exceptions minus confirmed appointments, in the dealer's
//!       own timezone, then hands them to the customer as opaque slot ids.
//!
//!       The database, not this code, is the final judge of double-booking: the
//!       EXCLUDE constraint on appointments rejects any overlap on the same
//!       salesperson. free_slots is a courtesy filter so customers are offered
//!       times that are probably free; book() is where the truth is settled, and
//!       it reports the constraint's verdict rather than deciding for itself.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sqlx::postgres::types::PgRange;
use sqlx::PgPool;
use std::ops::Bound;
use uuid::Uuid;

use crate::db::DealerSettings;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Slot {
    /// Opaque handle the model hands back to book_appointment. Encoding the
    /// resource and start time means the model cannot invent an arbitrary time
    /// range - it can only pick one of the slots it was offered.
    pub id: String,
    pub resource_id: Uuid,
    pub resource_name: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Formatted in the dealer's timezone, which is the only form a customer
    /// should ever see.
    pub local: String,
}

impl Slot {
    fn encode(resource_id: Uuid, start: DateTime<Utc>) -> String {
        format!("{}.{}", resource_id.simple(), start.timestamp())
    }

    /// ponytail: unsigned, so a caller could craft an id for an arbitrary time.
    /// book() re-derives everything from the database and the EXCLUDE
    /// constraint still applies, so the worst case is a booking outside
    /// business hours by a caller who already has API access. Sign it (HMAC over
    /// resource+start) if the API ever faces the open internet.
    pub fn decode(id: &str) -> Result<(Uuid, DateTime<Utc>)> {
        let (res, ts) = id
            .split_once('.')
            .ok_or_else(|| anyhow!("malformed slot id"))?;
        let resource_id = Uuid::parse_str(res).map_err(|_| anyhow!("malformed slot id"))?;
        let secs: i64 = ts.parse().map_err(|_| anyhow!("malformed slot id"))?;
        let start = DateTime::from_timestamp(secs, 0).ok_or_else(|| anyhow!("bad slot time"))?;
        Ok((resource_id, start))
    }
}

#[derive(Debug, Clone)]
pub struct BookingRequest {
    pub dealer_id: Uuid,
    pub lead_id: Uuid,
    pub customer_id: Uuid,
    pub vehicle_id: Option<Uuid>,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Booking {
    pub appointment_id: Uuid,
    pub resource_id: Uuid,
    pub resource_name: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub local: String,
    pub kind: String,
}

/// The seam a Google Calendar backend would slot into.
pub trait CalendarProvider {
    fn free_slots(
        &self,
        settings: &DealerSettings,
        from: NaiveDate,
        to: NaiveDate,
        kind: &str,
        limit: usize,
    ) -> impl std::future::Future<Output = Result<Vec<Slot>>> + Send;

    fn book(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        settings: &DealerSettings,
        slot_id: &str,
        req: BookingRequest,
    ) -> impl std::future::Future<Output = Result<Booking>> + Send;

    fn cancel(&self, appointment_id: Uuid) -> impl std::future::Future<Output = Result<()>> + Send;
}

pub struct LocalCalendar {
    db: PgPool,
}

impl LocalCalendar {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct HoursRow {
    opens: NaiveTime,
    closes: NaiveTime,
}

/// Converts a wall-clock time in the dealer's timezone to a real instant.
///
/// This is where DST lives. On the spring-forward morning a local time may not
/// exist at all, and on the fall-back morning it may happen twice. Neither case
/// may be allowed to produce a silently wrong appointment: a skipped time is
/// dropped, and an ambiguous one takes the earlier of the two instants so the
/// slot is never offered twice.
fn to_instant(tz: Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earlier, _) => Some(earlier.with_timezone(&Utc)),
        chrono::LocalResult::None => None,
    }
}

fn format_local(tz: Tz, at: DateTime<Utc>) -> String {
    tz.from_utc_datetime(&at.naive_utc())
        .format("%A %B %-d, %-I:%M %p %Z")
        .to_string()
}

impl CalendarProvider for LocalCalendar {
    async fn free_slots(
        &self,
        settings: &DealerSettings,
        from: NaiveDate,
        to: NaiveDate,
        _kind: &str,
        limit: usize,
    ) -> Result<Vec<Slot>> {
        let tz = settings.tz();
        let step = Duration::minutes((settings.appointment_minutes + settings.buffer_minutes) as i64);
        let length = Duration::minutes(settings.appointment_minutes as i64);
        let now = Utc::now();

        let resources: Vec<(Uuid, String)> = sqlx::query_as(
            "select id, name from resources
             where dealer_id = $1 and kind = 'salesperson' and active order by name",
        )
        .bind(settings.dealer_id)
        .fetch_all(&self.db)
        .await?;
        if resources.is_empty() {
            return Ok(vec![]);
        }

        let mut out: Vec<Slot> = Vec::new();
        let mut date = from;
        while date <= to && out.len() < limit {
            // an exception for this date replaces the weekday's normal hours;
            // both times NULL means closed all day
            let exceptions: Vec<HoursRow> = sqlx::query_as(
                "select opens, closes from availability_exceptions
                 where dealer_id = $1 and date = $2 and opens is not null and closes is not null",
            )
            .bind(settings.dealer_id)
            .bind(date)
            .fetch_all(&self.db)
            .await?;

            let closed_all_day: Option<(bool,)> = sqlx::query_as(
                "select true from availability_exceptions
                 where dealer_id = $1 and date = $2 and opens is null and closes is null limit 1",
            )
            .bind(settings.dealer_id)
            .bind(date)
            .fetch_optional(&self.db)
            .await?;

            let windows: Vec<HoursRow> = if closed_all_day.is_some() {
                vec![]
            } else if !exceptions.is_empty() {
                exceptions
            } else {
                sqlx::query_as(
                    "select opens, closes from business_hours
                     where dealer_id = $1 and weekday = $2 order by opens",
                )
                .bind(settings.dealer_id)
                .bind(date.weekday().num_days_from_sunday() as i16)
                .fetch_all(&self.db)
                .await?
            };

            for w in windows {
                let Some(open_at) = to_instant(tz, date, w.opens) else {
                    continue;
                };
                let Some(close_at) = to_instant(tz, date, w.closes) else {
                    continue;
                };
                let mut cursor = open_at;
                while cursor + length <= close_at && out.len() < limit {
                    if cursor > now {
                        for (rid, rname) in &resources {
                            if out.len() >= limit {
                                break;
                            }
                            let taken: Option<(bool,)> = sqlx::query_as(
                                "select true from appointments
                                 where resource_id = $1 and status = 'confirmed'
                                   and slot && tstzrange($2, $3, '[)') limit 1",
                            )
                            .bind(rid)
                            .bind(cursor)
                            .bind(cursor + length)
                            .fetch_optional(&self.db)
                            .await?;
                            if taken.is_none() {
                                out.push(Slot {
                                    id: Slot::encode(*rid, cursor),
                                    resource_id: *rid,
                                    resource_name: rname.clone(),
                                    start: cursor,
                                    end: cursor + length,
                                    local: format_local(tz, cursor),
                                });
                                // one option per time, so three slots means
                                // three different times, not three salespeople
                                break;
                            }
                        }
                    }
                    cursor += step;
                }
            }
            date = date.succ_opt().ok_or_else(|| anyhow!("date overflow"))?;
        }
        Ok(out)
    }

    async fn book(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        settings: &DealerSettings,
        slot_id: &str,
        req: BookingRequest,
    ) -> Result<Booking> {
        let (resource_id, start) = Slot::decode(slot_id)?;
        let end = start + Duration::minutes(settings.appointment_minutes as i64);
        let tz = settings.tz();

        if start <= Utc::now() {
            return Err(anyhow!("that time has already passed"));
        }

        let resource: Option<(String,)> = sqlx::query_as(
            "select name from resources where id = $1 and dealer_id = $2 and active",
        )
        .bind(resource_id)
        .bind(req.dealer_id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some((resource_name,)) = resource else {
            return Err(anyhow!("that salesperson is no longer available"));
        };

        let appointment_id = crate::new_id();
        let range = PgRange {
            start: Bound::Included(start),
            end: Bound::Excluded(end),
        };

        // The EXCLUDE constraint decides. A unique-violation here means someone
        // else committed the same slot first, which is a normal outcome under
        // concurrency, not a bug - the caller offers fresh times.
        let res = sqlx::query(
            "insert into appointments (id, dealer_id, lead_id, customer_id, resource_id,
                 vehicle_id, kind, status, slot)
             values ($1, $2, $3, $4, $5, $6, $7, 'confirmed', $8)",
        )
        .bind(appointment_id)
        .bind(req.dealer_id)
        .bind(req.lead_id)
        .bind(req.customer_id)
        .bind(resource_id)
        .bind(req.vehicle_id)
        .bind(&req.kind)
        .bind(range)
        .execute(&mut **tx)
        .await;

        if let Err(e) = res {
            let msg = e.to_string();
            if msg.contains("appointments_no_overlap") {
                return Err(anyhow!("SLOT_TAKEN"));
            }
            return Err(e.into());
        }

        Ok(Booking {
            appointment_id,
            resource_id,
            resource_name,
            start,
            end,
            local: format_local(tz, start),
            kind: req.kind,
        })
    }

    async fn cancel(&self, appointment_id: Uuid) -> Result<()> {
        sqlx::query("update appointments set status = 'cancelled' where id = $1")
            .bind(appointment_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }
}

/// Not implemented. See docs/google_calendar_notes.md for the design.
#[derive(Debug, Clone, Default)]
pub struct GoogleCalendarProvider {
    pub calendar_id: String,
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
}

impl CalendarProvider for GoogleCalendarProvider {
    async fn free_slots(
        &self,
        _settings: &DealerSettings,
        _from: NaiveDate,
        _to: NaiveDate,
        _kind: &str,
        _limit: usize,
    ) -> Result<Vec<Slot>> {
        Err(anyhow!("GoogleCalendarProvider is a stub - see docs/google_calendar_notes.md"))
    }

    async fn book(
        &self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _settings: &DealerSettings,
        _slot_id: &str,
        _req: BookingRequest,
    ) -> Result<Booking> {
        Err(anyhow!("GoogleCalendarProvider is a stub - see docs/google_calendar_notes.md"))
    }

    async fn cancel(&self, _appointment_id: Uuid) -> Result<()> {
        Err(anyhow!("GoogleCalendarProvider is a stub - see docs/google_calendar_notes.md"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dst_fall_back_keeps_the_right_offsets() {
        // US DST ends 2026-11-01. A 10:00 local appointment is -05:00 the day
        // before and -06:00 the day after; if these drifted, every appointment
        // around the change would be an hour wrong.
        let tz: Tz = "America/Chicago".parse().unwrap();
        let ten = NaiveTime::from_hms_opt(10, 0, 0).unwrap();

        let before = to_instant(tz, NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(), ten).unwrap();
        let after = to_instant(tz, NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(), ten).unwrap();

        assert_eq!(before.to_rfc3339(), "2026-10-31T15:00:00+00:00");
        assert_eq!(after.to_rfc3339(), "2026-11-02T16:00:00+00:00");
    }

    #[test]
    fn ambiguous_local_time_resolves_to_one_instant() {
        // 01:30 happens twice on 2026-11-01 in Chicago. Taking the earlier one
        // means the slot is offered once, not twice.
        let tz: Tz = "America/Chicago".parse().unwrap();
        let t = to_instant(
            tz,
            NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            NaiveTime::from_hms_opt(1, 30, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(t.to_rfc3339(), "2026-11-01T06:30:00+00:00");
    }

    #[test]
    fn skipped_local_time_is_dropped() {
        // 02:30 does not exist on 2026-03-08 (spring forward)
        let tz: Tz = "America/Chicago".parse().unwrap();
        assert!(to_instant(
            tz,
            NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(),
            NaiveTime::from_hms_opt(2, 30, 0).unwrap()
        )
        .is_none());
    }

    #[test]
    fn slot_ids_round_trip() {
        let id = Uuid::now_v7();
        let at = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let (r, s) = Slot::decode(&Slot::encode(id, at)).unwrap();
        assert_eq!(r, id);
        assert_eq!(s, at);
        assert!(Slot::decode("garbage").is_err());
    }
}
