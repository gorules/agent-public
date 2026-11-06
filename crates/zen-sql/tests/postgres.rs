mod common;

use std::process::Command;

use common::{FILTERS, init, rows, zen_ids};
use serde_json::Value;
use zen_sql::{Dialect, Options, Schema};

const UNSUPPORTED_COLUMNS: [&str; 3] = ["items", "customer", "attrs"];

fn url() -> Option<String> {
    std::env::var("ZEN_SQL_POSTGRES")
        .ok()
        .filter(|u| !u.is_empty())
}

fn psql(url: &str, sql: &str) -> Result<String, String> {
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

fn literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        other => other.to_string(),
    }
}

fn value(key: &str, v: &Value) -> String {
    if v.is_null() {
        return "NULL".into();
    }
    match key {
        "tags" => format!(
            "ARRAY[{}]::text[]",
            v.as_array()
                .unwrap()
                .iter()
                .map(literal)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "scores" => format!(
            "ARRAY[{}]::double precision[]",
            v.as_array()
                .unwrap()
                .iter()
                .map(literal)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "payload" => format!("{}::jsonb", literal(&Value::String(v.to_string()))),
        "created" => format!("{}::timestamptz", literal(v)),
        "day" => format!("{}::date", literal(v)),
        "local" => format!("{}::timestamp", literal(v)),
        _ => literal(v),
    }
}

const COLUMNS: [&str; 15] = [
    "id",
    "amount",
    "qty",
    "name",
    "tier",
    "flag",
    "created",
    "created_text",
    "created_ms",
    "day",
    "local",
    "tags",
    "scores",
    "payload",
    "input.amount",
];

fn schema() -> Schema {
    let mut s = common::schema();
    let source = &mut s.sources[0];
    source
        .fields
        .retain(|f| !UNSUPPORTED_COLUMNS.contains(&f.name.as_str()));
    s
}

fn create(url: &str, table: &str) {
    let mut sql = format!(
        "SET TIME ZONE 'UTC'; DROP TABLE IF EXISTS {table}; CREATE TABLE {table} (
            id integer NOT NULL, amount double precision, qty integer, name text, tier text, flag boolean,
            created timestamptz, created_text text, created_ms bigint, day date, local timestamp,
            tags text[], scores double precision[], payload jsonb, \"input.amount\" double precision);"
    );
    for row in rows() {
        let values: Vec<String> = COLUMNS.iter().map(|c| value(c, &row[*c])).collect();
        sql.push_str(&format!(
            "INSERT INTO {table} VALUES ({});",
            values.join(", ")
        ));
    }
    psql(url, &sql).unwrap();
}

fn ids(url: &str, table: &str, compiled: &zen_sql::Compiled) -> Result<Vec<i64>, String> {
    let select = format!("SELECT id FROM {table} WHERE {} ORDER BY id", compiled.sql);
    let sql = if compiled.params.is_empty() {
        format!("SET TIME ZONE 'UTC'; {select}")
    } else {
        let types = vec!["text"; compiled.params.len()].join(", ");
        let args: Vec<String> = compiled
            .params
            .iter()
            .map(|p| match p {
                zen_sql::predicate::Scalar::Str(s) => format!("'{}'", s.replace('\'', "''")),
                other => panic!("unexpected param {other:?}"),
            })
            .collect();
        format!(
            "SET TIME ZONE 'UTC'; PREPARE q({types}) AS {select}; EXECUTE q({});",
            args.join(", ")
        )
    };
    let out = psql(url, &sql).map_err(|e| format!("{e}\n{sql}"))?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.trim().parse().unwrap())
        .collect())
}

#[test]
fn filters_match_zen_on_postgres() {
    let Some(url) = url() else {
        eprintln!("ZEN_SQL_POSTGRES is not set; skipping");
        return;
    };
    init();
    let table = format!("zen_sql_t_{}", std::process::id());
    create(&url, &table);
    let schema = schema();
    let mut failures = Vec::new();
    for expr in FILTERS {
        if UNSUPPORTED_COLUMNS.iter().any(|c| expr.contains(c)) {
            continue;
        }
        for _once in 0..1 {
            let options = Options::new(Dialect::Postgres);
            for (label, source) in [("", expr.to_string()), ("not ", format!("not ({expr})"))] {
                let expected = zen_ids(&source);
                let compiled = match zen_sql::filter(&source, &schema, &options) {
                    Ok(c) => c,
                    Err(e) => {
                        failures.push(format!("{label}{expr}: compile error {e}"));
                        continue;
                    }
                };
                match ids(&url, &table, &compiled) {
                    Ok(actual) if actual == expected => {}
                    Ok(actual) => failures.push(format!(
                        "{label}{expr}: zen {expected:?} sql {actual:?}\n  {}",
                        compiled.sql
                    )),
                    Err(e) => failures.push(format!("{label}{expr}: {e}")),
                }
            }
        }
    }
    let _ = psql(&url, &format!("DROP TABLE IF EXISTS {table}"));
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
