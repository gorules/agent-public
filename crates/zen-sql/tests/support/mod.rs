#![allow(dead_code)]

use std::process::Command;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, Date32Array, Int32Array, Int64Array, StringArray, TimestampMicrosecondArray,
};
use arrow::datatypes::{DataType, Field as AField, Schema as ASchema, TimeUnit};
use arrow::record_batch::RecordBatch;
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use duckdb::Connection;
use serde_json::{Value, json};
use zen_expression::evaluate_expression;
use zen_sql::{Compiled, Dialect, Field, Options, Schema, Source, Ty};

pub fn schema() -> Schema {
    Schema::single(Source::new(
        "row",
        vec![
            Field::required("id", Ty::Number),
            Field::new("ts", Ty::instant()),
            Field::new("loc", Ty::local()),
            Field::new("day", Ty::date()),
            Field::new("txt", Ty::String),
            Field::new("ms", Ty::Number),
            Field::new("qty", Ty::Number),
        ],
    ))
}

pub fn row(
    id: i64,
    ts: Option<&str>,
    loc: Option<&str>,
    day: Option<&str>,
    txt: Option<&str>,
    ms: Option<i64>,
    qty: Option<i64>,
) -> Value {
    json!({"id": id, "ts": ts, "loc": loc, "day": day, "txt": txt, "ms": ms, "qty": qty})
}

pub fn zen_ids(rows: &[Value], expr: &str) -> Vec<i64> {
    rows.iter()
        .filter(|r| {
            matches!(
                evaluate_expression(expr, (*r).clone().into()),
                Ok(zen_expression::Variable::Bool(true))
            )
        })
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

pub fn zen_value(row: &Value, expr: &str) -> String {
    match evaluate_expression(expr, row.clone().into()) {
        Ok(v) => v.to_string(),
        Err(e) => format!("ERR {e}"),
    }
}

fn lit(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        other => other.to_string(),
    }
}

pub fn duck(rows: &[Value]) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t (id INTEGER NOT NULL, ts TIMESTAMPTZ, loc TIMESTAMP, day DATE, txt VARCHAR, ms BIGINT, qty INTEGER)",
    )
    .unwrap();
    for r in rows {
        let v = |k: &str, ty: &str| {
            if r[k].is_null() {
                "NULL".to_string()
            } else {
                format!("CAST({} AS {ty})", lit(&r[k]))
            }
        };
        conn.execute_batch(&format!(
            "INSERT INTO t VALUES ({}, {}, {}, {}, {}, {}, {})",
            r["id"],
            v("ts", "TIMESTAMPTZ"),
            v("loc", "TIMESTAMP"),
            v("day", "DATE"),
            lit(&r["txt"]),
            lit(&r["ms"]),
            lit(&r["qty"])
        ))
        .unwrap();
    }
    conn
}

pub fn duck_ids(conn: &Connection, c: &Compiled) -> Result<Vec<i64>, String> {
    let sql = format!("SELECT id FROM t WHERE {} ORDER BY id", c.sql);
    let params: Vec<String> = c
        .params
        .iter()
        .map(|p| match p {
            zen_sql::predicate::Scalar::Str(s) => s.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("{e}"))?;
    let ids = stmt
        .query_map(duckdb::params_from_iter(params.iter()), |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| format!("{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{e}"))?;
    Ok(ids)
}

pub fn pg_url() -> Option<String> {
    std::env::var("ZEN_SQL_POSTGRES")
        .ok()
        .filter(|u| !u.is_empty())
}

pub fn psql(url: &str, sql: &str) -> Result<String, String> {
    let out = Command::new("psql")
        .arg(url)
        .args(["-X", "-A", "-t", "-q", "-v", "ON_ERROR_STOP=1", "-c", sql])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn pg_create(url: &str, table: &str, rows: &[Value]) {
    let mut sql = format!(
        "SET TIME ZONE 'UTC'; DROP TABLE IF EXISTS {table}; CREATE TABLE {table} (id integer NOT NULL, ts timestamptz, loc timestamp, day date, txt text, ms bigint, qty integer);"
    );
    for r in rows {
        let v = |k: &str, ty: &str| {
            if r[k].is_null() {
                "NULL".to_string()
            } else {
                format!("CAST({} AS {ty})", lit(&r[k]))
            }
        };
        sql.push_str(&format!(
            "INSERT INTO {table} VALUES ({}, {}, {}, {}, {}, {}, {});",
            r["id"],
            v("ts", "timestamptz"),
            v("loc", "timestamp"),
            v("day", "date"),
            lit(&r["txt"]),
            lit(&r["ms"]),
            lit(&r["qty"])
        ));
    }
    psql(url, &sql).unwrap();
}

pub fn pg_ids(url: &str, table: &str, session_tz: &str, c: &Compiled) -> Result<Vec<i64>, String> {
    assert!(c.params.is_empty());
    let sql = format!(
        "SET TIME ZONE '{session_tz}'; SELECT id FROM {table} WHERE {} ORDER BY id",
        c.sql
    );
    let out = psql(url, &sql)?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.trim().parse().unwrap())
        .collect())
}

pub fn batch(rows: &[Value]) -> RecordBatch {
    let ts: Vec<Option<i64>> = rows
        .iter()
        .map(|r| {
            r["ts"]
                .as_str()
                .map(|s| DateTime::parse_from_rfc3339(s).unwrap().timestamp_micros())
        })
        .collect();
    let loc: Vec<Option<i64>> = rows
        .iter()
        .map(|r| {
            r["loc"].as_str().map(|s| {
                NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                    .unwrap()
                    .and_utc()
                    .timestamp_micros()
            })
        })
        .collect();
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    let day: Vec<Option<i32>> = rows
        .iter()
        .map(|r| {
            r["day"].as_str().map(|s| {
                (NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap() - epoch).num_days() as i32
            })
        })
        .collect();
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(
            rows.iter().map(|r| r["id"].as_i64()).collect::<Vec<_>>(),
        )),
        Arc::new(TimestampMicrosecondArray::from(ts).with_timezone("UTC")),
        Arc::new(TimestampMicrosecondArray::from(loc)),
        Arc::new(Date32Array::from(day)),
        Arc::new(StringArray::from(
            rows.iter().map(|r| r["txt"].as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(Int64Array::from(
            rows.iter().map(|r| r["ms"].as_i64()).collect::<Vec<_>>(),
        )),
        Arc::new(Int32Array::from(
            rows.iter()
                .map(|r| r["qty"].as_i64().map(|q| q as i32))
                .collect::<Vec<_>>(),
        )),
    ];
    let schema = ASchema::new(vec![
        AField::new("id", DataType::Int64, false),
        AField::new(
            "ts",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            true,
        ),
        AField::new(
            "loc",
            DataType::Timestamp(TimeUnit::Microsecond, None),
            true,
        ),
        AField::new("day", DataType::Date32, true),
        AField::new("txt", DataType::Utf8, true),
        AField::new("ms", DataType::Int64, true),
        AField::new("qty", DataType::Int32, true),
    ]);
    RecordBatch::try_new(Arc::new(schema), cols).unwrap()
}

pub fn pushdown_ids(rows: &[Value], c: &Compiled) -> Result<Vec<i64>, String> {
    let preds: Vec<_> = c.pushdown.iter().map(|p| p.predicate.clone()).collect();
    let out = apply(&batch(rows), &preds).map_err(|e| e.to_string())?;
    let ids = out
        .column_by_name("id")
        .unwrap()
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    Ok(ids.iter().map(|v| v.unwrap()).collect())
}

pub fn subset(a: &[i64], b: &[i64]) -> bool {
    a.iter().all(|x| b.contains(x))
}

pub struct Report {
    pub lines: Vec<String>,
    pub failures: Vec<String>,
}

pub fn check_duck(rows: &[Value], exprs: &[&str], tz: chrono_tz::Tz) -> Report {
    let conn = duck(rows);
    let schema = schema();
    let mut rep = Report {
        lines: vec![],
        failures: vec![],
    };
    for expr in exprs {
        for source in [expr.to_string(), format!("not ({expr})")] {
            let zen = zen_ids(rows, &source);
            let opts = Options::new(Dialect::DuckDb).timezone(tz);
            let c = match zen_sql::filter(&source, &schema, &opts) {
                Ok(c) => c,
                Err(e) => {
                    rep.lines.push(format!("{source}: compile error {e}"));
                    continue;
                }
            };
            let sql = duck_ids(&conn, &c);
            let pd = pushdown_ids(rows, &c);
            let line = format!(
                "{source}\n   zen {zen:?} duck {sql:?} pushdown {pd:?}\n   sql {}\n   pd {:?}",
                c.sql,
                c.pushdown.iter().map(|p| &p.predicate).collect::<Vec<_>>()
            );
            let mut bad = false;
            if sql.as_ref().map(|s| s != &zen).unwrap_or(true) {
                bad = true;
            }
            if let Ok(p) = &pd {
                if !subset(&zen, p) || sql.as_ref().map(|s| !subset(s, p)).unwrap_or(false) {
                    bad = true;
                }
            } else {
                bad = true;
            }
            if bad {
                rep.failures.push(line.clone());
            }
            rep.lines.push(line);
        }
    }
    rep
}

pub fn check_pg(
    rows: &[Value],
    exprs: &[&str],
    tz: chrono_tz::Tz,
    session: &str,
    table: &str,
) -> Report {
    let mut rep = Report {
        lines: vec![],
        failures: vec![],
    };
    let Some(url) = pg_url() else {
        rep.lines.push("no postgres".into());
        return rep;
    };
    pg_create(&url, table, rows);
    let schema = schema();
    for expr in exprs {
        for source in [expr.to_string(), format!("not ({expr})")] {
            let zen = zen_ids(rows, &source);
            let opts = Options::new(Dialect::Postgres).timezone(tz);
            let c = match zen_sql::filter(&source, &schema, &opts) {
                Ok(c) => c,
                Err(e) => {
                    rep.lines.push(format!("{source}: compile error {e}"));
                    continue;
                }
            };
            let sql = pg_ids(&url, table, session, &c);
            let pd = pushdown_ids(rows, &c);
            let line = format!(
                "{source} [session {session}]\n   zen {zen:?} pg {sql:?} pushdown {pd:?}\n   sql {}\n   pd {:?}",
                c.sql,
                c.pushdown.iter().map(|p| &p.predicate).collect::<Vec<_>>()
            );
            let mut bad = sql.as_ref().map(|s| s != &zen).unwrap_or(true);
            match &pd {
                Ok(p) => {
                    if !subset(&zen, p) || sql.as_ref().map(|s| !subset(s, p)).unwrap_or(false) {
                        bad = true;
                    }
                }
                Err(_) => bad = true,
            }
            if bad {
                rep.failures.push(line.clone());
            }
            rep.lines.push(line);
        }
    }
    let _ = psql(&url, &format!("DROP TABLE IF EXISTS {table}"));
    rep
}

pub fn dump(name: &str, rep: &Report) {
    let path = format!("{}/zen_sql_{name}.txt", env!("CARGO_TARGET_TMPDIR"));

    let mut text = String::new();
    text.push_str(&format!("FAILURES {}\n", rep.failures.len()));
    for f in &rep.failures {
        text.push_str(f);
        text.push('\n');
    }
    text.push_str("\nALL\n");
    for l in &rep.lines {
        text.push_str(l);
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
}

pub fn apply(
    batch: &arrow::record_batch::RecordBatch,
    filters: &[zen_sql::predicate::Predicate],
) -> anyhow::Result<arrow::record_batch::RecordBatch> {
    use arrow::array::BooleanArray;
    use arrow::compute::{and_kleene, filter_record_batch};
    if filters.is_empty() {
        return Ok(batch.clone());
    }
    let mut mask: Option<BooleanArray> = None;
    for f in filters {
        let m = evaluate(batch, f)?;
        mask = Some(match mask {
            Some(prev) => and_kleene(&prev, &m)?,
            None => m,
        });
    }
    Ok(filter_record_batch(batch, &mask.unwrap())?)
}

fn evaluate(
    batch: &arrow::record_batch::RecordBatch,
    f: &zen_sql::predicate::Predicate,
) -> anyhow::Result<arrow::array::BooleanArray> {
    use anyhow::Context;
    use arrow::array::{Array, BooleanArray};
    use arrow::compute::{is_not_null, is_null, or_kleene};
    use zen_sql::predicate::Op;
    let col = batch
        .column_by_name(&f.column)
        .with_context(|| format!("no column {}", f.column))?;
    let n = col.len();
    match f.op {
        Op::IsNull => return Ok(is_null(col)?),
        Op::IsNotNull => return Ok(is_not_null(col)?),
        Op::In => {
            let mut acc = BooleanArray::from(vec![false; n]);
            for v in &f.values {
                acc = or_kleene(&acc, &compare(col, Op::Eq, v)?)?;
            }
            return Ok(acc);
        }
        _ => {}
    }
    compare(col, f.op, f.values.first().context("missing value")?)
}

fn compare(
    col: &arrow::array::ArrayRef,
    op: zen_sql::predicate::Op,
    v: &zen_sql::predicate::Scalar,
) -> anyhow::Result<arrow::array::BooleanArray> {
    use std::sync::Arc;

    use anyhow::Context;
    use arrow::array::{
        Array, ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, StringArray,
        TimestampMicrosecondArray, new_null_array,
    };
    use arrow::compute::cast;
    use arrow::compute::kernels::cmp;
    use arrow::datatypes::{DataType, TimeUnit};
    use zen_sql::predicate::{Op, Scalar};
    let n = col.len();
    if matches!(v, Scalar::Null) {
        return Ok(BooleanArray::from(vec![None; n]));
    }
    let lit: ArrayRef = match v {
        Scalar::Null => new_null_array(&DataType::Null, 1),
        Scalar::Bool(b) => Arc::new(BooleanArray::from(vec![*b])),
        Scalar::Int(i) => Arc::new(Int64Array::from(vec![*i])),
        Scalar::Float(x) => Arc::new(Float64Array::from(vec![*x])),
        Scalar::Str(s) => Arc::new(StringArray::from(vec![s.as_str()])),
        Scalar::Timestamp(t) => Arc::new(TimestampMicrosecondArray::from(vec![*t])),
        Scalar::Date(d) => Arc::new(Date32Array::from(vec![*d])),
    };
    let numeric = |t: &DataType| t.is_numeric();
    let (left, right): (ArrayRef, ArrayRef) = if numeric(col.data_type())
        && numeric(lit.data_type())
        && col.data_type() != lit.data_type()
    {
        if matches!(v, Scalar::Int(_)) && col.data_type().is_integer() {
            (col.clone(), cast(&lit, col.data_type())?)
        } else {
            (
                cast(col, &DataType::Float64)?,
                cast(&lit, &DataType::Float64)?,
            )
        }
    } else if let DataType::Timestamp(unit, tz) = col.data_type() {
        let target = DataType::Timestamp(TimeUnit::Microsecond, tz.clone());
        let left = if *unit == TimeUnit::Microsecond {
            col.clone()
        } else {
            cast(col, &target)?
        };
        let lit = match v {
            Scalar::Timestamp(t) => {
                Arc::new(TimestampMicrosecondArray::from(vec![*t]).with_timezone_opt(tz.clone()))
                    as ArrayRef
            }
            _ => cast(&lit, &target)?,
        };
        (left, lit)
    } else {
        let target = match col.data_type() {
            DataType::Dictionary(_, value) => value.as_ref().clone(),
            other => other.clone(),
        };
        let left = if matches!(col.data_type(), DataType::Dictionary(..)) {
            cast(col, &target)?
        } else {
            col.clone()
        };
        (
            left,
            cast(&lit, &target)
                .with_context(|| format!("cannot compare {} with {v:?}", col.data_type()))?,
        )
    };
    let right = arrow::array::Scalar::new(right);
    Ok(match op {
        Op::Eq => cmp::eq(&left, &right)?,
        Op::Ne => cmp::neq(&left, &right)?,
        Op::Lt => cmp::lt(&left, &right)?,
        Op::Le => cmp::lt_eq(&left, &right)?,
        Op::Gt => cmp::gt(&left, &right)?,
        Op::Ge => cmp::gt_eq(&left, &right)?,
        Op::Prefix => arrow::compute::kernels::comparison::starts_with(&left, &right)?,
        Op::In | Op::IsNull | Op::IsNotNull => unreachable!(),
    })
}
