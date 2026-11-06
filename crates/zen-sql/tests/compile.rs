mod common;

use std::collections::HashMap;
use std::sync::Arc;

use arrow::datatypes::{DataType, Field as ArrowField, Fields, Schema as ArrowSchema, TimeUnit};
use chrono::TimeZone;
use zen_sql::predicate::{Op, Predicate, Scalar};
use zen_sql::{Dialect, Field, Options, Pushdown, Schema, Source, Ty};

fn duck(expr: &str) -> String {
    common::init();
    let c = zen_sql::filter(expr, &common::schema(), &Options::default())
        .unwrap_or_else(|e| panic!("{expr}: {e}"));
    common::shown(&c)
}

fn sql(dialect: Dialect, expr: &str) -> String {
    common::init();
    let c = zen_sql::filter(expr, &common::schema(), &Options::new(dialect))
        .unwrap_or_else(|e| panic!("{expr}: {e}"));
    common::shown(&c)
}

fn error(expr: &str) -> zen_sql::Error {
    common::init();
    match zen_sql::filter(expr, &common::schema(), &Options::default()) {
        Ok(c) => panic!("{expr} compiled to {}", c.sql),
        Err(e) => e,
    }
}

fn joined() -> Schema {
    Schema::single(Source::new(
        "decision",
        vec![
            Field::required("id", Ty::String),
            Field::new("status", Ty::String),
            Field::new("input", Ty::Json),
            Field::new("created", Ty::instant()),
        ],
    ))
    .with(
        Source::new(
            "customers",
            vec![
                Field::required("id", Ty::String),
                Field::new("tier", Ty::String),
            ],
        )
        .qualified("c"),
    )
}

#[test]
fn paths_resolve_across_sources() {
    let schema = joined();
    let c = zen_sql::filter(
        "customers.tier == 'gold' and status == 'ok' and decision.input.amount > 100",
        &schema,
        &Options::default(),
    )
    .unwrap();
    assert_eq!(
        common::shown(&c),
        r#"((("c"."tier" = 'gold') AND ("decision"."status" = 'ok')) AND ((CASE WHEN json_type("decision"."input", '/amount') IN ('BIGINT', 'UBIGINT', 'DOUBLE', 'HUGEINT') THEN CAST(json_extract_string("decision"."input", '/amount') AS DOUBLE) END) > 100))"#
    );
    assert_eq!(
        c.pushdown,
        vec![
            Pushdown {
                source: "customers".into(),
                predicate: Predicate::new("tier", Op::Eq, Scalar::Str("gold".into())),
            },
            Pushdown {
                source: "decision".into(),
                predicate: Predicate::new("status", Op::Eq, Scalar::Str("ok".into())),
            },
        ]
    );
}

#[test]
fn unknown_and_ambiguous_paths_are_errors() {
    let schema = joined();
    let e = zen_sql::filter("custmers.tier == 'gold'", &schema, &Options::default()).unwrap_err();
    assert!(
        e.message.contains("unknown field `custmers` in `decision`"),
        "{e}"
    );
    assert!(
        e.message.contains("available: created, id, input, status"),
        "{e}"
    );
    assert!(e.span.is_some());

    let e = zen_sql::filter("customers.segment == 'x'", &schema, &Options::default()).unwrap_err();
    assert!(
        e.message.contains("unknown field `segment` in `customers`"),
        "{e}"
    );

    let clash = Schema::single(Source::new(
        "decision",
        vec![Field::new("customers", Ty::String)],
    ))
    .with(Source::new(
        "customers",
        vec![Field::new("tier", Ty::String)],
    ));
    let e = zen_sql::filter("customers.tier == 'gold'", &clash, &Options::default()).unwrap_err();
    assert!(e.message.contains("is both a source and a field"), "{e}");

    let no_default = Schema::new().with(Source::new("a", vec![Field::new("x", Ty::Number)]));
    let e = zen_sql::filter("b.x > 1", &no_default, &Options::default()).unwrap_err();
    assert!(
        e.message.contains("unknown source `b`; available: a"),
        "{e}"
    );
    assert_eq!(
        common::shown(&zen_sql::filter("a.x > 1", &no_default, &Options::default()).unwrap()),
        r#"("x" > 1)"#
    );
}

#[test]
fn nested_dotted_and_root_paths() {
    assert_eq!(duck("input.amount > 1"), r#"("input.amount" > 1)"#);
    assert_eq!(duck("$root.amount > 1"), r#"("amount" > 1)"#);
    assert_eq!(duck("row.amount > 1"), r#"("amount" > 1)"#);
    assert_eq!(
        duck("customer['address'].city == 'Paris'"),
        r#"(struct_extract(struct_extract("customer", 'address'), 'city') = 'Paris')"#
    );
    assert_eq!(
        duck("items[2].sku == 'A'"),
        r#"(struct_extract(list_extract("items", 3), 'sku') = 'A')"#
    );
    assert_eq!(
        duck("payload['odd key'].x == 'y'"),
        r#"((CASE WHEN json_type("payload", '/odd key/x') IN ('VARCHAR') THEN json_extract_string("payload", '/odd key/x') END) = 'y')"#
    );
}

#[test]
fn unsupported_expressions_are_errors() {
    let cases = [
        ("name < 'b'", "cannot order strings"),
        ("rand() > 0.5", "rand() is not supported"),
        (
            "len(flatMap(items, [#.sku])) > 0",
            "flatMap() is not supported",
        ),
        (
            "d(created).format('%Y') == '2024'",
            "format() is not supported",
        ),
        ("keys(attrs) == 1", "keys() is not supported"),
        ("date(created_text) > 0", "date() is deprecated"),
        ("x = 1; x > 0", "not supported"),
        ("$ > 1", "`$` is only available in unary expressions"),
        ("payload[tier] == 1", "dynamic property access"),
        ("tags[-1] == 'a'", "negative indexes"),
        (
            "customer.zip == '1'",
            "unknown field `zip`; available: address, age, tier",
        ),
        ("unknown == 1", "unknown field `unknown` in `row`"),
        ("payload.a + payload.b > 1", "cannot tell whether `+`"),
        ("d(created, 'Europe/Paris') > d('2024-01-01')", "timezone"),
        (
            "d(created).diff(d('2024-01-01'), 'month') > 1",
            "diff() in months",
        ),
        (
            "amount",
            "a filter must be a boolean expression, got a number",
        ),
        (
            "tags == ['a']",
            "arrays can only be used on the right of `in`",
        ),
        ("amount >", ""),
        ("flag and 1", "expected a boolean, got a number"),
        ("payload.a == payload.b", "types of both sides are unknown"),
        ("'x' in name", "use contains()"),
    ];
    for (expr, needle) in cases {
        let e = error(expr);
        assert!(e.message.contains(needle), "{expr}: {e}");
    }
}

#[test]
fn dialect_errors_name_the_dialect() {
    common::init();
    let schema = common::schema();
    let cases = [
        (
            Dialect::BigQuery,
            "attrs.color == 'red'",
            "reading map keys is not supported on BigQuery",
        ),
        (
            Dialect::Redshift,
            "some(tags, # == 'a')",
            "closures over arrays is not supported on Redshift",
        ),
        (
            Dialect::Redshift,
            "'a' in tags",
            "searching arrays is not supported on Redshift",
        ),
        (
            Dialect::Redshift,
            "payload.list[0] == 1",
            "indexing into JSON arrays is not supported on Redshift",
        ),
        (
            Dialect::Snowflake,
            "median(scores) > 1",
            "median() over arrays is not supported on Snowflake",
        ),
    ];
    for (d, expr, needle) in cases {
        let e = zen_sql::filter(expr, &schema, &Options::new(d)).unwrap_err();
        assert_eq!(e.message, needle, "{expr}");
    }
}

#[test]
fn every_filter_compiles_or_fails_cleanly_on_every_dialect() {
    common::init();
    let schema = common::schema();
    for d in Dialect::ALL {
        {
            for expr in common::FILTERS {
                for source in [expr.to_string(), format!("not ({expr})")] {
                    if let Err(e) = zen_sql::filter(&source, &schema, &Options::new(d)) {
                        assert!(
                            e.message
                                .ends_with(&format!("is not supported on {}", d.name())),
                            "{}: {source}: {e}",
                            d.name()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn snapshots_per_dialect() {
    let expr = "tier in ['gold', 'platinum'] and startsWith(name, 'A') and some(tags, # == 'vip')";
    assert_eq!(
        sql(Dialect::DuckDb, expr),
        r#"((("tier" IN ('gold', 'platinum')) AND starts_with("name", 'A')) AND (len(list_filter("tags", lambda _z0: (_z0 = 'vip'))) > 0))"#
    );
    assert_eq!(
        sql(Dialect::Postgres, expr),
        r#"((("tier" IN ('gold', 'platinum')) AND starts_with("name", 'A')) AND (CASE WHEN "tags" IS NULL THEN NULL ELSE EXISTS (SELECT 1 FROM unnest("tags") AS _z0 WHERE (_z0 = 'vip')) END))"#
    );
    assert_eq!(
        sql(Dialect::BigQuery, expr),
        "(((`tier` IN ('gold', 'platinum')) AND STARTS_WITH(`name`, 'A')) AND (CASE WHEN `tags` IS NULL THEN NULL ELSE EXISTS (SELECT 1 FROM UNNEST(`tags`) AS _z0 WHERE (_z0 = 'vip')) END))"
    );
    assert_eq!(
        sql(Dialect::Snowflake, expr),
        r#"((("tier" IN ('gold', 'platinum')) AND STARTSWITH("name", 'A')) AND (ARRAY_SIZE(FILTER("tags", _z0 -> (_z0::VARCHAR = 'vip'))) > 0))"#
    );
    assert_eq!(
        sql(Dialect::Databricks, expr),
        "(((`tier` IN ('gold', 'platinum')) AND startswith(`name`, 'A')) AND exists(`tags`, _z0 -> (_z0 = 'vip')))"
    );

    let dates = "d(created).year() == 2024 and d(created) >= d('2024-03-01')";
    assert_eq!(
        sql(Dialect::DuckDb, dates),
        r#"((EXTRACT(year FROM CAST("created" AS TIMESTAMP)) = 2024) AND COALESCE(("created" >= TIMESTAMPTZ '2024-03-01 00:00:00.000000+00:00'), FALSE))"#
    );
    assert_eq!(
        sql(Dialect::BigQuery, dates),
        "((EXTRACT(YEAR FROM DATETIME(`created`, 'UTC')) = 2024) AND COALESCE((`created` >= TIMESTAMP '2024-03-01 00:00:00.000000+00:00'), FALSE))"
    );
    assert_eq!(
        sql(Dialect::Snowflake, dates),
        r#"((DATE_PART(year, CONVERT_TIMEZONE('UTC', "created")::TIMESTAMP_NTZ) = 2024) AND COALESCE(("created" >= TO_TIMESTAMP_TZ('2024-03-01 00:00:00.000000+00:00')), FALSE))"#
    );
    assert_eq!(
        sql(Dialect::Databricks, dates),
        "((extract(YEAR FROM from_utc_timestamp(`created`, 'UTC')) = 2024) AND COALESCE((`created` >= TIMESTAMP '2024-03-01 00:00:00.000000+00:00'), FALSE))"
    );
    assert_eq!(
        sql(Dialect::Redshift, dates),
        r#"((DATE_PART(year, CONVERT_TIMEZONE('UTC', "created")) = 2024) AND COALESCE(("created" >= CAST('2024-03-01 00:00:00.000000+00:00' AS TIMESTAMPTZ)), FALSE))"#
    );

    let json = "payload.score > 5";
    assert_eq!(
        sql(Dialect::BigQuery, json),
        r#"((CASE WHEN JSON_TYPE(JSON_QUERY(`payload`, '$."score"')) IN ('number') THEN CAST(JSON_VALUE(`payload`, '$."score"') AS FLOAT64) END) > 5)"#
    );
    assert_eq!(
        sql(Dialect::Snowflake, json),
        r#"((CASE WHEN TYPEOF(GET_PATH("payload", '"score"')) IN ('INTEGER', 'DECIMAL', 'DOUBLE') THEN CAST(GET_PATH("payload", '"score"')::VARCHAR AS DOUBLE PRECISION) END) > 5)"#
    );
    assert_eq!(
        sql(Dialect::Databricks, json),
        r#"((CASE WHEN (get_json_object(`payload`, '$[\'score\']') RLIKE '^\\s*[+-]?([0-9][0-9_]*(\\.[0-9_]*)?|\\.[0-9][0-9_]*)([eE][+-]?[0-9]+)?\\s*$') THEN CAST(REPLACE(regexp_replace(get_json_object(`payload`, '$[\'score\']'), '^\\s+|\\s+$', ''), '_', '') AS DOUBLE) END) > 5)"#
    );
    assert_eq!(
        sql(Dialect::Redshift, json),
        r#"((CASE WHEN (json_extract_path_text("payload", 'score') ~ '^[[:space:]]*[+-]?([0-9][0-9_]*(\\.[0-9_]*)?|\\.[0-9][0-9_]*)([eE][+-]?[0-9]+)?[[:space:]]*$') THEN CAST(REPLACE(REGEXP_REPLACE(json_extract_path_text("payload", 'score'), '^[[:space:]]+|[[:space:]]+$', ''), '_', '') AS DOUBLE PRECISION) END) > 5)"#
    );
}

#[test]
fn user_strings_are_always_parameters() {
    common::init();
    let schema = common::schema();
    let hostile = r#"a\b' OR 1=1 -- ); DROP TABLE t; /* $$ "#;
    for d in Dialect::ALL {
        let c =
            zen_sql::filter(&format!("name == `{hostile}`"), &schema, &Options::new(d)).unwrap();
        assert!(!c.sql.contains("OR 1=1"), "{}: {}", d.name(), c.sql);
        assert!(!c.sql.contains('\\'), "{}: {}", d.name(), c.sql);
        assert_eq!(
            c.params,
            vec![Scalar::Str(hostile.to_string())],
            "{}",
            d.name()
        );
    }
}

#[test]
fn unsafe_names_are_rejected() {
    for name in [
        "we\"ird",
        "back`tick",
        "it's",
        "back\\slash",
        "new\nline",
        "",
    ] {
        let schema = Schema::single(Source::new("t", vec![Field::new(name, Ty::String)]));
        let q = ['"', '\'', '`']
            .into_iter()
            .find(|q| !name.contains(*q))
            .unwrap();
        let expr = format!("$root[{q}{name}{q}] == 'x'");
        for d in Dialect::ALL {
            let e = zen_sql::filter(&expr, &schema, &Options::new(d)).unwrap_err();
            assert!(
                e.message.contains("cannot be used as a name")
                    || e.message.contains("unknown field"),
                "{}: {name:?}: {e}",
                d.name()
            );
        }
    }
    let schema = common::schema();
    for key in ["a'b", "a\"b", "a`b", "a\\b"] {
        for expr in [
            format!(
                "payload[{q}{key}{q}] == 'x'",
                q = ['"', '\'', '`']
                    .into_iter()
                    .find(|q| !key.contains(*q))
                    .unwrap()
            ),
            format!(
                "attrs[{q}{key}{q}] == 'x'",
                q = ['"', '\'', '`']
                    .into_iter()
                    .find(|q| !key.contains(*q))
                    .unwrap()
            ),
        ] {
            let e = zen_sql::filter(&expr, &schema, &Options::default()).unwrap_err();
            assert!(
                e.message.contains("cannot be used as a name"),
                "{expr}: {e}"
            );
        }
    }
    let ok = Schema::single(Source::new(
        "t",
        vec![Field::new("odd name-1é", Ty::String)],
    ));
    let c = zen_sql::filter("$root['odd name-1é'] == 'x'", &ok, &Options::default()).unwrap();
    assert_eq!(common::shown(&c), r#"("odd name-1é" = 'x')"#);
}

#[test]
fn injection_attempts_stay_inside_parameters() {
    common::init();
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t (name VARCHAR); INSERT INTO t VALUES ('x'), ('y'' OR 1=1 --')",
    )
    .unwrap();
    let schema = Schema::single(Source::new("t", vec![Field::new("name", Ty::String)]));
    for payload in [
        "x' OR 1=1 --",
        "y' OR 1=1 --",
        "x\\' OR TRUE --",
        "'); DROP TABLE t; --",
    ] {
        let expr = format!("name == \"{payload}\"");
        let c = zen_sql::filter(&expr, &schema, &Options::default()).unwrap();
        let params: Vec<String> = c
            .params
            .iter()
            .map(|p| match p {
                Scalar::Str(s) => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        let n: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM t WHERE {}", c.sql),
                duckdb::params_from_iter(params.iter()),
                |r| r.get(0),
            )
            .unwrap();
        let expected = i64::from(payload == "y' OR 1=1 --");
        assert_eq!(n, expected, "{payload}: {}", c.sql);
    }
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 2);
}

#[test]
fn bind_mode_uses_dialect_markers() {
    common::init();
    let schema = common::schema();
    let expr = "tier == 'a' or name == 'b' or name == ''";
    let expect = [
        (
            Dialect::DuckDb,
            r#"((("tier" = ?) OR ("name" = ?)) OR ("name" = ''))"#,
        ),
        (
            Dialect::Snowflake,
            r#"((("tier" = ?) OR ("name" = ?)) OR ("name" = ''))"#,
        ),
        (
            Dialect::Postgres,
            r#"((("tier" = $1) OR ("name" = $2)) OR ("name" = ''))"#,
        ),
        (
            Dialect::BigQuery,
            "(((`tier` = @p0) OR (`name` = @p1)) OR (`name` = ''))",
        ),
        (
            Dialect::Databricks,
            "(((`tier` = :p0) OR (`name` = :p1)) OR (`name` = ''))",
        ),
        (
            Dialect::Redshift,
            r#"((("tier" = :p0) OR ("name" = :p1)) OR ("name" = ''))"#,
        ),
    ];
    for (d, want) in expect {
        let c = zen_sql::filter(expr, &schema, &Options::new(d)).unwrap();
        assert_eq!(c.sql, want, "{}", d.name());
        assert_eq!(
            c.params,
            vec![Scalar::Str("a".into()), Scalar::Str("b".into())]
        );
    }
}

#[test]
fn pushdown_extracts_top_level_conjuncts_only() {
    common::init();
    let c = zen_sql::filter(
        "amount > 100 and tier in ['a', 'b'] and startsWith(name, 'x') and flag \
         and d(created) >= d('2024-01-01') and qty != 5 and (qty == 1 or qty == 2) \
         and amount <= 10.5 and name != null and customer.tier == 'gold' and 3 < qty \
         and d(day) == d('2024-03-05') and qty in [1..5)",
        &common::schema(),
        &Options::default(),
    )
    .unwrap();
    let got: Vec<Predicate> = c.pushdown.into_iter().map(|p| p.predicate).collect();
    let jan = chrono::Utc
        .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
        .unwrap()
        .timestamp_micros();
    assert_eq!(
        got,
        vec![
            Predicate::new("amount", Op::Gt, Scalar::Int(100)),
            Predicate::is_in(
                "tier",
                vec![Scalar::Str("a".into()), Scalar::Str("b".into())]
            ),
            Predicate::new("name", Op::Prefix, Scalar::Str("x".into())),
            Predicate::new("flag", Op::Eq, Scalar::Bool(true)),
            Predicate::new("created", Op::Ge, Scalar::Timestamp(jan)),
            Predicate::new("amount", Op::Le, Scalar::Float(10.5)),
            Predicate::unary("name", Op::IsNotNull),
            Predicate::new("qty", Op::Gt, Scalar::Int(3)),
            Predicate::new("day", Op::Eq, Scalar::Date(19787)),
            Predicate::new("qty", Op::Ge, Scalar::Int(1)),
            Predicate::new("qty", Op::Lt, Scalar::Int(5)),
        ]
    );
    let none = zen_sql::filter(
        "not (amount > 1) or tier == 'a'",
        &common::schema(),
        &Options::default(),
    )
    .unwrap();
    assert!(none.pushdown.is_empty());
}

#[test]
fn timezone_shapes_literals_and_getters() {
    common::init();
    let tz: chrono_tz::Tz = "Europe/Ljubljana".parse().unwrap();
    let opts = Options::new(Dialect::DuckDb).timezone(tz);
    let schema = common::schema();
    let f = |e: &str| common::shown(&zen_sql::filter(e, &schema, &opts).unwrap());
    assert_eq!(
        f("d(created) > d('2024-03-05 09:00')"),
        r#"COALESCE(("created" > TIMESTAMPTZ '2024-03-05 08:00:00.000000+00:00'), FALSE)"#
    );
    assert_eq!(
        f("d(local) > d('2024-03-05 09:00')"),
        r#"COALESCE(("local" > TIMESTAMP '2024-03-05 09:00:00.000000'), FALSE)"#
    );
    assert_eq!(
        f("d(day) == d('2024-07-01')"),
        r#"("day" = DATE '2024-07-01')"#
    );
    assert_eq!(
        f("d(created).hour() == 10"),
        r#"(EXTRACT(hour FROM timezone('Europe/Ljubljana', "created")) = 10)"#
    );
    let pg = zen_sql::filter(
        "d(created).hour() == 10",
        &schema,
        &Options::new(Dialect::Postgres).timezone(tz),
    )
    .unwrap();
    assert_eq!(
        common::shown(&pg),
        r#"(EXTRACT(hour FROM ("created" AT TIME ZONE 'Europe/Ljubljana')) = 10)"#
    );
}

#[test]
fn unary_subject_must_be_a_path() {
    common::init();
    let schema = common::schema();
    let c = zen_sql::unary(
        "'gold', 'silver'",
        "customer.tier",
        &schema,
        &Options::default(),
    )
    .unwrap();
    assert_eq!(
        common::shown(&c),
        r#"((struct_extract("customer", 'tier') = 'gold') OR (struct_extract("customer", 'tier') = 'silver'))"#
    );
    let json = zen_sql::unary("> 5", "payload.score", &schema, &Options::default()).unwrap();
    assert_eq!(
        common::shown(&json),
        r#"((CASE WHEN json_type("payload", '/score') IN ('BIGINT', 'UBIGINT', 'DOUBLE', 'HUGEINT') THEN CAST(json_extract_string("payload", '/score') AS DOUBLE) END) > 5)"#
    );
    let e = zen_sql::unary("> 5", "amount + 1", &schema, &Options::default()).unwrap_err();
    assert!(e.message.contains("must be a field path"), "{e}");
}

#[test]
fn values_have_types() {
    common::init();
    let schema = common::schema();
    let v = zen_sql::value("upper(name)", &schema, &Options::default()).unwrap();
    assert_eq!(v.ty, Ty::String);
    assert_eq!(common::shown(&v), r#"UPPER("name")"#);
    let n = zen_sql::value("payload.kind", &schema, &Options::default()).unwrap();
    assert_eq!(n.ty, Ty::String);
    assert_eq!(
        common::shown(&n),
        r#"(CASE WHEN json_type("payload", '/kind') IN ('VARCHAR', 'BIGINT', 'UBIGINT', 'DOUBLE', 'HUGEINT', 'BOOLEAN') THEN json_extract_string("payload", '/kind') WHEN json_type("payload", '/kind') IS NULL OR json_type("payload", '/kind') = 'NULL' THEN 'null' END)"#
    );
    let l = zen_sql::value("map(items, #.price)", &schema, &Options::default()).unwrap();
    assert_eq!(l.ty, Ty::list(Ty::Number));
}

#[test]
fn arrow_schemas_map_to_types() {
    let mut json_meta = HashMap::new();
    json_meta.insert("ARROW:extension:name".to_string(), "arrow.json".to_string());
    let map_entries = ArrowField::new(
        "entries",
        DataType::Struct(Fields::from(vec![
            ArrowField::new("key", DataType::Utf8, false),
            ArrowField::new("value", DataType::Int64, true),
        ])),
        false,
    );
    let arrow = ArrowSchema::new(vec![
        ArrowField::new("id", DataType::Int64, false),
        ArrowField::new("price", DataType::Decimal128(10, 2), true),
        ArrowField::new(
            "created",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            true,
        ),
        ArrowField::new(
            "local",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            true,
        ),
        ArrowField::new("day", DataType::Date32, true),
        ArrowField::new(
            "tags",
            DataType::List(Arc::new(ArrowField::new("item", DataType::Utf8, true))),
            true,
        ),
        ArrowField::new(
            "address",
            DataType::Struct(Fields::from(vec![ArrowField::new(
                "city",
                DataType::Utf8View,
                true,
            )])),
            true,
        ),
        ArrowField::new("counts", DataType::Map(Arc::new(map_entries), false), true),
        ArrowField::new("payload", DataType::Utf8, true).with_metadata(json_meta),
        ArrowField::new("blob", DataType::Binary, true),
    ]);
    let source = Source::from_arrow("t", &arrow);
    let ty = |n: &str| source.field(n).map(|f| f.ty.clone());
    assert_eq!(ty("id"), Some(Ty::Number));
    assert!(!source.field("id").unwrap().nullable);
    assert_eq!(ty("price"), Some(Ty::Number));
    assert_eq!(ty("created"), Some(Ty::instant()));
    assert_eq!(ty("local"), Some(Ty::local()));
    assert_eq!(ty("day"), Some(Ty::date()));
    assert_eq!(ty("tags"), Some(Ty::list(Ty::String)));
    assert_eq!(
        ty("address"),
        Some(Ty::object(vec![Field::new("city", Ty::String)]))
    );
    assert_eq!(ty("counts"), Some(Ty::map(Ty::Number)));
    assert_eq!(ty("payload"), Some(Ty::Json));
    assert_eq!(ty("blob"), None);

    let schema = Schema::single(source);
    let c = zen_sql::filter(
        "address.city == 'x' and counts.a > 1 and payload.k == 'v' and price > 9.99",
        &schema,
        &Options::default(),
    )
    .unwrap();
    assert_eq!(
        common::shown(&c),
        r#"((((struct_extract("address", 'city') = 'x') AND ("counts"['a'] > 1)) AND ((CASE WHEN json_type("payload", '/k') IN ('VARCHAR') THEN json_extract_string("payload", '/k') END) = 'v')) AND ("price" > 9.99))"#
    );
}

#[test]
fn schema_round_trips_through_json() {
    let schema = joined();
    let text = serde_json::to_string(&schema).unwrap();
    let back: Schema = serde_json::from_str(&text).unwrap();
    assert_eq!(back, schema);
}

#[test]
fn path_pushdown_reaches_into_json_and_structs() {
    common::init();
    let c = zen_sql::filter(
        "payload.order.total > 100 and payload.country == 'SI' and customer.tier in ['a', 'b'] \
         and payload.vip and payload.note != null and amount >= 5 \
         and (payload.a == 1 or payload.b == 2) and payload.items[0].sku == 'x' \
         and payload['odd.key'] == 'y' and contains(payload.name, 'z')",
        &common::schema(),
        &Options::default(),
    )
    .unwrap();
    let got: Vec<(Vec<String>, Predicate)> =
        c.paths.into_iter().map(|p| (p.path, p.predicate)).collect();
    let path = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut gt = Predicate::unary("payload", Op::Gt);
    gt.values = vec![Scalar::Int(100)];
    let mut eq = Predicate::unary("payload", Op::Eq);
    eq.values = vec![Scalar::Str("SI".into())];
    let mut tier = Predicate::unary("customer", Op::In);
    tier.values = vec![Scalar::Str("a".into()), Scalar::Str("b".into())];
    let mut vip = Predicate::unary("payload", Op::Eq);
    vip.values = vec![Scalar::Bool(true)];
    assert_eq!(
        got,
        vec![
            (path(&["payload", "order", "total"]), gt),
            (path(&["payload", "country"]), eq),
            (path(&["customer", "tier"]), tier),
            (path(&["payload", "vip"]), vip),
            (
                path(&["payload", "note"]),
                Predicate::unary("payload", Op::IsNotNull)
            ),
            (
                path(&["amount"]),
                Predicate::new("amount", Op::Ge, Scalar::Int(5))
            ),
        ]
    );
    assert_eq!(
        c.pushdown
            .iter()
            .map(|p| p.predicate.column.as_str())
            .collect::<Vec<_>>(),
        vec!["amount"]
    );
}
