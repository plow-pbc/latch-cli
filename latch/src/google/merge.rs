//! Project each command's published JSON result, without guessing from text.
use serde_json::{Value, json};

/// One account's result for a fan-out read, as a list of items, each tagged
/// with `account`. When the agent projected the fields itself (`projected`),
/// gog already returned just those, so they pass through. Otherwise each
/// command's list is taken from the field gog publishes it in.
pub fn items(command: &str, result: Value, projected: bool, account: &str) -> Vec<Value> {
    let mut values = if projected {
        result.as_array().cloned().unwrap_or_else(|| vec![result])
    } else if command == "calendar.freebusy" {
        result["calendars"]
            .as_object()
            .map(|calendars| {
                calendars
                    .iter()
                    .map(|(id, availability)| {
                        json!({
                            "calendar": id,
                            "busy": availability["busy"],
                            "errors": availability["errors"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        let field = match command {
            "gmail.search" => "threads",
            "calendar.events" => "events",
            "calendar.calendars" => "calendars",
            "calendar.conflicts" => "conflicts",
            _ => unreachable!("only curated reads fan out"),
        };
        result[field].as_array().cloned().unwrap_or_default()
    };
    for value in &mut values {
        if let Some(object) = value.as_object_mut() {
            object.insert("account".into(), json!(account));
        }
    }
    values
}

/// How much of a merged calendar event list is kept, in serialized
/// characters: an agent's tool output is cut off near 50,000, and this
/// leaves room for the rest of the answer.
pub const CALENDAR_BUDGET: usize = 40_000;

/// Merged calendar events (sorted soonest first), each cut down to what
/// scheduling needs, in the owner's time zone that gog already wrote into
/// `startDayOfWeek`, `startLocal` and `endLocal`. Past `budget` the rest are
/// dropped, and the second value says how many and `after` which start: the
/// earliest among them, a local time or the bare date of an all-day event.
pub fn compact_events(items: &[Value], budget: usize) -> (Vec<Value>, Option<Value>) {
    let mut kept = Vec::new();
    let mut used = 0;
    for item in items {
        let event = compact_event(item);
        // Counted as JavaScript counts string length, so the budget cuts
        // where plow-gog's does.
        used += event.to_string().encode_utf16().count() + 1;
        if used > budget {
            let omitted = &items[kept.len()..];
            let earliest = omitted
                .iter()
                .filter(|item| item["startLocal"].is_string())
                .reduce(|a, b| if starts_before(b, a) { b } else { a });
            let truncated = json!({
                "omitted": omitted.len(),
                "after": earliest.map(|item| item["startLocal"].clone()),
            });
            return (kept, Some(truncated));
        }
        kept.push(event);
    }
    (kept, None)
}

/// One event as `{summary, startDayOfWeek, startLocal, endLocal, allDay?,
/// attendees?, transparency?, declined?, id, account}`.
fn compact_event(item: &Value) -> Value {
    let mut event = json!({
        "summary": item["summary"],
        "startDayOfWeek": item["startDayOfWeek"],
        "startLocal": item["startLocal"],
        "endLocal": item["endLocal"],
    });
    if all_day(item) {
        event["allDay"] = json!(true);
    }
    let attendees = item["attendees"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    // Everyone else invited, by email: not the owner, not rooms, not those
    // who declined.
    let others: Vec<_> = attendees
        .iter()
        .filter(|a| a["self"] != true && a["resource"] != true && a["responseStatus"] != "declined")
        .map(|a| a["email"].clone())
        .collect();
    if !others.is_empty() {
        event["attendees"] = json!(others);
    }
    // The two ways an event on the calendar leaves the owner free.
    if item["transparency"] == "transparent" {
        event["transparency"] = json!("transparent");
    }
    if attendees
        .iter()
        .any(|a| a["self"] == true && a["responseStatus"] == "declined")
    {
        event["declined"] = json!(true);
    }
    event["id"] = item["id"].clone();
    event["account"] = item["account"].clone();
    event
}

fn all_day(item: &Value) -> bool {
    item["start"]["date"].is_string()
}

/// Whether `a` starts before `b`, both with a `startLocal`: by local day,
/// an all-day event first (it covers its day from midnight), then by instant.
fn starts_before(a: &Value, b: &Value) -> bool {
    let day = |item: &Value| {
        let local = item["startLocal"].as_str().unwrap_or("");
        local.get(..10).unwrap_or(local).to_owned()
    };
    let (day_a, day_b) = (day(a), day(b));
    if day_a != day_b {
        return day_a < day_b;
    }
    if all_day(a) != all_day(b) {
        return all_day(a);
    }
    let instant = |item: &Value| {
        item["start"]["dateTime"]
            .as_str()
            .and_then(|time| time.parse::<jiff::Timestamp>().ok())
    };
    match (instant(a), instant(b)) {
        (Some(a), Some(b)) => a < b,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Orders merged items by time: mail newest first, calendar entries earliest
/// first. Times are compared as instants, so different time zones sort
/// correctly; an all-day date counts as midnight UTC.
pub fn sort(command: &str, items: &mut [Value]) {
    let key = |value: &Value| {
        let time = if command == "gmail.search" {
            value["internalDateIso"].as_str()
        } else {
            value["startLocal"]
                .as_str()
                .or_else(|| value["start"].as_str())
                .or_else(|| value["start"]["dateTime"].as_str())
                .or_else(|| value["start"]["date"].as_str())
        };
        time.and_then(|time| {
            if time.len() == 10 {
                format!("{time}T00:00:00Z").parse::<jiff::Timestamp>().ok()
            } else {
                time.parse::<jiff::Timestamp>().ok()
            }
        })
    };
    items.sort_by_key(key);
    if command == "gmail.search" {
        items.reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_calendar_projection_survives_fanout() {
        let values = items(
            "calendar.events",
            json!([{"id":"event","description":"requested"}]),
            true,
            "owner@example.com",
        );
        assert_eq!(
            values,
            vec![json!({"id":"event","description":"requested","account":"owner@example.com"})]
        );
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;
    #[test]
    fn mail_order_uses_instants_instead_of_the_display_minute() {
        let mut values = vec![
            json!({
                "id": "old",
                "date": "2026-10-14 22:53",
                "internalDateIso": "2026-10-14T22:53:32-07:00",
            }),
            json!({
                "id": "new",
                "date": "2026-10-14 22:53",
                "internalDateIso": "2026-10-14T22:53:56-07:00",
            }),
        ];
        sort("gmail.search", &mut values);
        assert_eq!(values[0]["id"], "new");
    }
    #[test]
    fn projected_calendar_times_sort_instantly_across_offsets() {
        let mut values = vec![
            json!({"id":"later","startLocal":"2042-11-17T17:00:00Z"}),
            json!({"id":"earlier","startLocal":"2042-11-17T08:00:00-08:00"}),
        ];
        sort("calendar.events", &mut values);
        assert_eq!(values[0]["id"], "earlier");
    }
}

/// plow-gog's own cases for its compact calendar shape
/// (`compactCalendarEvents` in the old Latch app's `plowGog.test.ts`).
#[cfg(test)]
mod compact_tests {
    use super::*;

    const DAYS: [&str; 5] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"];
    const ACCOUNTS: [&str; 3] = ["a@example.com", "b@example.com", "c@example.com"];

    /// A raw event as gog prints it under `--timezone`: Google's own fields,
    /// padded as real ones are to 2-5 KB, plus gog's local fields.
    fn raw_event(i: usize) -> Value {
        let local = format!("2026-09-{}T{:02}:00:00-07:00", 14 + i % 5, 8 + i % 9);
        json!({
            "kind": "calendar#event",
            "id": format!("evt-{i}"),
            "status": "confirmed",
            "htmlLink": format!("https://www.google.com/calendar/event?eid={}{i}", "x".repeat(60)),
            "summary": format!("Meeting {i}"),
            "description": "Agenda. ".repeat(80),
            "start": {"dateTime": local, "timeZone": "America/Sao_Paulo"},
            "end": {"dateTime": local, "timeZone": "America/Sao_Paulo"},
            "attendees": (0..8).map(|n| json!({"email": format!("person{n}@example.com"), "responseStatus": "accepted"})).collect::<Vec<_>>(),
            "conferenceData": {"entryPoints": [{"uri": format!("https://meet.google.com/{}", "abc-".repeat(20))}]},
            "startDayOfWeek": DAYS[i % 5],
            "startLocal": local,
            "endDayOfWeek": DAYS[i % 5],
            "endLocal": local,
            "timezone": "America/Los_Angeles",
            "account": ACCOUNTS[i % 3],
        })
    }

    #[test]
    fn a_busy_three_account_week_fits_and_keeps_each_weekday() {
        let items: Vec<_> = (0..63).map(raw_event).collect();
        assert!(json!(items).to_string().len() > 100_000);
        let (events, truncated) = compact_events(&items, CALENDAR_BUDGET);
        assert!(truncated.is_none());
        assert_eq!(events.len(), 63);
        assert!(json!({"items": events, "accounts": []}).to_string().len() < 50_000);
        for (i, event) in events.iter().enumerate() {
            assert_eq!(event["startDayOfWeek"], DAYS[i % 5]);
            assert_eq!(event["account"], ACCOUNTS[i % 3]);
        }
        assert_eq!(
            events[0],
            json!({
                "summary": "Meeting 0",
                "startDayOfWeek": "Monday",
                "startLocal": "2026-09-14T08:00:00-07:00",
                "endLocal": "2026-09-14T08:00:00-07:00",
                "attendees": (0..8).map(|n| format!("person{n}@example.com")).collect::<Vec<_>>(),
                "id": "evt-0",
                "account": "a@example.com",
            })
        );
    }

    #[test]
    fn all_day_events_others_invited_and_the_ways_an_event_leaves_the_owner_free() {
        let (events, _) = compact_events(
            &[json!({
                "summary": "holiday",
                "start": {"date": "2026-09-16"},
                "end": {"date": "2026-09-17"},
                "startDayOfWeek": "Wednesday",
                "startLocal": "2026-09-16",
                "endLocal": "2026-09-17",
                "transparency": "transparent",
                "attendees": [
                    {"email": "me@example.com", "self": true, "responseStatus": "declined"},
                    {"email": "room@resource.calendar.google.com", "resource": true, "responseStatus": "accepted"},
                    {"email": "gone@example.com", "responseStatus": "declined"},
                    {"email": "dana@example.com", "responseStatus": "needsAction"},
                ],
                "account": "a",
            })],
            CALENDAR_BUDGET,
        );
        assert_eq!(
            events[0],
            json!({
                "summary": "holiday",
                "startDayOfWeek": "Wednesday",
                "startLocal": "2026-09-16",
                "endLocal": "2026-09-17",
                "allDay": true,
                "attendees": ["dana@example.com"],
                "transparency": "transparent",
                "declined": true,
                "id": null,
                "account": "a",
            })
        );
    }

    fn timed(local: &str) -> Value {
        json!({"summary": "x".repeat(200), "start": {"dateTime": local}, "startLocal": local, "account": "a"})
    }

    #[test]
    fn past_the_budget_events_are_cut_whole_saying_how_many_and_from_when() {
        let items: Vec<_> = (0..10)
            .map(|i| timed(&format!("2026-09-16T{:02}:00:00-07:00", 8 + i)))
            .collect();
        let (kept, truncated) = compact_events(&items, 1_000);
        assert!(!kept.is_empty() && kept.len() < 10);
        assert!(json!(kept).to_string().len() <= 1_000);
        assert_eq!(
            truncated,
            Some(json!({
                "omitted": 10 - kept.len(),
                "after": format!("2026-09-16T{:02}:00:00-07:00", 8 + kept.len()),
            }))
        );
    }

    #[test]
    fn the_cut_reaches_back_to_an_all_day_event_sorted_after_a_timed_one() {
        // Sorted as UTC midnight, an all-day 2026-09-16 in Auckland falls at
        // noon, after a 09:00 meeting that day, but it covers the morning too.
        let items = [
            timed("2026-09-16T08:00:00+12:00"),
            timed("2026-09-16T09:00:00+12:00"),
            json!({"summary": "offsite", "start": {"date": "2026-09-16"}, "startLocal": "2026-09-16", "account": "b"}),
            timed("2026-09-16T13:00:00+12:00"),
        ];
        let (kept, truncated) = compact_events(&items, 700);
        let starts: Vec<_> = kept.iter().map(|event| &event["startLocal"]).collect();
        assert_eq!(
            starts,
            ["2026-09-16T08:00:00+12:00", "2026-09-16T09:00:00+12:00"]
        );
        assert_eq!(
            truncated,
            Some(json!({"omitted": 2, "after": "2026-09-16"}))
        );
    }

    #[test]
    fn merged_calendar_items_sort_soonest_first_timed_or_all_day() {
        let mut items = vec![
            json!({"summary": "late", "start": {"dateTime": "2026-08-28T15:00:00-07:00"}}),
            json!({"summary": "early", "start": {"dateTime": "2026-08-28T10:00:00-07:00"}}),
            json!({"summary": "allday", "start": {"date": "2026-08-27"}}),
        ];
        sort("calendar.events", &mut items);
        let order: Vec<_> = items.iter().map(|item| &item["summary"]).collect();
        assert_eq!(order, ["allday", "early", "late"]);
    }
}
