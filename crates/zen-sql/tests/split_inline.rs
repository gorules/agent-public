use duckdb::Connection;
use serde_json::{Value, json};
use zen_expression::{Variable, evaluate_expression};
use zen_sql::{Dialect, Field, Options, Schema, Source, Ty, filter, filter_parts};

fn rows() -> Vec<Value> {
    vec![
        json!({"id": 1, "s": "ivan", "n": 10}),
        json!({"id": 2, "s": "it's", "n": 0}),
        json!({"id": 3, "s": null, "n": null}),
        json!({"id": 4, "s": "", "n": -3}),
        json!({"id": 5, "s": "back\\slash", "n": 7}),
        json!({"id": 6, "s": "日本 é", "n": 2}),
    ]
}

fn schema() -> Schema {
    Schema::single(Source::new(
        "t",
        vec![
            Field::required("id", Ty::Number),
            Field::new("s", Ty::String),
            Field::new("n", Ty::Number),
        ],
    ))
}

fn duck() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t (id INTEGER, s VARCHAR, n DOUBLE)")
        .unwrap();
    let mut insert = conn.prepare("INSERT INTO t VALUES (?, ?, ?)").unwrap();
    for r in rows() {
        insert
            .execute(duckdb::params![
                r["id"].as_i64(),
                r["s"].as_str(),
                r["n"].as_f64()
            ])
            .unwrap();
    }
    conn
}

fn params(c: &zen_sql::Compiled) -> Vec<String> {
    c.params
        .iter()
        .map(|p| match p {
            zen_sql::predicate::Scalar::Str(s) => s.clone(),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn zen_ids(expr: &str) -> Vec<i64> {
    rows()
        .iter()
        .filter(|r| {
            matches!(
                evaluate_expression(expr, (*r).clone().into()),
                Ok(Variable::Bool(true))
            )
        })
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

fn duck_ids(conn: &Connection, c: &zen_sql::Compiled) -> Vec<i64> {
    let mut st = conn
        .prepare(&format!("SELECT id FROM t WHERE {} ORDER BY id", c.sql))
        .unwrap();
    st.query_map(duckdb::params_from_iter(params(c).iter()), |r| {
        r.get::<_, i64>(0)
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn opts() -> Options {
    Options::new(Dialect::DuckDb)
}

#[test]
fn and_chains_split_into_sql_and_a_zen_remainder() {
    let parts = filter_parts("n > 1 and rand(10) >= 0 and s != ''", &schema(), &opts()).unwrap();
    assert_eq!(parts.residual, vec!["rand(10) >= 0".to_string()]);
    assert!(parts.compiled.is_some());

    let parts = filter_parts("n > 1 and s != ''", &schema(), &opts()).unwrap();
    assert!(parts.residual.is_empty());
    assert!(parts.residual_source().is_none());

    let parts = filter_parts("n > 1 or len(flatMap([[n]], #)) == 1", &schema(), &opts()).unwrap();
    assert!(parts.compiled.is_none());
    assert_eq!(
        parts.residual,
        vec!["n > 1 or len(flatMap([[n]], #)) == 1".to_string()]
    );
}

#[test]
fn split_parts_pass_the_same_rows_as_zen() {
    let conn = duck();
    for e in [
        "n > 1 and count(flatMap([[n]], #), # < 9) == 1",
        "(id == 1 or id == 5) and len(flatMap([[s]], #)) == 1",
        "count(flatMap([[n]], #), # >= 0) == 1 and s != null",
    ] {
        let parts = filter_parts(e, &schema(), &opts()).unwrap();
        let residual = parts.residual_source().expect("a residual");
        let sql_ids = match &parts.compiled {
            Some(c) => duck_ids(&conn, c),
            None => rows().iter().map(|r| r["id"].as_i64().unwrap()).collect(),
        };
        let both: Vec<i64> = sql_ids
            .into_iter()
            .filter(|id| zen_ids(&residual).contains(id))
            .collect();
        assert_eq!(
            both,
            zen_ids(e),
            "{e}: compiled {:?} residual {residual}",
            parts.compiled.map(|c| c.sql)
        );
    }
}

#[test]
fn inline_literals_are_quoted_and_bind_nothing() {
    let conn = duck();
    for (expr, quoted) in [
        ("s == \"x' OR 1=1 --\"", "'x'' OR 1=1 --'"),
        ("s == \"it's\"", "'it''s'"),
        ("s == 'back\\\\slash'", "'back\\\\slash'"),
        ("s == '日本 é'", "'日本 é'"),
    ] {
        let c = filter(expr, &schema(), &opts().inline()).unwrap();
        assert!(c.params.is_empty(), "{expr}: {:?}", c.params);
        assert!(c.sql.contains(quoted), "{expr}: {}", c.sql);
        assert_eq!(duck_ids(&conn, &c), zen_ids(expr), "{expr}: {}", c.sql);
    }
    assert!(filter("s == 'a\u{0}b'", &schema(), &opts().inline()).is_err());
    let c = filter("s == 'ivan'", &schema(), &opts()).unwrap();
    assert_eq!(params(&c), vec!["ivan".to_string()]);
}
