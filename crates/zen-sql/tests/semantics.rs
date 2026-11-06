mod common;

use std::process::Command;

use common::init;
use duckdb::Connection;
use serde_json::{Value, json};
use zen_expression::{evaluate_expression, evaluate_unary_expression};
use zen_sql::{Compiled, Dialect, Field, Options, Schema, Source, Ty};

fn rows() -> Vec<Value> {
    vec![
        json!({"id": 1, "n": 10.5, "i": 5, "s": "Alice", "t": "gold", "b": true,
               "j": {"score": 7, "kind": "x", "ok": true}}),
        json!({"id": 2, "n": 250.0, "i": 0, "s": "\tpad\n", "t": null, "b": false,
               "j": {"score": "7", "kind": 5, "ok": "true"}}),
        json!({"id": 3, "n": null, "i": null, "s": null, "t": null, "b": null, "j": null}),
        json!({"id": 4, "n": -3.7, "i": 1, "s": "inf", "t": "x", "b": null,
               "j": {"score": "n/a", "nested": {"a": 1}}}),
        json!({"id": 5, "n": 0.0, "i": 3, "s": "straße", "t": "STRASSE", "b": true,
               "j": {"score": 100}}),
        json!({"id": 6, "n": 1000.0, "i": 2, "s": " 12 ", "t": "nan", "b": false,
               "j": {"score": [1, 2]}}),
        json!({"id": 7, "n": 99.99, "i": 12, "s": "1_000", "t": "Infinity", "b": null,
               "j": {}}),
        json!({"id": 8, "n": 2.675, "i": null, "s": "+5", "t": "gold", "b": true,
               "j": {"kind": "x"}}),
    ]
}

fn schema() -> Schema {
    Schema::single(Source::new(
        "h",
        vec![
            Field::required("id", Ty::Number),
            Field::new("n", Ty::Number),
            Field::new("i", Ty::Number),
            Field::new("s", Ty::String),
            Field::new("t", Ty::String),
            Field::new("b", Ty::Bool),
            Field::new("j", Ty::Json),
        ],
    ))
}

const COLUMNS: [&str; 7] = ["id", "n", "i", "s", "t", "b", "j"];

fn lit(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        other => other.to_string(),
    }
}

fn cell(dialect: Dialect, key: &str, v: &Value) -> String {
    if v.is_null() {
        return "NULL".into();
    }
    match (key, dialect) {
        ("j", Dialect::DuckDb) => format!("{}::JSON", lit(&json!(v.to_string()))),
        ("j", _) => format!("{}::jsonb", lit(&json!(v.to_string()))),
        ("s", Dialect::Postgres) => format!(
            "E'{}'",
            v.as_str()
                .unwrap()
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('\t', "\\t")
                .replace('\n', "\\n")
        ),
        _ => lit(v),
    }
}

fn zen_ids(expr: &str) -> Vec<i64> {
    rows()
        .iter()
        .filter(|row| {
            matches!(
                evaluate_expression(expr, (*row).clone().into()),
                Ok(zen_expression::Variable::Bool(true))
            )
        })
        .map(|row| row["id"].as_i64().unwrap())
        .collect()
}

fn zen_unary_ids(expr: &str, subject: &str) -> Vec<i64> {
    rows()
        .iter()
        .filter(|row| {
            let mut ctx: Value = (*row).clone();
            let value = ctx[subject].clone();
            ctx.as_object_mut().unwrap().insert("$".into(), value);
            matches!(evaluate_unary_expression(expr, ctx.into()), Ok(true))
        })
        .map(|row| row["id"].as_i64().unwrap())
        .collect()
}

fn duck() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE h (id INTEGER NOT NULL, n DOUBLE, i INTEGER, s VARCHAR, t VARCHAR, b BOOLEAN, j JSON)",
    )
    .unwrap();
    for row in rows() {
        let values: Vec<String> = COLUMNS
            .iter()
            .map(|c| cell(Dialect::DuckDb, c, &row[*c]))
            .collect();
        conn.execute_batch(&format!("INSERT INTO h VALUES ({})", values.join(", ")))
            .unwrap();
    }
    conn
}

fn texts(c: &Compiled) -> Vec<String> {
    c.params
        .iter()
        .map(|p| match p {
            zen_sql::predicate::Scalar::Str(s) => s.clone(),
            other => panic!("unexpected param {other:?}"),
        })
        .collect()
}

fn duck_ids(conn: &Connection, c: &Compiled) -> Result<Vec<i64>, String> {
    let sql = format!("SELECT id FROM h WHERE {} ORDER BY id", c.sql);
    let params = texts(c);
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    stmt.query_map(duckdb::params_from_iter(params.iter()), |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| e.to_string())?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| e.to_string())
}

fn pg_url() -> Option<String> {
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
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct Pg {
    url: String,
    table: String,
}

impl Pg {
    fn new(tag: &str) -> Option<Pg> {
        let url = pg_url()?;
        let table = format!("hunt_sem_{tag}_{}", std::process::id());
        let mut sql = format!(
            "DROP TABLE IF EXISTS {table}; CREATE TABLE {table} (id integer NOT NULL, n double precision, i integer, s text, t text, b boolean, j jsonb);"
        );
        for row in rows() {
            let values: Vec<String> = COLUMNS
                .iter()
                .map(|c| cell(Dialect::Postgres, c, &row[*c]))
                .collect();
            sql.push_str(&format!(
                "INSERT INTO {table} VALUES ({});",
                values.join(", ")
            ));
        }
        psql(&url, &sql).unwrap();
        Some(Pg { url, table })
    }

    fn ids(&self, c: &Compiled) -> Result<Vec<i64>, String> {
        let select = format!(
            "SELECT id FROM {} AS h WHERE {} ORDER BY id",
            self.table, c.sql
        );
        let params = texts(c);
        let q = if params.is_empty() {
            format!("SET TIME ZONE 'UTC'; {select}")
        } else {
            let types = vec!["text"; params.len()].join(", ");
            let args: Vec<String> = params
                .iter()
                .map(|v| format!("E'{}'", v.replace('\\', "\\\\").replace('\'', "''")))
                .collect();
            format!(
                "SET TIME ZONE 'UTC'; PREPARE q({types}) AS {select}; EXECUTE q({});",
                args.join(", ")
            )
        };
        let out = psql(&self.url, &q)?;
        Ok(out
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.trim().parse().unwrap())
            .collect())
    }
}

impl Drop for Pg {
    fn drop(&mut self) {
        let _ = psql(&self.url, &format!("DROP TABLE IF EXISTS {}", self.table));
    }
}

fn exec(
    dialect: Dialect,
    conn: &Connection,
    pg: Option<&Pg>,
    c: &Compiled,
) -> Option<Result<Vec<i64>, String>> {
    match dialect {
        Dialect::DuckDb => Some(duck_ids(conn, c)),
        Dialect::Postgres => pg.map(|p| p.ids(c)),
        _ => None,
    }
}

fn check_filters(tag: &str, cases: &[&str]) {
    init();
    let conn = duck();
    let pg = Pg::new(tag);
    let schema = schema();
    let mut failures = Vec::new();
    for expr in cases {
        for dialect in [Dialect::DuckDb, Dialect::Postgres] {
            let options = Options::new(dialect);
            for source in [expr.to_string(), format!("not ({expr})")] {
                let expected = zen_ids(&source);
                let compiled = match zen_sql::filter(&source, &schema, &options) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[info] {dialect:?} `{source}`: compile error: {e}");
                        continue;
                    }
                };
                match exec(dialect, &conn, pg.as_ref(), &compiled) {
                    None => {}
                    Some(Ok(actual)) if actual == expected => {}
                    Some(Ok(actual)) => failures.push(format!(
                        "{dialect:?} `{source}`: zen {expected:?} sql {actual:?}\n    {}",
                        compiled.sql
                    )),
                    Some(Err(e)) => failures.push(format!(
                        "{dialect:?} `{source}`: SQL error (zen {expected:?}): {e}\n    {}",
                        compiled.sql
                    )),
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

fn check_unary(tag: &str, cases: &[(&str, &str)]) {
    init();
    let conn = duck();
    let pg = Pg::new(tag);
    let schema = schema();
    let mut failures = Vec::new();
    for (subject, expr) in cases {
        for dialect in [Dialect::DuckDb, Dialect::Postgres] {
            let options = Options::new(dialect);
            let expected = zen_unary_ids(expr, subject);
            let compiled = match zen_sql::unary(expr, subject, &schema, &options) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[info] {dialect:?} {subject}: `{expr}`: compile error: {e}");
                    continue;
                }
            };
            match exec(dialect, &conn, pg.as_ref(), &compiled) {
                None => {}
                Some(Ok(actual)) if actual == expected => {}
                Some(Ok(actual)) => failures.push(format!(
                    "{dialect:?} {subject}: `{expr}`: zen {expected:?} sql {actual:?}\n    {}",
                    compiled.sql
                )),
                Some(Err(e)) => failures.push(format!(
                    "{dialect:?} {subject}: `{expr}`: SQL error (zen {expected:?}): {e}\n    {}",
                    compiled.sql
                )),
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
fn logic_left_operand_errors() {
    check_filters(
        "logic",
        &[
            "b or true",
            "b and false",
            "b or t == 'x'",
            "b and i > 5",
            "n > 100 or true",
            "n > 100 and false",
            "true or b",
            "false and b",
        ],
    );
}

#[test]
fn error_vs_null_conflation() {
    check_filters(
        "conflate",
        &[
            "(i > 3) == null",
            "(n > 100) == false",
            "(n > 100) != true",
            "b == (i > 3)",
            "startsWith(s, 'A') == null",
            "contains(s, 'li') == false",
            "(i > 3 ? n : 0) == null",
            "bool(i > 3)",
            "bool(b)",
        ],
    );
}

#[test]
fn coalesce_does_not_catch_errors() {
    check_filters(
        "coalesce",
        &[
            "(n > 100 ?? true)",
            "(n / i ?? 1) == 1",
            "(n / i ?? 0) >= 0",
            "(i % 0 ?? 1) == 1",
            "(n / 0 ?? 7) == 7",
        ],
    );
}

#[test]
fn in_list_with_nullable_elements() {
    check_filters(
        "inlist",
        &[
            "s in [t, 'x']",
            "t in [s, 'gold']",
            "i in [n, 5]",
            "b in [true, false]",
            "t not in ['gold', 'x']",
        ],
    );
}

#[test]
fn json_type_confusion() {
    check_filters(
        "json",
        &[
            "j.score == '7'",
            "j.score == 7",
            "j.kind == '5'",
            "j.kind == 5",
            "j.ok == 'true'",
            "j.ok == true",
            "bool(j.score)",
            "bool(j.kind)",
            "string(j.nested) != ''",
            "string(j.score) == '100'",
            "number(j.score) == 7",
            "j.score in [7, 100]",
            "j.score in ['7']",
        ],
    );
}

#[test]
fn string_functions() {
    check_filters(
        "strings",
        &[
            "trim(s) == 'pad'",
            "trim(s) == '12'",
            "lower(t) == 'strasse'",
            "len(s) == 6",
            "len(s) == 7",
            "endsWith(s, '')",
            "startsWith(s, '')",
            "contains(s, '')",
            "matches(s, '^\\s')",
            "matches(s, '\\d')",
            "s + t == 'Alicegold'",
            "`${s}` == 'null'",
        ],
    );
}

#[test]
fn number_conversion() {
    check_filters(
        "numconv",
        &[
            "isNumeric(s)",
            "isNumeric(t)",
            "number(s) > 0",
            "number(t) > 0",
            "number(s) == 12",
            "number(s) == 1000",
            "number(s) == 5",
            "isNumeric(n)",
            "number(b) == 1",
        ],
    );
}

#[test]
fn number_formatting() {
    check_filters(
        "numfmt",
        &["string(i * 1.5) == '7.5'", "string(b) == 'true'"],
    );
}

#[test]
fn arithmetic_edges() {
    check_filters(
        "arith",
        &[
            "n ^ 0.5 > 1",
            "i ^ -1 > 1",
            "(-n) ^ 0.5 > 0",
            "n % 2 == 0.5",
            "n % 3 == 1",
            "i % 2 == 1",
            "round(n, 2) == 2.68",
            "round(n, -1) == 250",
            "round(n) == 11",
            "round(-n) == -11",
            "trunc(n) == -3",
            "-i == 0",
            "-n < 0",
            "i - -1 == 6",
            "abs(n) == 3.7",
            "0.1 + 0.2 == 0.3",
            "n + 0.01 == 100",
        ],
    );
}

#[test]
fn ternary_and_not() {
    check_filters(
        "ternary",
        &[
            "b ? true : false",
            "b ? false : true",
            "(i > 3 ? 'big' : 'small') == 'small'",
            "not b",
            "not not b",
            "(b ? n : i) > 2",
            "(b ?? false) ? true : false",
        ],
    );
}

#[test]
fn unary_mode() {
    check_unary(
        "unary",
        &[
            ("n", "> 100, null"),
            ("n", "> 100 or $ == null"),
            ("n", "null"),
            ("n", "$ == null or > 100"),
            ("t", "'gold', null"),
            ("t", "null, 'gold'"),
            ("i", "[1..5], 12"),
            ("i", "not($ > 3)"),
            ("i", "-1"),
            ("b", "false"),
            ("b", "$"),
            ("b", "not $"),
            ("s", "''"),
            ("s", "trim($) == 'pad'"),
            ("s", "len($) > 3"),
            ("n", "$ ^ 0.5 > 1"),
            ("j", "7"),
            ("j", "'7'"),
            ("j", "'x', 5"),
        ],
    );
}

#[test]
fn division_and_modulo_nulls() {
    check_filters(
        "divmod",
        &[
            "n / i == null",
            "i % 0 == null",
            "(i % 0) == null",
            "i % i == null",
        ],
    );
}

#[test]
#[ignore = "known limits: SQL engines use floating point and their own number formatting and Unicode case mapping, while ZEN uses exact decimals"]
fn known_limits() {
    check_filters(
        "known",
        &[
            "upper(s) == 'STRASSE'",
            "upper(s) == t",
            "string(n) == '250'",
            "string(n) == '250.0'",
            "string(n) == '0'",
            "string(n / 2) == '5.25'",
            "string(n / 1000000) == '0.0000105'",
            "string(n * 1000000000000000000000) == '10500000000000000000000.0'",
            "`${n / 2}` == '5.25'",
            "string(i / 2) == '2.5'",
            "i / 3 * 3 == i",
            "n / 3 * 3 == n",
            "n * 1000000000000000000000000000 > 1",
        ],
    );
}
