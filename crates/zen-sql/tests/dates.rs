mod support;

use serde_json::Value;
use support::{check_duck, check_pg, dump, row};

fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe { std::env::set_var("TZ", "UTC") });
}

fn rows() -> Vec<Value> {
    vec![
        row(
            1,
            Some("2024-01-31T12:00:00Z"),
            Some("2024-01-31 12:00:00"),
            Some("2024-01-31"),
            Some("2024-01-31"),
            Some(1706702400000),
            Some(1),
        ),
        row(
            2,
            Some("2024-02-29T23:59:59.999Z"),
            Some("2024-02-29 23:59:59.999"),
            Some("2024-02-29"),
            Some("2024-02-29T23:59:59.999Z"),
            Some(1709251199999),
            Some(2),
        ),
        row(
            3,
            Some("2020-12-31T00:00:00Z"),
            Some("2020-12-31 00:00:00"),
            Some("2020-12-31"),
            Some("2020-12-31 00:00"),
            Some(1609372800000),
            Some(3),
        ),
        row(
            4,
            Some("2021-01-03T10:00:00Z"),
            Some("2021-01-03 10:00:00"),
            Some("2021-01-03"),
            Some("2021-01-03T10:00:00"),
            Some(1609668000000),
            Some(4),
        ),
        row(
            5,
            Some("1969-12-31T23:59:59.500Z"),
            Some("1969-12-31 23:59:59.500"),
            Some("1969-12-31"),
            Some("1969-12-31T23:59:59.5Z"),
            Some(-500),
            Some(5),
        ),
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
        row(8, None, None, None, None, None, None),
        row(
            9,
            Some("2023-12-31T23:59:59Z"),
            Some("2023-12-31 23:59:59"),
            Some("2023-12-31"),
            Some("garbage"),
            Some(1704067199000),
            Some(9),
        ),
        row(
            10,
            Some("2024-12-30T00:00:00Z"),
            Some("2024-12-30 00:00:00"),
            Some("2024-12-30"),
            Some("2024-02-30"),
            Some(1735516800000),
            Some(10),
        ),
        row(
            11,
            Some("2024-01-15T04:30:00Z"),
            Some("2024-01-15 10:00:00"),
            Some("2024-01-15"),
            Some("2024-01-15T10:00:00+05:30"),
            Some(1705293000000),
            Some(11),
        ),
        row(
            12,
            Some("1900-01-01T00:00:00Z"),
            Some("1900-01-01 00:00:00"),
            Some("1900-01-01"),
            Some(""),
            Some(-2208988800000),
            Some(12),
        ),
        row(
            13,
            Some("2024-02-29T00:00:00Z"),
            Some("2024-02-29 00:00:00"),
            Some("2024-02-29"),
            Some("2024-02-29"),
            Some(1709164800000),
            Some(13),
        ),
        row(
            14,
            Some("2024-03-31T10:00:00Z"),
            Some("2024-03-31 10:00:00"),
            Some("2024-03-31"),
            Some("2024-03-31T10:00:00Z"),
            Some(-1),
            Some(14),
        ),
    ]
}

const EXPRS: &[&str] = &[
    "d(ts).add(1, 'year') > d('2025-01-01')",
    "d(ts).add(1, 'year').isValid()",
    "d(day).add(1, 'year') == d('2025-02-28')",
    "d(loc).add(1, 'year').month() == 2",
    "d(ts).sub(1, 'year').day() == 28",
    "d(ts).add(1, 'month').day() == 29",
    "d(ts).add(1, 'month').month() == 2",
    "d(day).add(1, 'month') == d('2024-02-29')",
    "d(ts).sub(1, 'month').day() == 29",
    "d(ts).add(1, 'quarter').day() == 30",
    "d(ts).add('1y').year() == 2025",
    "d(ts).add('-1d').day() == 30",
    "d(ts).add(-1, 'day').day() == 30",
    "d(ts).week() == 53",
    "d(day).week() == 53",
    "d(loc).week() == 53",
    "d(ts).week() == 1",
    "d(ts).weekday() == 7",
    "d(ts).dayOfYear() == 366",
    "d(ts).dayOfYear() == 60",
    "d(ts).quarter() == 1",
    "d(ts).second() == 59",
    "d(loc).second() == 59",
    "d(ts).timestamp() == -500",
    "d(ts).timestamp() < 0",
    "d(loc).timestamp() == -500",
    "d(ts).timestamp() == 1709251199999",
    "d(day).timestamp() < 0",
    "d(ms).timestamp() == -500",
    "d(ms) < d('1970-01-01')",
    "d(ms).year() == 1969",
    "d(ms).second() == 59",
    "d(ms) == d(ts)",
    "d(txt).isValid()",
    "d(txt) > d('2000-01-01')",
    "d(txt) < d('2030-01-01')",
    "d(txt) == d(ts)",
    "d(txt).hour() == 4",
    "d(txt).year() == 2024",
    "d(txt) == null",
    "d(ts).startOf('week') == d('2020-12-28')",
    "d(ts).startOf('quarter') == d('2024-01-01')",
    "d(ts).startOf('day') == d('1969-12-31')",
    "d(ts).startOf('hour').timestamp() == -3600000",
    "d(ts).startOf('second').timestamp() == -1000",
    "d(ts).startOf('minute') == d('1969-12-31 23:59')",
    "d(ts).isSame(d('2024-02-29'), 'day')",
    "d(ts).isSame('2024-02', 'month')",
    "d(ts).isBefore(d('2024-02-29'), 'day')",
    "d(ts).isAfter(d('2024-02-29'), 'day')",
    "d(ts).isSameOrBefore(d('2024-02-29'), 'day')",
    "d(ts).isSameOrAfter(d('2024-02-29'), 'month')",
    "d(ts).isSame(d(day), 'day')",
    "d(ts).isSame(d(loc))",
    "d(ts).isBefore(d(txt))",
    "d(ts).isAfter(d(txt))",
    "d(ts).isSame(d(txt))",
    "d(ts).isBefore(d(txt), 'day')",
    "d(ts).isSameOrBefore(d(txt))",
    "d(ts).diff(d('2024-01-01'), 'day') == 30",
    "d(ts).diff(d('1970-01-01'), 'second') == 0",
    "d(ts).diff(d('1970-01-01'), 'second') == -1",
    "d(ts).diff(d('1970-01-01')) == -500",
    "d(ts).diff(d(day), 'hour') == 12",
    "d(ts).diff(d(txt), 'day') == 0",
    "d(ts).diff(d(txt)) == 0",
    "d(day) == d(ts)",
    "d(day) < d(ts)",
    "d(day) <= d(loc)",
    "d(loc) == d(ts)",
    "d(loc) < d(ts)",
    "d(ts) > d(txt)",
    "d(ts) < d(txt)",
    "d(ts) <= d(txt)",
    "d(ts) >= d(txt)",
    "d(txt) < d(ts)",
    "d(ts) < '2024-01-01'",
    "d(ts) <= '2024-01-01'",
    "d(ts) > '2024-01-01'",
    "d(day) < '2024-02-29 12:00'",
    "d(day) <= '2024-02-29 12:00'",
    "d(day) > '2024-02-29 12:00'",
    "d(day) >= '2024-02-29 12:00'",
    "d(day) == '2024-02-29 12:00'",
    "d(day) != '2024-02-29 12:00'",
    "d(day) in [d('2024-02-29 12:00')..d('2024-12-31')]",
    "d(day) in (d('2024-02-28 12:00')..d('2024-03-01 12:00'))",
    "d(day) in ['2024-02-29', '2024-03-10']",
    "d(day) in [d('2024-02-29 12:00'), d('2024-03-10')]",
    "d(ts) in [d('2024-02-29T23:59:59.999Z'), '2020-12-31']",
    "d(ts) not in [d('2024-02-29T23:59:59.999Z'), '2020-12-31']",
    "d(ts) in ['2020-12-31', null]",
    "d(ts) == d('2024-02-29T23:59:59.9999Z')",
    "d(ts) == d('2024-02-29T23:59:59.999Z')",
    "d(loc) == d('2024-02-29 23:59:59.999')",
    "d(ts) > d('2024-02-29T23:59:59.9989Z')",
    "d(ts) < d('1970-01-01')",
    "d(ts) < d(-1)",
    "d(ts) == d(-500)",
    "d(ts) <= d(-500)",
    "d(ts) > d(1709251199998.5)",
    "d(ts) in [d('1969-12-31T23:59:59Z')..d('1970-01-01')]",
    "d(ts) < d('2024-01-01') or d(ts) > d('2024-12-01')",
    "d(ts) < d('2024-01-01') and d(ts) > d('2000-01-01')",
    "d(ts).isValid() and d(ts) > d('2024-01-01')",
    "not d(ts).isValid()",
    "d(ts) != null",
    "d(ts) == null",
    "ts == null",
    "ts != null and d(ts).year() > 2023",
    "d(ts) == d(ts)",
    "d(ts).add(1, 'day') > d(ts)",
    "d(ts).add(1, 'second') > '2024-03-01'",
    "d(ts).add(1, 'week').week() == 1",
    "d(ts).add(1, 'week') == d('2024-01-06')",
    "d(loc).add(1, 'hour').hour() == 0",
    "d(day).add(1, 'day').weekday() == 1",
    "d(day).add(36, 'hour').hour() == 12",
    "d(day).startOf('month') == d('2024-02-01')",
    "d(day).startOf('year').timestamp() == 1704067200000",
    "d(ts).isToday()",
    "d(day).isToday()",
    "d(loc).isYesterday()",
    "d(ts).hour() == 23 and d(ts).minute() == 59",
    "d(ts).timestamp() % 1000 == 999",
    "d(ts).timestamp() / 1000 > 1709251199",
    "date(ts) > date('2024-01-01')",
    "d(ts) > d('2024-01-01', 'Europe/Paris')",
    "d(ts) > d('2024-02-30')",
    "d(ts) > '2024-02-30'",
    "d(ts).isAfter('garbage')",
    "d(day).hour() == 0",
    "d(day).minute() == 0",
    "d(day).second() == 0",
    "d(day).startOf('day').timestamp() == 1709164800000",
    "d(ts) in [null]",
    "d(ts).isSame(null)",
    "d(ts) < d('2024-02-29T23:59:59.9995Z')",
    "d(loc) < d('2024-02-29 23:59:59.9995')",
];

#[test]
fn dates_duckdb_utc() {
    init();
    let rep = check_duck(&rows(), EXPRS, chrono_tz::Tz::UTC);
    dump("duck_utc", &rep);
    assert!(
        rep.failures.is_empty(),
        "{} failures:\n{}",
        rep.failures.len(),
        rep.failures.join("\n")
    );
}

#[test]
fn dates_postgres_utc() {
    init();
    let rep = check_pg(
        &rows(),
        EXPRS,
        chrono_tz::Tz::UTC,
        "UTC",
        &format!("hunt_utc_{}", std::process::id()),
    );
    dump("pg_utc", &rep);
    assert!(
        rep.failures.is_empty(),
        "{} failures:\n{}",
        rep.failures.len(),
        rep.failures.join("\n")
    );
}

mod pushdown_numbers {
    use std::sync::Arc;

    use arrow::array::{ArrayRef, Int32Array, Int64Array, UInt32Array};
    use arrow::datatypes::{DataType, Field, Schema as ASchema};
    use arrow::record_batch::RecordBatch;
    use serde_json::json;
    use zen_sql::{Dialect, Options, Schema, Source};

    fn batch() -> RecordBatch {
        let schema = ASchema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("q", DataType::Int32, true),
            Field::new("big", DataType::Int64, true),
            Field::new("u", DataType::UInt32, true),
        ]);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(vec![1, 2, 3])),
            Arc::new(Int32Array::from(vec![Some(1), Some(7), None])),
            Arc::new(Int64Array::from(vec![
                Some(9007199254740993),
                Some(5),
                None,
            ])),
            Arc::new(UInt32Array::from(vec![Some(0), Some(4), None])),
        ];
        RecordBatch::try_new(Arc::new(schema), cols).unwrap()
    }

    fn rows() -> Vec<serde_json::Value> {
        vec![
            json!({"id": 1, "q": 1, "big": 9007199254740993i64, "u": 0}),
            json!({"id": 2, "q": 7, "big": 5, "u": 4}),
            json!({"id": 3, "q": null, "big": null, "u": null}),
        ]
    }

    fn duck() -> duckdb::Connection {
        let c = duckdb::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE t (id BIGINT, q INTEGER, big BIGINT, u UINTEGER);
             INSERT INTO t VALUES (1, 1, 9007199254740993, 0), (2, 7, 5, 4), (3, NULL, NULL, NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn pushdown_numbers() {
        super::init();
        let b = batch();
        let schema = Schema::single(Source::from_arrow("row", &b.schema()));
        let conn = duck();
        let mut failures = Vec::new();
        for expr in [
            "q < 5000000000",
            "q > -5000000000",
            "q in [1..5000000000]",
            "u > -1",
            "u >= -1 and u < 10",
            "q > 0.99999999999999999",
            "q >= 1.00000000000000001",
            "q < 7.000000000000000001",
            "big > 9007199254740992.5",
            "big >= 9007199254740992.9",
            "big < 9007199254740993.4",
            "big in [9007199254740992.5..9007199254740994]",
        ] {
            let c = zen_sql::filter(expr, &schema, &Options::new(Dialect::DuckDb)).unwrap();
            let zen = crate::support::zen_ids(&rows(), expr);
            let sql = format!("SELECT id FROM t WHERE {} ORDER BY id", c.sql);
            let duck: Vec<i64> = conn
                .prepare(&sql)
                .unwrap()
                .query_map([], |r| r.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let preds: Vec<_> = c.pushdown.iter().map(|p| p.predicate.clone()).collect();
            let kept = crate::support::apply(&b, &preds).unwrap();
            let kept: Vec<i64> = kept
                .column_by_name("id")
                .unwrap()
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .iter()
                .map(|v| v.unwrap())
                .collect();
            let line = format!(
                "{expr}: zen {zen:?} duck {duck:?} pushdown {kept:?}\n   sql {}\n   pd {preds:?}",
                c.sql
            );
            eprintln!("{line}");
            if zen != duck
                || !crate::support::subset(&duck, &kept)
                || !crate::support::subset(&zen, &kept)
            {
                failures.push(line);
            }
        }
        assert!(
            failures.is_empty(),
            "{} failures:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

fn parse_rows() -> Vec<Value> {
    let texts = [
        "epoch",
        "infinity",
        "now",
        "today",
        "2024-03-05 10:00:00+05",
        "2024-03-05T10:00:00+05",
        "2024-03-05 10:00:00 +05:00",
        "March 5, 2024",
        "05/03/2024",
        "2024-03-05T10:00:00.123456789Z",
        "2024-03-05T24:00:00",
        "2024-03-05T10:00",
        "-infinity",
        "2024-03-05 10:00:00.5",
        "10000-01-01",
        "0001-01-01",
        "2024-03-05T10:00:00+0530",
    ];
    let mut out: Vec<Value> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| row(i as i64 + 1, None, None, None, Some(t), None, None))
        .collect();
    let n = out.len() as i64;
    for (k, ms) in [
        1i64 << 62,
        100000000000000000,
        -100000000000000000,
        8210266876799999,
    ]
    .iter()
    .enumerate()
    {
        out.push(row(
            n + 1 + k as i64,
            None,
            None,
            None,
            None,
            Some(*ms),
            None,
        ));
    }
    out
}

#[test]
fn parse_duckdb() {
    init();
    let rep = check_duck(
        &parse_rows(),
        &[
            "d(txt).isValid()",
            "d(txt).year() == 2024",
            "d(txt) > d('2000-01-01')",
            "d(ms).isValid()",
            "d(ms).year() > 2024",
        ],
        chrono_tz::Tz::UTC,
    );
    dump("parse_duck", &rep);
    assert!(
        rep.failures.is_empty(),
        "{} failures:\n{}",
        rep.failures.len(),
        rep.failures.join("\n")
    );
}

#[test]
fn parse_postgres() {
    init();
    let rep = check_pg(
        &parse_rows(),
        &[
            "d(txt).isValid()",
            "d(txt).year() == 2024",
            "d(txt) > d('2000-01-01')",
            "d(ms).isValid()",
            "d(ms).year() > 2024",
        ],
        chrono_tz::Tz::UTC,
        "UTC",
        &format!("hunt_parse_{}", std::process::id()),
    );
    dump("parse_pg", &rep);
    assert!(
        rep.failures.is_empty(),
        "{} failures:\n{}",
        rep.failures.len(),
        rep.failures.join("\n")
    );
}

#[test]
fn print_dialects() {
    init();
    let schema = support::schema();
    let tz: chrono_tz::Tz = "Asia/Kolkata".parse().unwrap();
    let mut out = String::new();
    for expr in [
        "d(ts) > d('2024-03-05 10:00')",
        "d(loc) > d('2024-03-05 10:00')",
        "d(day) == d('2024-03-05')",
        "d(day) > d(ts)",
        "d(loc) < d(ts)",
        "d(day) < d(loc)",
        "d(ts).hour() == 10",
        "d(day).hour() == 0",
        "d(loc).weekday() == 7",
        "d(ts).week() == 53",
        "d(ts).dayOfYear() == 60",
        "d(ts).quarter() == 1",
        "d(ts).second() == 5",
        "d(ts).add(1, 'month').month() == 3",
        "d(ts).add(1, 'day') > d('2024-03-05')",
        "d(day).add(1, 'week') == d('2024-03-12')",
        "d(loc).add(2, 'quarter') > d('2024-01-01')",
        "d(day).add(1, 'month') == d('2024-03-31')",
        "d(ts).startOf('week') == d('2024-03-04')",
        "d(day).startOf('month') == d('2024-03-01')",
        "d(loc).startOf('day') == d('2024-03-01')",
        "d(ts).isSame(d('2024-03-05'), 'day')",
        "d(ts).isBefore(d(loc), 'month')",
        "d(ts).diff(d('2024-01-01'), 'day') > 3",
        "d(day).diff(d(loc), 'hour') > 3",
        "d(ts).timestamp() > 0",
        "d(loc).timestamp() > 0",
        "d(day).timestamp() > 0",
        "d(txt) > d('2024-01-01')",
        "d(txt).hour() == 3",
        "d(ms) > d('2024-01-01')",
        "d(ms).year() == 2024",
        "d(ts).isToday()",
        "d(day).isToday()",
        "d(loc).isTomorrow()",
        "d(ts) in [d('2024-01-01')..d('2024-02-01'))",
        "d(day) in ['2024-01-01', '2024-01-02']",
        "d(ts).isValid()",
        "d(ts) < d('2024-01-01')",
        "d(ts) <= d(txt)",
    ] {
        out.push_str(&format!("### {expr}\n"));
        for dialect in zen_sql::Dialect::ALL {
            let r = zen_sql::filter(expr, &schema, &zen_sql::Options::new(dialect).timezone(tz));
            match r {
                Ok(c) => out.push_str(&format!("  {:10} {}\n", dialect.name(), c.sql)),
                Err(e) => out.push_str(&format!("  {:10} ERROR {}\n", dialect.name(), e)),
            }
        }
    }
    std::fs::write(
        format!("{}/zen_sql_dialects.txt", env!("CARGO_TARGET_TMPDIR")),
        out,
    )
    .unwrap();
}

#[test]
fn fractional_millis() {
    init();
    use zen_sql::{Dialect, Field, Options, Schema, Source, Ty};
    let schema = Schema::single(Source::new(
        "row",
        vec![
            Field::required("id", Ty::Number),
            Field::new("msf", Ty::Number),
        ],
    ));
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t (id INTEGER, msf DOUBLE); INSERT INTO t VALUES (1, 1500.7), (2, -1500.7), (3, 1499.6);").unwrap();
    let rows = vec![
        serde_json::json!({"id": 1, "msf": 1500.7}),
        serde_json::json!({"id": 2, "msf": -1500.7}),
        serde_json::json!({"id": 3, "msf": 1499.6}),
    ];
    let mut failures = Vec::new();
    for expr in [
        "d(msf) > d(1500)",
        "d(msf) == d(1500)",
        "d(msf) < d(-1500)",
        "d(msf).timestamp() == 1500",
        "d(msf).timestamp() == 1499",
    ] {
        let c = zen_sql::filter(expr, &schema, &Options::new(Dialect::DuckDb)).unwrap();
        let zen = support::zen_ids(&rows, expr);
        let duck: Vec<i64> = conn
            .prepare(&format!("SELECT id FROM t WHERE {} ORDER BY id", c.sql))
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let line = format!("{expr}: zen {zen:?} duck {duck:?}\n   sql {}", c.sql);
        eprintln!("{line}");
        if zen != duck {
            failures.push(line);
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn loose_rows() -> Vec<Value> {
    [
        "2024-3-5",
        " 2024-03-05",
        "2024-03-05 10:00:00Z",
        "2024",
        "2024-03",
        "20240305T1000",
        "2024/03/05 10:00",
        "UTC",
        "Europe/Paris",
    ]
    .iter()
    .enumerate()
    .map(|(i, t)| row(i as i64 + 1, None, None, None, Some(t), None, None))
    .collect()
}

#[test]
#[ignore = "known limit: d() on text columns only reads ISO-8601 dates; looser formats ZEN accepts become invalid dates"]
fn known_limits_loose_text_dates() {
    init();
    let rep = check_duck(
        &loose_rows(),
        &["d(txt).isValid()", "d(txt).year() == 2024"],
        chrono_tz::Tz::UTC,
    );
    assert!(
        rep.failures.is_empty(),
        "{} failures:\n{}",
        rep.failures.len(),
        rep.failures.join("\n")
    );
}
