mod support;

use serde_json::Value;
use support::{check_pg, dump, row, zen_value};

const TZ: &str = "America/New_York";

fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe { std::env::set_var("TZ", TZ) });
}

fn rows() -> Vec<Value> {
    vec![
        row(
            1,
            Some("2024-03-09T17:00:00Z"),
            Some("2024-03-09 12:00:00"),
            Some("2024-03-09"),
            Some("2024-03-09 12:00"),
            Some(1710003600000),
            Some(1),
        ),
        row(
            2,
            Some("2024-03-10T05:00:00Z"),
            Some("2024-03-10 00:00:00"),
            Some("2024-03-10"),
            Some("2024-03-10T00:00:00Z"),
            Some(1710046800000),
            Some(2),
        ),
        row(
            3,
            Some("2024-02-15T17:00:00Z"),
            Some("2024-02-15 12:00:00"),
            Some("2024-02-15"),
            Some("2024-02-15"),
            Some(1708016400000),
            Some(3),
        ),
        row(
            4,
            Some("2024-11-03T04:00:00Z"),
            Some("2024-11-03 00:00:00"),
            Some("2024-11-03"),
            Some("2024-11-03 03:30"),
            Some(1730606400000),
            Some(4),
        ),
        row(
            5,
            Some("2024-01-01T03:00:00Z"),
            Some("2023-12-31 22:00:00"),
            Some("2023-12-31"),
            Some("2024-01-01"),
            Some(1704078000000),
            Some(5),
        ),
        row(8, None, None, None, None, None, None),
    ]
}

fn dst_rows() -> Vec<Value> {
    vec![
        row(
            6,
            Some("2024-03-10T07:30:00Z"),
            Some("2024-03-10 02:30:00"),
            Some("2024-03-10"),
            Some("2024-03-10 02:30:00"),
            Some(1710055800000),
            Some(6),
        ),
        row(
            7,
            Some("2024-11-03T05:30:00Z"),
            Some("2024-11-03 01:30:00"),
            Some("2024-11-03"),
            Some("2024-11-03T01:30:00-04:00"),
            Some(1730611800000),
            Some(7),
        ),
        row(
            9,
            Some("2024-11-03T06:30:00Z"),
            Some("2024-11-03 01:30:00"),
            Some("2024-11-02"),
            Some("2024-11-03T01:30:00-05:00"),
            Some(1730615400000),
            Some(9),
        ),
    ]
}

const EXPRS: &[&str] = &[
    "d(ts).hour() == 12",
    "d(ts).day() == 31",
    "d(ts).year() == 2023",
    "d(ts).add(1, 'day').hour() == 12",
    "d(ts).add(1, 'day').hour() == 13",
    "d(loc).add(1, 'day').hour() == 12",
    "d(loc).add(1, 'day').hour() == 13",
    "d(day).add(1, 'day') == d('2024-03-10')",
    "d(day).add(1, 'day').hour() == 1",
    "d(ts).add(1, 'week').hour() == 12",
    "d(ts).add(24, 'hour').hour() == 13",
    "d(ts).add(1, 'month').hour() == 12",
    "d(ts).add(1, 'month').hour() == 13",
    "d(loc).add(1, 'month').hour() == 12",
    "d(day).add(1, 'month') == d('2024-03-15')",
    "d(ts).sub(1, 'day').hour() == 23",
    "d(ts).diff(d('2024-03-09'), 'day') == 1",
    "d(ts).diff(d('2024-03-09 12:00'), 'day') == 0",
    "d(ts).diff(d('2024-03-09 12:00'), 'hour') == 23",
    "d(day).diff(d('2024-03-09'), 'day') == 1",
    "d(loc).diff(d('2024-03-09 12:00'), 'day') == 0",
    "d(ts).startOf('day') == d('2024-03-10')",
    "d(ts).startOf('day').timestamp() == 1710046800000",
    "d(ts).startOf('month') == d('2024-03-01')",
    "d(ts).startOf('year') == d('2024-01-01')",
    "d(ts).startOf('week') == d('2024-03-04')",
    "d(ts).isSame(d('2024-03-10'), 'day')",
    "d(ts).isSame(d('2024-11-03'), 'day')",
    "d(ts).isBefore(d('2024-03-10'), 'day')",
    "d(ts).isAfter(d('2024-11-02 23:00'), 'hour')",
    "d(ts).isSame(d(day), 'day')",
    "d(ts).isSame(d(loc))",
    "d(loc) == d(ts)",
    "d(loc) < d(ts)",
    "d(loc) > d(ts)",
    "d(day) == d(ts)",
    "d(day) < d(ts)",
    "d(day) <= d(loc)",
    "d(day) < d(loc)",
    "d(loc) == d(txt)",
    "d(ts) == d(txt)",
    "d(ts) < d(txt)",
    "d(day) == d(txt)",
    "d(ts) == d(ms)",
    "d(loc) == d(ms)",
    "d(day).timestamp() == 1710046800000",
    "d(loc).timestamp() == 1710003600000",
    "d(loc).timestamp() == 1710055800000",
    "d(loc).timestamp() == 1730611800000",
    "d(loc).timestamp() == 1730615400000",
    "d(ts).timestamp() == 1710003600000",
    "d(loc) == d('2024-03-10 02:30')",
    "d(loc) == d('2024-11-03 01:30')",
    "d(loc) > d('2024-11-03 01:00')",
    "d(ts) == d('2024-11-03 01:30')",
    "d(ts) == d('2024-03-10 02:30')",
    "d(loc) == d('2024-03-10T07:30:00Z')",
    "d(day) == d('2024-03-10')",
    "d(day) == d('2024-03-10T05:00:00Z')",
    "d(day) > d('2024-03-09T05:00:00Z')",
    "d(day) >= d('2024-03-10T04:00:00Z')",
    "d(day) in [d('2024-03-10')..d('2024-11-03')]",
    "d(loc) in [d('2024-03-10')..d('2024-11-03')]",
    "d(loc) > d('2024-03-10T05:00:00Z')",
    "d(ts) > d('2024-03-10')",
    "d(ts) in [d('2024-03-10')..d('2024-11-03')]",
    "d(loc).hour() == 2",
    "d(loc).add(1, 'hour').hour() == 3",
    "d(loc).add(30, 'minute').hour() == 3",
    "d(loc).startOf('hour').hour() == 2",
    "d(ms).hour() == 12",
    "d(ms).day() == 31",
    "d(txt).hour() == 0",
    "d(txt).hour() == 1",
    "d(txt).hour() == 19",
    "d(txt).timestamp() == 1710055800000",
    "d(txt).timestamp() == 1730611800000",
    "d(ts).isToday()",
    "d(ts).weekday() == 7",
    "d(ts).week() == 1",
    "d(ts).dayOfYear() == 366",
    "d(ts).quarter() == 4",
];

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "iana-time-zone reads /etc/localtime on Linux, not TZ"
)]
fn zen_respects_tz() {
    init();
    let v = zen_value(&serde_json::json!({}), "d('2024-01-01').timestamp()");
    assert_eq!(v, "1704085200000", "ZEN did not pick up TZ={TZ}");
}

#[test]
fn dates_postgres_ny_session_utc() {
    init();
    let tz: chrono_tz::Tz = TZ.parse().unwrap();
    let rep = check_pg(
        &rows(),
        EXPRS,
        tz,
        "UTC",
        &format!("hunt_ny_u_{}", std::process::id()),
    );
    dump("pg_ny_session_utc", &rep);
    assert!(
        rep.failures.is_empty(),
        "{} failures:\n{}",
        rep.failures.len(),
        rep.failures.join("\n")
    );
}

#[test]
fn dates_postgres_ny_session_ny() {
    init();
    let tz: chrono_tz::Tz = TZ.parse().unwrap();
    let rep = check_pg(
        &rows(),
        EXPRS,
        tz,
        TZ,
        &format!("hunt_ny_n_{}", std::process::id()),
    );
    dump("pg_ny_session_ny", &rep);
    assert!(
        rep.failures.is_empty(),
        "{} failures:\n{}",
        rep.failures.len(),
        rep.failures.join("\n")
    );
}

#[test]
#[ignore = "known limit: wall-clock times inside a DST gap or overlap resolve differently in ZEN and in the warehouse"]
fn known_limits_dst_transitions() {
    init();
    let tz: chrono_tz::Tz = TZ.parse().unwrap();
    let rep = check_pg(
        &dst_rows(),
        EXPRS,
        tz,
        "UTC",
        &format!("hunt_ny_dst_{}", std::process::id()),
    );
    assert!(
        rep.failures.is_empty(),
        "{} failures:\n{}",
        rep.failures.len(),
        rep.failures.join("\n")
    );
}
