mod common;

use common::{FILTERS, context, init, rows, schema, zen_ids, zen_unary_ids};
use duckdb::Connection;
use serde_json::{Value, json};
use zen_expression::evaluate_expression;
use zen_sql::{Dialect, Options};

const CREATE: &str = r#"
CREATE TABLE t (
    id INTEGER NOT NULL,
    amount DOUBLE,
    qty INTEGER,
    name VARCHAR,
    tier VARCHAR,
    flag BOOLEAN,
    created TIMESTAMPTZ,
    created_text VARCHAR,
    created_ms BIGINT,
    day DATE,
    local TIMESTAMP,
    tags VARCHAR[],
    scores DOUBLE[],
    items STRUCT(sku VARCHAR, price DOUBLE)[],
    customer STRUCT(tier VARCHAR, age INTEGER, address STRUCT(city VARCHAR)),
    attrs MAP(VARCHAR, VARCHAR),
    payload JSON,
    "input.amount" DOUBLE
)"#;

fn sql_literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        other => other.to_string(),
    }
}

fn duck_value(key: &str, v: &Value) -> String {
    if v.is_null() {
        return "NULL".into();
    }
    match key {
        "tags" => format!(
            "[{}]::VARCHAR[]",
            v.as_array()
                .unwrap()
                .iter()
                .map(sql_literal)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "scores" => format!(
            "[{}]::DOUBLE[]",
            v.as_array()
                .unwrap()
                .iter()
                .map(sql_literal)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "items" => {
            let items: Vec<String> = v
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    format!(
                        "{{'sku': {}, 'price': {}}}",
                        sql_literal(&i["sku"]),
                        sql_literal(&i["price"])
                    )
                })
                .collect();
            format!(
                "[{}]::STRUCT(sku VARCHAR, price DOUBLE)[]",
                items.join(", ")
            )
        }
        "customer" => {
            let addr = if v["address"].is_null() {
                "NULL".to_string()
            } else {
                format!("{{'city': {}}}", sql_literal(&v["address"]["city"]))
            };
            format!(
                "{{'tier': {}, 'age': {}, 'address': {addr}}}::STRUCT(tier VARCHAR, age INTEGER, address STRUCT(city VARCHAR))",
                sql_literal(&v["tier"]),
                sql_literal(&v["age"])
            )
        }
        "attrs" => {
            let obj = v.as_object().unwrap();
            let keys: Vec<String> = obj.keys().map(|k| sql_literal(&json!(k))).collect();
            let vals: Vec<String> = obj.values().map(sql_literal).collect();
            format!(
                "MAP([{}]::VARCHAR[], [{}]::VARCHAR[])",
                keys.join(", "),
                vals.join(", ")
            )
        }
        "payload" => format!("{}::JSON", sql_literal(&json!(v.to_string()))),
        "created" => format!("{}::TIMESTAMPTZ", sql_literal(v)),
        "day" => format!("{}::DATE", sql_literal(v)),
        "local" => format!("{}::TIMESTAMP", sql_literal(v)),
        _ => sql_literal(v),
    }
}

const COLUMNS: [&str; 18] = [
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
    "items",
    "customer",
    "attrs",
    "payload",
    "input.amount",
];

fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(CREATE).unwrap();
    for row in rows() {
        let values: Vec<String> = COLUMNS.iter().map(|c| duck_value(c, &row[*c])).collect();
        conn.execute_batch(&format!("INSERT INTO t VALUES ({})", values.join(", ")))
            .unwrap();
    }
    conn
}

fn duck_ids(conn: &Connection, compiled: &zen_sql::Compiled) -> Result<Vec<i64>, String> {
    let sql = format!("SELECT id FROM t WHERE {} ORDER BY id", compiled.sql);
    let params: Vec<String> = compiled
        .params
        .iter()
        .map(|p| match p {
            zen_sql::predicate::Scalar::Str(s) => s.clone(),
            other => panic!("unexpected param {other:?}"),
        })
        .collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("{e}\n{sql}"))?;
    let ids = stmt
        .query_map(duckdb::params_from_iter(params.iter()), |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| format!("{e}\n{sql}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{e}\n{sql}"))?;
    Ok(ids)
}

#[test]
fn filters_match_zen_on_duckdb() {
    init();
    let conn = db();
    let schema = schema();
    let mut failures = Vec::new();
    for expr in FILTERS {
        for _once in 0..1 {
            let options = Options::new(Dialect::DuckDb);
            for (label, source) in [("", expr.to_string()), ("not ", format!("not ({expr})"))] {
                let expected = zen_ids(&source);
                let compiled = match zen_sql::filter(&source, &schema, &options) {
                    Ok(c) => c,
                    Err(e) => {
                        failures.push(format!("{label}{expr}: compile error {e}"));
                        continue;
                    }
                };
                match duck_ids(&conn, &compiled) {
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
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn unary_tests_match_zen_on_duckdb() {
    init();
    let conn = db();
    let schema = schema();
    let cases = [
        ("tier", "'gold', 'silver'"),
        ("tier", "\"gold\""),
        ("amount", "> 100"),
        ("amount", "[10..300]"),
        ("amount", "< 0 or > 500"),
        ("amount", "not in [10.5, 250]"),
        ("amount", "$ > 5 and $ < 300"),
        ("qty", "1, 2, 3"),
        ("name", "startsWith($, 'A')"),
        ("flag", "true"),
        ("created", "> d('2024-06-01')"),
        ("created", "d($) in [d('2024-01-01')..d('2024-06-30')]"),
        ("created", "d($).year() == 2024"),
        ("day", "d($) < d('2024-06-01')"),
    ];
    let mut failures = Vec::new();
    for (subject, expr) in cases {
        let expected = zen_unary_ids(expr, subject);
        let options = Options::new(Dialect::DuckDb);
        match zen_sql::unary(expr, subject, &schema, &options) {
            Ok(compiled) => match duck_ids(&conn, &compiled) {
                Ok(actual) if actual == expected => {}
                Ok(actual) => failures.push(format!(
                    "{subject}: {expr}: zen {expected:?} sql {actual:?}\n  {}",
                    compiled.sql
                )),
                Err(e) => failures.push(format!("{subject}: {expr}: {e}")),
            },
            Err(e) => failures.push(format!("{subject}: {expr}: compile error {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn values_match_zen_on_duckdb() {
    init();
    let conn = db();
    let schema = schema();
    let cases = [
        "upper(name)",
        "qty + 1",
        "tier ?? 'none'",
        "d(created).year()",
        "amount > 100",
        "not (qty == 5)",
        "customer.address.city",
        "len(tags)",
        "`${tier}:${qty}`",
        "d(created).diff(d('2024-01-01'), 'day')",
    ];
    let mut failures = Vec::new();
    for expr in cases {
        let compiled = match zen_sql::value(expr, &schema, &Options::default()) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{expr}: compile error {e}"));
                continue;
            }
        };
        let sql = format!(
            "SELECT id, CAST({} AS VARCHAR) FROM t ORDER BY id",
            compiled.sql
        );
        let params: Vec<String> = compiled
            .params
            .iter()
            .map(|p| match p {
                zen_sql::predicate::Scalar::Str(s) => s.clone(),
                other => panic!("unexpected param {other:?}"),
            })
            .collect();
        let mut stmt = conn.prepare(&sql).unwrap();
        let got: Vec<(i64, Option<String>)> = stmt
            .query_map(duckdb::params_from_iter(params.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for (row, (id, sql_value)) in rows().iter().zip(got) {
            let zen = evaluate_expression(expr, context(row).into());
            let zen = match zen {
                Ok(zen_expression::Variable::Null) | Err(_) => None,
                Ok(zen_expression::Variable::String(s)) => Some(s.to_string()),
                Ok(other) => Some(other.to_string()),
            };
            let normalize = |s: Option<String>| {
                s.map(|s| match s.parse::<f64>() {
                    Ok(f) => format!("{f}"),
                    Err(_) => s,
                })
            };
            if normalize(zen.clone()) != normalize(sql_value.clone()) {
                failures.push(format!(
                    "{expr} row {id}: zen {zen:?} sql {sql_value:?}\n  {}",
                    compiled.sql
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
