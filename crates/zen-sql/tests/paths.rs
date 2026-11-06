mod common;

use duckdb::Connection;
use serde_json::{Value, json};
use zen_expression::evaluate_expression;
use zen_sql::{Compiled, Dialect, Field, Options, Schema, Source, Ty};

struct Fixture {
    conn: Connection,
    schema: Schema,
    rows: Vec<Value>,
}

fn fixture(cols: &[(&str, &str, Ty)], rows: Vec<Value>) -> Fixture {
    common::init();
    let conn = Connection::open_in_memory().unwrap();
    let defs: Vec<String> = cols
        .iter()
        .map(|(n, t, _)| format!("\"{}\" {t}", n.replace('"', "\"\"")))
        .collect();
    conn.execute_batch(&format!("CREATE TABLE t ({})", defs.join(", ")))
        .unwrap();
    let structure: serde_json::Map<String, Value> = cols
        .iter()
        .map(|(n, t, _)| (n.to_string(), Value::String(t.to_string())))
        .collect();
    let structure = Value::Object(structure).to_string();
    for row in &rows {
        let names: Vec<String> = cols
            .iter()
            .map(|(n, _, _)| format!("s.\"{}\"", n.replace('"', "\"\"")))
            .collect();
        let sql = format!(
            "INSERT INTO t SELECT {} FROM (SELECT json_transform(CAST(? AS JSON), ?) AS s)",
            names.join(", ")
        );
        conn.execute(&sql, duckdb::params![row.to_string(), structure])
            .unwrap_or_else(|e| panic!("{e}\n{sql}\n{row}"));
    }
    let schema = Schema::single(Source::new(
        "row",
        cols.iter()
            .map(|(n, _, ty)| Field::new(*n, ty.clone()))
            .collect(),
    ));
    Fixture { conn, schema, rows }
}

fn zen(rows: &[Value], expr: &str) -> Vec<i64> {
    rows.iter()
        .filter(|row| {
            matches!(
                evaluate_expression(expr, (*row).clone().into()),
                Ok(zen_expression::Variable::Bool(true))
            )
        })
        .map(|row| row["id"].as_i64().unwrap())
        .collect()
}

fn zen_raw(row: &Value, expr: &str) -> String {
    match evaluate_expression(expr, row.clone().into()) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR {e}"),
    }
}

fn duck(conn: &Connection, c: &Compiled) -> Result<Vec<i64>, String> {
    let sql = format!("SELECT id FROM t WHERE {} ORDER BY id", c.sql);
    let params: Vec<String> = c
        .params
        .iter()
        .map(|p| match p {
            zen_sql::predicate::Scalar::Str(s) => s.clone(),
            other => panic!("unexpected param {other:?}"),
        })
        .collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("{e}\n{sql}"))?;
    stmt.query_map(duckdb::params_from_iter(params.iter()), |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| format!("{e}\n{sql}"))?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| format!("{e}\n{sql}"))
}

fn mismatches(f: &Fixture, exprs: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for expr in exprs {
        for source in [expr.to_string(), format!("not ({expr})")] {
            let expected = zen(&f.rows, &source);
            for _once in 0..1 {
                let options = Options::new(Dialect::DuckDb);
                let compiled = match zen_sql::filter(&source, &f.schema, &options) {
                    Ok(c) => c,
                    Err(e) => {
                        {
                            out.push(format!("COMPILE-ERR {source}: {e}"));
                        }
                        break;
                    }
                };
                match duck(&f.conn, &compiled) {
                    Ok(actual) if actual == expected => {}
                    Ok(actual) => {
                        let per_row: Vec<String> = f
                            .rows
                            .iter()
                            .filter(|r| {
                                let id = r["id"].as_i64().unwrap();
                                expected.contains(&id) != actual.contains(&id)
                            })
                            .map(|r| format!("{}={}", r["id"], zen_raw(r, &source)))
                            .collect();
                        out.push(format!(
                            "MISMATCH {source}: zen {expected:?} sql {actual:?}\n    sql: {}\n    zen: {}",
                            compiled.sql,
                            per_row.join(" | ")
                        ));
                    }
                    Err(e) => out.push(format!("SQL-ERR {source}: {e}")),
                }
            }
        }
    }
    out
}

fn report(name: &str, f: &Fixture, exprs: &[&str]) -> Vec<String> {
    let m = mismatches(f, exprs);
    for line in &m {
        eprintln!("[{name}] {line}");
    }
    m
}

fn json_fixture() -> Fixture {
    fixture(
        &[("id", "INTEGER", Ty::Number), ("p", "JSON", Ty::Json)],
        vec![
            json!({"id": 1, "p": {"n": 7, "s": "x", "b": true, "o": {"k": 1}, "l": [1, 2], "z": null}}),
            json!({"id": 2, "p": {"n": "7", "s": 5, "b": "true", "o": "str", "l": "notalist"}}),
            json!({"id": 3, "p": {"n": 7.0, "s": "5", "b": 1, "o": null, "l": []}}),
            json!({"id": 4, "p": {}}),
            json!({"id": 5, "p": null}),
            json!({"id": 6, "p": {"n": true, "s": true, "b": false, "o": [1], "l": [null]}}),
            json!({"id": 7, "p": {"a.b": 1, "a\"b": 2, "": 3, "ü": 4, "a'b": 5, "a\\b": 6, "a]b": 7, "a b": 8, "$": 9, "*": 10}}),
            json!({"id": 8, "p": {"a": {"b": 2}}}),
            json!({"id": 9, "p": {"n": "abc", "b": "TRUE", "s": "", "z": "null"}}),
            json!({"id": 10, "p": {"n": " 7 ", "b": "t", "s": null}}),
            json!({"id": 11, "p": {"n": 1e300, "b": "yes", "s": "null"}}),
            json!({"id": 12, "p": {"n": "1e2", "b": 0}}),
        ],
    )
}

#[test]
#[ignore = "exploration dump"]
fn explore_json() {
    let f = json_fixture();
    let m = report(
        "json",
        &f,
        &[
            "p.n == 7",
            "p.n != 7",
            "p.n > 5",
            "p.n == '7'",
            "p.s == '5'",
            "p.s == 'x'",
            "p.s == ''",
            "p.b == true",
            "p.b",
            "p.b == false",
            "p.z == null",
            "p.missing == null",
            "p.z != null",
            "p.o == null",
            "p.o.k == 1",
            "p.l[0] == 1",
            "p.l[0] == null",
            "p['a.b'] == 1",
            "p['a\"b'] == 2",
            "p[''] == 3",
            "p['ü'] == 4",
            "p[\"a'b\"] == 5",
            "p['a\\\\b'] == 6",
            "p['a]b'] == 7",
            "p['a b'] == 8",
            "p['$'] == 9",
            "p['*'] == 10",
            "p.a.b == 2",
            "p['a'].b == 2",
            "p.n in [7, 8]",
            "p.s in ['x', 'y']",
            "p.n in [1..10]",
            "number(p.n) == 7",
            "string(p.s) == '5'",
            "string(p.n) == '7'",
            "string(p.z) == 'null'",
            "isNumeric(p.n)",
            "contains(p.s, 'x')",
            "startsWith(p.s, 'x')",
            "p.s + 'y' == 'xy'",
            "p.n + 1 == 8",
            "bool(p.b)",
            "(p.n ?? 0) == 7",
            "(p.z ?? 'd') == 'd'",
            "p == null",
            "p.s == p.s",
            "lower(p.s) == 'x'",
            "len(string(p.s)) == 1",
            "p.n == 7 and p.s == 'x'",
            "-p.n == -7",
        ],
    );
    assert!(m.is_empty());
}

fn list_fixture() -> Fixture {
    let item = Ty::object(vec![
        Field::new("sku", Ty::String),
        Field::new("price", Ty::Number),
    ]);
    fixture(
        &[
            ("id", "INTEGER", Ty::Number),
            ("tags", "VARCHAR[]", Ty::list(Ty::String)),
            ("nums", "DOUBLE[]", Ty::list(Ty::Number)),
            (
                "items",
                "STRUCT(sku VARCHAR, price DOUBLE)[]",
                Ty::list(item),
            ),
            ("m", "MAP(VARCHAR, VARCHAR)", Ty::map(Ty::String)),
            (
                "s",
                "STRUCT(a INTEGER, b STRUCT(c VARCHAR))",
                Ty::object(vec![
                    Field::new("a", Ty::Number),
                    Field::new("b", Ty::object(vec![Field::new("c", Ty::String)])),
                ]),
            ),
            ("ll", "INTEGER[][]", Ty::list(Ty::list(Ty::Number))),
            ("x", "VARCHAR", Ty::String),
            ("n", "DOUBLE", Ty::Number),
        ],
        vec![
            json!({"id": 1, "tags": ["a", "b"], "nums": [1, 2, 3], "items": [{"sku": "A", "price": 5}, {"sku": "B", "price": 7}], "m": {"k": "v"}, "s": {"a": 1, "b": {"c": "x"}}, "ll": [[1, 2], [3]], "x": "a", "n": 1}),
            json!({"id": 2, "tags": [], "nums": [], "items": [], "m": {}, "s": {"a": null, "b": null}, "ll": [], "x": "z", "n": 2}),
            json!({"id": 3, "tags": null, "nums": null, "items": null, "m": null, "s": null, "ll": null, "x": null, "n": null}),
            json!({"id": 4, "tags": [null, "a"], "nums": [null, 5], "items": [null, {"sku": "C", "price": 1}], "m": {"k": null}, "s": {"a": 2, "b": {"c": null}}, "ll": [null, [1]], "x": "b", "n": 5}),
            json!({"id": 5, "tags": [null], "nums": [null], "items": [{"sku": null, "price": null}], "m": {"j": "w"}, "s": {"a": 3, "b": {"c": "y"}}, "ll": [[]], "x": "q", "n": 0}),
            json!({"id": 6, "tags": ["vip", "b"], "nums": [10, -1], "items": [{"sku": "A", "price": 12}, {"sku": "D", "price": 0}], "m": {"k": "V"}, "s": {"a": 4, "b": {"c": "x"}}, "ll": [[5]], "x": "vip", "n": -1}),
        ],
    )
}

#[test]
#[ignore = "exploration dump"]
fn explore_lists() {
    let f = list_fixture();
    let m = report(
        "lists",
        &f,
        &[
            "'a' in tags",
            "'zz' in tags",
            "x in tags",
            "x not in tags",
            "null in tags",
            "contains(tags, 'a')",
            "contains(tags, x)",
            "5 in nums",
            "n in nums",
            "len(tags) == 2",
            "len(tags) > 0",
            "len(items) == 1",
            "tags[0] == 'a'",
            "tags[0] == null",
            "tags[1] == 'a'",
            "nums[0] > 0",
            "items[0].sku == 'A'",
            "items[0].price == null",
            "items[0] == null",
            "ll[0][1] == 2",
            "some(tags, # == 'a')",
            "all(tags, # == 'a')",
            "none(tags, # == 'a')",
            "one(tags, # == 'a')",
            "count(tags, # == 'a') == 1",
            "some(tags, # == null)",
            "all(tags, # != null)",
            "some(nums, # > 2)",
            "all(nums, # > 0)",
            "none(nums, # > 4)",
            "one(nums, # > 4)",
            "count(nums, # > 0) == 2",
            "some(items, #.price > 6)",
            "all(items, #.price > 0)",
            "none(items, #.sku == 'A')",
            "some(items, #.sku == null)",
            "some(items, # == null)",
            "some(items as it, it.sku == x)",
            "some(items as it, some(tags, # == it.sku))",
            "some(items as it, some(tags as t, t == lower(it.sku)))",
            "some(ll, some(#, # > 1))",
            "some(ll, len(#) == 0)",
            "all(ll, all(#, # > 0))",
            "len(filter(nums, # > 1)) == 2",
            "len(filter(items, #.price > 1)) == 1",
            "filter(items, #.price > 1)[0].sku == 'B'",
            "map(items, #.sku)[0] == 'A'",
            "'A' in map(items, #.sku)",
            "sum(map(items, #.price)) > 10",
            "sum(nums) == 6",
            "sum(nums) == 0",
            "sum(nums) == 5",
            "avg(nums) == 2",
            "avg(nums) == null",
            "min(nums) == 1",
            "max(nums) == 3",
            "max(nums) == 5",
            "median(nums) == 2",
            "median(nums) == 4.5",
            "min(nums) == -1",
            "m.k == 'v'",
            "m.k == null",
            "m['k'] != 'v'",
            "m.zz == null",
            "s.a == 1",
            "s.a == null",
            "s.b.c == 'x'",
            "s.b.c == null",
            "s.b == null",
            "s == null",
            "tags == null",
            "items == null",
            "count(items, true) == 2",
            "some(tags, true)",
            "all(tags, false)",
            "some(tags, #)",
            "count(filter(nums, # > 0), # > 1) == 2",
            "some(map(nums, # * 2), # > 5)",
            "len(map(nums, # + 1)) == 3",
            "some(filter(items, #.price > 1) as it, it.sku == 'B')",
        ],
    );
    assert!(m.is_empty());
}

#[test]
#[ignore = "exploration dump"]
fn explore_paths() {
    let f = fixture(
        &[
            ("id", "INTEGER", Ty::Number),
            ("input.amount", "DOUBLE", Ty::Number),
            ("a", "INTEGER", Ty::Number),
        ],
        vec![
            json!({"id": 1, "input.amount": 5, "a": 1}),
            json!({"id": 2, "input.amount": null, "a": null}),
        ],
    );
    let mut m = Vec::new();
    for (expr, expected) in [
        ("$root['input.amount'] == 5", vec![]),
        ("$root['input.amount'] == null", vec![1, 2]),
        ("$root.a == 1", vec![1]),
        ("$root['a'] == 1", vec![1]),
    ] {
        let c = zen_sql::filter(expr, &f.schema, &Options::default());
        let got = c.as_ref().map(|c| duck(&f.conn, c));
        let sql = c.as_ref().map(|c| c.sql.clone());
        let line = format!("{expr}: expected(zen nested ctx) {expected:?} got {got:?} sql {sql:?}");
        eprintln!("[paths] {line}");
        m.push(line);
    }
}

#[test]
#[ignore = "exploration dump"]
fn explore_sources() {
    let schema = Schema::new()
        .with(
            Source::new(
                "orders",
                vec![
                    Field::new("id", Ty::Number),
                    Field::new("customer", Ty::object(vec![Field::new("id", Ty::Number)])),
                    Field::new("total", Ty::Number),
                ],
            )
            .qualified("o"),
        )
        .with(
            Source::new(
                "customer",
                vec![Field::new("id", Ty::Number), Field::new("vip", Ty::Bool)],
            )
            .qualified("c"),
        )
        .with(Source::new("x", vec![Field::new("id", Ty::Number)]))
        .default_source("orders");
    for expr in [
        "id == 1",
        "customer.id == 1",
        "customer.vip",
        "orders.id == 1",
        "orders.total > 1",
        "x.id == 1",
        "$root.id == 1",
        "$root.customer.id == 1",
        "$root.orders.id == 1",
        "orders == null",
        "customer == null",
        "x == null",
        "some([1], # == id)",
    ] {
        let r = zen_sql::filter(expr, &schema, &Options::default());
        eprintln!("[sources] {expr} => {:?}", r.map(|c| c.sql));
    }
    let nodefault = Schema::new()
        .with(Source::new("a", vec![Field::new("id", Ty::Number)]))
        .with(Source::new("b", vec![Field::new("id", Ty::Number)]));
    for expr in ["a.id == b.id", "id == 1", "a == 1", "c.id == 1"] {
        let r = zen_sql::filter(expr, &nodefault, &Options::default());
        eprintln!("[sources-nodefault] {expr} => {:?}", r.map(|c| c.sql));
    }
}

#[test]
#[ignore = "exploration dump"]
fn explore_panics() {
    let f = list_fixture();
    let exprs = [
        "tags[9223372036854775807] == 'a'",
        "tags[9223372036854775806] == 'a'",
        "ll[0][9223372036854775807] == 1",
        "tags[-9223372036854775808] == 'a'",
        "tags[1e30] == 'a'",
        "tags[0.5] == 'a'",
        "tags['0'] == 'a'",
        "m[0] == 'a'",
        "s[0] == 1",
        "$root == null",
        "$ == 1",
        "# == 1",
        "$root[0] == 1",
        "$root.tags[0] == 'a'",
        "some(tags, $root.x == #)",
        "some(tags as t, t == x and some(tags as t, t == 'b'))",
        "some(tags as x, x == 'a')",
        "some(tags, some(tags, # == #))",
        "map(tags, #) == null",
        "filter(tags, true) == null",
        "some(nums as n, n == n)",
        "count(tags, # == 'a') > 0 ? true : false",
        "sum(map(items, #.sku)) > 1",
        "sum(ll) > 1",
        "len(m) == 1",
        "len(s) == 1",
        "contains(m, 'k')",
        "'k' in m",
        "some(m, # == 'v')",
        "items[0] == items[1]",
        "tags == tags",
        "s.b == s.b",
        "s.zz == 1",
        "tags['a'] == 1",
        "x[0] == 'a'",
        "n[0] == 1",
        "some(ll, # == null)",
        "some(items, #.price == null and #.sku == null)",
        "one(items, #)",
        "count(items, #.sku) == 1",
        "map(items, #) == null",
        "some(map(ll, len(#)), # > 0)",
    ];
    for expr in exprs {
        for d in Dialect::ALL {
            let r = std::panic::catch_unwind(|| zen_sql::filter(expr, &f.schema, &Options::new(d)));
            match r {
                Err(_) => eprintln!("[panic] PANIC {expr} on {d:?}"),
                Ok(Ok(c)) if d == Dialect::DuckDb => {
                    let got = duck(&f.conn, &c);
                    eprintln!(
                        "[panic] {expr} on {d:?} => {} {:?} zen {:?}",
                        c.sql,
                        got,
                        zen(&f.rows, expr)
                    );
                }
                Ok(Err(e)) if d == Dialect::DuckDb => {
                    eprintln!("[panic] {expr} on {d:?} => ERR {e}")
                }
                _ => {}
            }
        }
    }
}

fn pg_url() -> Option<String> {
    std::env::var("ZEN_SQL_POSTGRES")
        .ok()
        .filter(|u| !u.is_empty())
}

fn psql(sql: &str) -> Result<String, String> {
    let url = pg_url().ok_or("ZEN_SQL_POSTGRES is not set")?;
    let out = std::process::Command::new("psql")
        .arg(url)
        .args(["-X", "-A", "-t", "-q", "-v", "ON_ERROR_STOP=1", "-c", sql])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn pg_fixture(table: &str, cols: &[(&str, &str)], rows: &[Value]) {
    let defs: Vec<String> = cols.iter().map(|(n, t)| format!("\"{n}\" {t}")).collect();
    let mut sql = format!(
        "SET TIME ZONE 'UTC'; DROP TABLE IF EXISTS {table}; CREATE TABLE {table} ({});",
        defs.join(", ")
    );
    for row in rows {
        sql.push_str(&format!(
            "INSERT INTO {table} SELECT * FROM jsonb_populate_record(NULL::{table}, '{}'::jsonb);",
            row.to_string().replace('\'', "''")
        ));
    }
    psql(&sql).unwrap();
}

fn pg_ids(table: &str, c: &Compiled) -> Result<Vec<i64>, String> {
    let select = format!("SELECT id FROM {table} WHERE {} ORDER BY id", c.sql);
    let sql = if c.params.is_empty() {
        format!("SET TIME ZONE 'UTC'; {select}")
    } else {
        let types = vec!["text"; c.params.len()].join(", ");
        let args: Vec<String> = c
            .params
            .iter()
            .map(|p| match p {
                zen_sql::predicate::Scalar::Str(s) => format!("'{}'", s.replace('\'', "''")),
                other => panic!("{other:?}"),
            })
            .collect();
        format!(
            "SET TIME ZONE 'UTC'; PREPARE q({types}) AS {select}; EXECUTE q({});",
            args.join(", ")
        )
    };
    let out = psql(&sql).map_err(|e| format!("{e}\n{sql}"))?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.trim().parse().unwrap())
        .collect())
}

fn pg_mismatches(table: &str, schema: &Schema, rows: &[Value], exprs: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for expr in exprs {
        for source in [expr.to_string(), format!("not ({expr})")] {
            let expected = zen(rows, &source);
            for _once in 0..1 {
                let c = match zen_sql::filter(&source, schema, &Options::new(Dialect::Postgres)) {
                    Ok(c) => c,
                    Err(e) => {
                        {
                            out.push(format!("COMPILE-ERR {source}: {e}"));
                        }
                        break;
                    }
                };
                match pg_ids(table, &c) {
                    Ok(actual) if actual == expected => {}
                    Ok(actual) => {
                        let per_row: Vec<String> = rows
                            .iter()
                            .filter(|r| {
                                let id = r["id"].as_i64().unwrap();
                                expected.contains(&id) != actual.contains(&id)
                            })
                            .map(|r| format!("{}={}", r["id"], zen_raw(r, &source)))
                            .collect();
                        out.push(format!(
                            "MISMATCH {source}: zen {expected:?} sql {actual:?}\n    sql: {}\n    zen: {}",
                            c.sql,
                            per_row.join(" | ")
                        ));
                    }
                    Err(e) => out.push(format!("SQL-ERR {source}: {e}")),
                }
            }
        }
    }
    out
}

fn pg_rows() -> Vec<Value> {
    vec![
        json!({"id": 1, "tags": ["a", "b"], "nums": [1, 2, 3], "x": "a", "p": {"n": 7, "s": "x", "b": true, "l": [1, 2]}}),
        json!({"id": 2, "tags": [], "nums": [], "x": "z", "p": {"n": "7", "s": 5, "b": "true"}}),
        json!({"id": 3, "tags": null, "nums": null, "x": null, "p": null}),
        json!({"id": 4, "tags": [null, "a"], "nums": [null, 5], "x": "b", "p": {"n": 7.0, "s": "5", "b": 1}}),
        json!({"id": 5, "tags": [null], "nums": [null], "x": "q", "p": {"n": null, "a\"b": 1, "a,b": 2, "": 3, "a}b": 4, "a\\b": 5}}),
        json!({"id": 6, "tags": ["vip", "b"], "nums": [10, -1], "x": "vip", "p": {"n": "1e2", "b": "t"}}),
    ]
}

fn pg_schema() -> Schema {
    Schema::single(Source::new(
        "row",
        vec![
            Field::new("id", Ty::Number),
            Field::new("tags", Ty::list(Ty::String)),
            Field::new("nums", Ty::list(Ty::Number)),
            Field::new("x", Ty::String),
            Field::new("p", Ty::Json),
        ],
    ))
}

#[test]
#[ignore = "exploration dump"]
fn explore_postgres() {
    if psql("SELECT 1").is_err() {
        eprintln!("no postgres");
        return;
    }
    common::init();
    let table = format!("hunt_paths_{}", std::process::id());
    let rows = pg_rows();
    pg_fixture(
        &table,
        &[
            ("id", "integer"),
            ("tags", "text[]"),
            ("nums", "double precision[]"),
            ("x", "text"),
            ("p", "jsonb"),
        ],
        &rows,
    );
    let m = pg_mismatches(
        &table,
        &pg_schema(),
        &rows,
        &[
            "x in tags",
            "'zz' in tags",
            "contains(tags, 'zz')",
            "'a' in tags",
            "some(tags, # == 'a')",
            "some(tags, # == x)",
            "all(tags, # == 'a')",
            "none(tags, # == 'zz')",
            "count(tags, # == 'a') == 1",
            "len(tags) == 0",
            "tags[0] == 'a'",
            "tags[0] == null",
            "sum(nums) == 5",
            "avg(nums) == 5",
            "max(nums) == 5",
            "median(nums) == 5",
            "sum(nums) == 0",
            "len(filter(tags, # != null)) == 1",
            "p.n == 7",
            "p.s == '5'",
            "p.b == true",
            "p.b",
            "p.l[1] == 2",
            "p['a\"b'] == 1",
            "p['a,b'] == 2",
            "p[''] == 3",
            "p['a}b'] == 4",
            "p['a\\\\b'] == 5",
            "p.n == null",
            "p.missing == null",
            "string(p.n) == '7'",
        ],
    );
    let _ = psql(&format!("DROP TABLE IF EXISTS {table}"));
    for line in &m {
        eprintln!("[pg] {line}");
    }
    assert!(m.is_empty());
}

#[test]
#[ignore = "exploration dump"]
fn explore_short_circuit() {
    let f = fixture(
        &[
            ("id", "INTEGER", Ty::Number),
            ("nums", "DOUBLE[]", Ty::list(Ty::Number)),
            ("jl", "JSON[]", Ty::list(Ty::Json)),
        ],
        vec![
            json!({"id": 1, "nums": [1, null], "jl": [{"a": 1}, {"a": "1"}]}),
            json!({"id": 2, "nums": [null, 1], "jl": [{"a": 2}]}),
            json!({"id": 3, "nums": [5, null], "jl": [null]}),
            json!({"id": 4, "nums": [null, 5], "jl": []}),
        ],
    );
    let m = report(
        "sc",
        &f,
        &[
            "all(nums, # > 2)",
            "none(nums, # > 0)",
            "some(nums, # > 2)",
            "one(nums, # > 2)",
            "count(nums, # > 2) == 1",
            "some(jl, #.a == 1)",
            "all(jl, #.a == 1)",
            "some(jl, #.a == '1')",
            "some(jl, # == null)",
        ],
    );
    assert!(m.is_empty());
}

fn duck_vs_zen(f: &Fixture, expr: &str) -> (Vec<i64>, Vec<i64>, String) {
    let c = zen_sql::filter(expr, &f.schema, &Options::default()).unwrap();
    (zen(&f.rows, expr), duck(&f.conn, &c).unwrap(), c.sql)
}

fn assert_same(f: &Fixture, exprs: &[&str]) {
    let mut bad = Vec::new();
    for e in exprs {
        let (z, s, sql) = duck_vs_zen(f, e);
        if z != s {
            bad.push(format!("{e}: zen {z:?} sql {s:?}\n  {sql}"));
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

fn small_json() -> Fixture {
    fixture(
        &[("id", "INTEGER", Ty::Number), ("p", "JSON", Ty::Json)],
        vec![
            json!({"id": 1, "p": {"n": 7, "s": "x", "b": true}}),
            json!({"id": 2, "p": {"n": "7", "s": 5, "b": "true"}}),
            json!({"id": 3, "p": {"n": "1e2", "b": 1}}),
            json!({"id": 4, "p": {}}),
        ],
    )
}

#[test]
fn bug_json_scalar_types_are_conflated_duckdb() {
    let f = small_json();
    assert_same(
        &f,
        &[
            "p.n == 7",
            "not (p.n == 7)",
            "p.n > 5",
            "p.n == '7'",
            "p.s == '5'",
            "p.b",
            "p.b == true",
            "not (contains(p.s, 'x'))",
        ],
    );
}

#[test]
fn bug_json_scalar_types_are_conflated_postgres() {
    if pg_url().is_none() {
        eprintln!("ZEN_SQL_POSTGRES is not set; skipping");
        return;
    }
    if psql("SELECT 1").is_err() {
        return;
    }
    common::init();
    let f = small_json();
    let table = format!("hunt_json_{}", std::process::id());
    pg_fixture(&table, &[("id", "integer"), ("p", "jsonb")], &f.rows);
    let mut bad = Vec::new();
    for e in ["p.n == 7", "p.s == '5'", "p.b", "not (p.n == 7)"] {
        let c = zen_sql::filter(e, &f.schema, &Options::new(Dialect::Postgres)).unwrap();
        let s = pg_ids(&table, &c).unwrap();
        let z = zen(&f.rows, e);
        if z != s {
            bad.push(format!("{e}: zen {z:?} sql {s:?}\n  {}", c.sql));
        }
    }
    let _ = psql(&format!("DROP TABLE IF EXISTS {table}"));
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

#[test]
fn bug_number_of_missing_json_key_under_not() {
    let f = small_json();
    assert_same(
        &f,
        &["not (number(p.n) == 7)", "not (number(p.missing) == 7)"],
    );
}

#[test]
fn bug_json_odd_keys_duckdb() {
    let f = fixture(
        &[("id", "INTEGER", Ty::Number), ("p", "JSON", Ty::Json)],
        vec![
            json!({"id": 1, "p": {"": 3, "*": 10}}),
            json!({"id": 2, "p": {}}),
        ],
    );
    let e = zen_sql::filter("p[''] == 3", &f.schema, &Options::default()).unwrap_err();
    assert!(e.message.contains("cannot be used as a name"), "{e}");
    assert_same(&f, &["p['*'] == 10"]);
}

fn null_elems() -> Fixture {
    fixture(
        &[
            ("id", "INTEGER", Ty::Number),
            ("tags", "VARCHAR[]", Ty::list(Ty::String)),
            ("nums", "DOUBLE[]", Ty::list(Ty::Number)),
        ],
        vec![
            json!({"id": 1, "tags": ["a", "b"], "nums": [1, 2]}),
            json!({"id": 2, "tags": [null, "a"], "nums": [null, 5]}),
            json!({"id": 3, "tags": [], "nums": [1, null]}),
        ],
    )
}

#[test]
fn bug_null_in_list_duckdb() {
    let f = null_elems();
    assert_same(&f, &["null in tags", "not (null in tags)"]);
}

#[test]
fn bug_aggregates_skip_null_elements() {
    let f = null_elems();
    assert_same(
        &f,
        &[
            "sum(nums) == 5",
            "max(nums) == 5",
            "avg(nums) == 5",
            "median(nums) == 5",
            "min(nums) == 1",
        ],
    );
}

#[test]
fn bug_closure_null_elements_and_short_circuit() {
    let f = null_elems();
    assert_same(
        &f,
        &[
            "some(nums, # > 2)",
            "not (all(nums, # > 2))",
            "not (none(nums, # > 0))",
            "some(map(nums, # * 2), # > 5)",
        ],
    );
}

#[test]
fn bug_in_list_with_null_elements_postgres() {
    if pg_url().is_none() {
        eprintln!("ZEN_SQL_POSTGRES is not set; skipping");
        return;
    }
    if psql("SELECT 1").is_err() {
        return;
    }
    common::init();
    let f = null_elems();
    let table = format!("hunt_nulls_{}", std::process::id());
    pg_fixture(
        &table,
        &[
            ("id", "integer"),
            ("tags", "text[]"),
            ("nums", "double precision[]"),
        ],
        &f.rows,
    );
    let mut bad = Vec::new();
    for e in [
        "not ('zz' in tags)",
        "not (contains(tags, 'zz'))",
        "sum(nums) == 5",
    ] {
        let c = zen_sql::filter(e, &f.schema, &Options::new(Dialect::Postgres)).unwrap();
        let s = pg_ids(&table, &c).unwrap();
        let z = zen(&f.rows, e);
        if z != s {
            bad.push(format!("{e}: zen {z:?} sql {s:?}\n  {}", c.sql));
        }
    }
    let _ = psql(&format!("DROP TABLE IF EXISTS {table}"));
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

#[test]
fn bug_bracket_key_with_dot_hits_dotted_column() {
    let f = fixture(
        &[
            ("id", "INTEGER", Ty::Number),
            ("input.amount", "DOUBLE", Ty::Number),
        ],
        vec![json!({"id": 1, "input.amount": 5})],
    );
    let ctx = json!({"id": 1, "input": {"amount": 5}});
    let zen_dotted = evaluate_expression("input.amount == 5", ctx.clone().into()).unwrap();
    let zen_bracket = evaluate_expression("$root['input.amount'] == 5", ctx.into()).unwrap();
    assert_eq!(format!("{zen_dotted:?}"), "true");
    assert_eq!(format!("{zen_bracket:?}"), "false");
    match zen_sql::filter("$root['input.amount'] == 5", &f.schema, &Options::default()) {
        Ok(c) => assert_eq!(duck(&f.conn, &c).unwrap(), Vec::<i64>::new(), "{}", c.sql),
        Err(e) => assert!(e.message.contains("unknown field `input.amount`"), "{e}"),
    }
}

#[test]
fn bug_unqualified_sources_collapse() {
    let schema = Schema::new()
        .with(Source::new("a", vec![Field::new("id", Ty::Number)]))
        .with(Source::new("b", vec![Field::new("id", Ty::Number)]));
    let c = zen_sql::filter("a.id == b.id", &schema, &Options::default()).unwrap();
    assert_ne!(c.sql, "(\"id\" IS NOT DISTINCT FROM \"id\")");
}

#[test]
fn bug_index_overflow() {
    let f = null_elems();
    let r = std::panic::catch_unwind(|| {
        zen_sql::filter(
            "tags[9223372036854775807] == 'a'",
            &f.schema,
            &Options::default(),
        )
    });
    match r {
        Err(_) => panic!("compiler panicked"),
        Ok(Ok(c)) => assert!(
            !c.sql.contains("-9223372036854775808"),
            "index wrapped: {}",
            c.sql
        ),
        Ok(Err(_)) => {}
    }
}
