mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use zen_sql::{Dialect, Limits, Options};

fn compile(expr: &str, options: &Options) -> Result<zen_sql::Compiled, zen_sql::Error> {
    common::init();
    zen_sql::filter(expr, &common::schema(), options)
}

fn question_marks(sql: &str) -> usize {
    let mut count = 0;
    let mut quoted = false;
    for c in sql.chars() {
        match c {
            '\'' => quoted = !quoted,
            '?' if !quoted => count += 1,
            _ => {}
        }
    }
    count
}

fn chain(n: usize, term: &str, op: &str) -> String {
    vec![term; n].join(op)
}

#[test]
fn binding_is_the_default() {
    for d in Dialect::ALL {
        let c = compile("name == \"x' OR 1=1 --\"", &Options::new(d)).unwrap();
        assert!(!c.sql.contains("OR 1=1"), "{}: {}", d.name(), c.sql);
        assert_eq!(c.params.len(), 1, "{}", d.name());
    }
}

#[test]
fn numbered_markers_reuse_repeated_values() {
    let c = compile(
        "name == 'a' or tier == 'a' or name == 'b'",
        &Options::new(Dialect::Postgres),
    )
    .unwrap();
    assert_eq!(c.sql.matches("$1").count(), 2, "{}", c.sql);
    assert_eq!(c.params.len(), 2);
}

#[test]
fn markers_always_match_params() {
    let exprs = [
        "d(name + 'x').add(1, 'month') > d('2024-01-01')",
        "d(name + 'x').add(1, 'year') > d('2024-01-01') and tier == 'gold'",
        "some(tags, d(# + 'x').add(1, 'month') > d('2024-01-01'))",
        "number(name + 'x') > 1 and trim(tier + 'y') == 'z'",
    ];
    for d in [Dialect::DuckDb, Dialect::Snowflake] {
        for expr in exprs {
            let c = compile(expr, &Options::new(d)).unwrap();
            assert_eq!(
                question_marks(&c.sql),
                c.params.len(),
                "{}: {expr}\n{}",
                d.name(),
                c.sql
            );
        }
    }
}

#[test]
fn hostile_values_only_ever_reach_parameters() {
    let hostile = [
        "x' OR 1=1 --",
        "\\' OR 1=1) --",
        "'); DROP TABLE t; --",
        "$$ OR TRUE $$",
        "*/ OR 1=1 /*",
        "\\",
        "@p0 :p0 $1 ?",
        "é ' \" ` \\ \n \r \t",
    ];
    for d in Dialect::ALL {
        for value in hostile {
            for template in [
                "name == `{}`",
                "startsWith(name, `{}`)",
                "tier in [`{}`, 'gold']",
                "lower(name) == `{}` or contains(name, `{}`)",
                "`${name}{}` == 'x'",
            ] {
                let expr = template.replace("{}", value);
                let Ok(c) = compile(&expr, &Options::new(d)) else {
                    continue;
                };
                assert!(!c.sql.contains(value), "{}: {expr}\n{}", d.name(), c.sql);
                assert!(
                    c.params.iter().any(
                        |p| matches!(p, zen_sql::predicate::Scalar::Str(s) if s.contains(value))
                    ),
                    "{}: {expr}: {:?}",
                    d.name(),
                    c.params
                );
            }
        }
    }
}

fn markers(d: Dialect, sql: &str) -> Vec<usize> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut quoted = false;
    let mut i = 0;
    let mut next = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            quoted = !quoted;
            i += 1;
            continue;
        }
        if quoted {
            if c == '\\' {
                i += 1;
            }
            i += 1;
            continue;
        }
        let number = |from: usize| {
            let mut j = from;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            (
                chars[from..j]
                    .iter()
                    .collect::<String>()
                    .parse::<usize>()
                    .ok(),
                j,
            )
        };
        match (d, c) {
            (Dialect::DuckDb | Dialect::Snowflake, '?') => {
                out.push(next);
                next += 1;
                i += 1;
            }
            (Dialect::Postgres, '$') => match number(i + 1) {
                (Some(n), j) => {
                    out.push(n - 1);
                    i = j;
                }
                _ => i += 1,
            },
            (Dialect::BigQuery, '@') if chars.get(i + 1) == Some(&'p') => match number(i + 2) {
                (Some(n), j) => {
                    out.push(n);
                    i = j;
                }
                _ => i += 1,
            },
            (Dialect::Databricks | Dialect::Redshift, ':')
                if chars.get(i + 1) == Some(&'p') && (i == 0 || chars[i - 1] != ':') =>
            {
                match number(i + 2) {
                    (Some(n), j) => {
                        out.push(n);
                        i = j;
                    }
                    _ => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    out
}

#[test]
fn markers_cover_exactly_the_params_on_every_dialect() {
    common::init();
    let schema = common::schema();
    for d in Dialect::ALL {
        for expr in common::FILTERS {
            for source in [expr.to_string(), format!("not ({expr})")] {
                let Ok(c) = zen_sql::filter(&source, &schema, &Options::new(d)) else {
                    continue;
                };
                let used = markers(d, &c.sql);
                let mut distinct: Vec<usize> = used.clone();
                distinct.sort_unstable();
                distinct.dedup();
                let all: Vec<usize> = (0..c.params.len()).collect();
                assert_eq!(distinct, all, "{}: {source}\n{}", d.name(), c.sql);
                if matches!(d, Dialect::DuckDb | Dialect::Snowflake) {
                    assert_eq!(used.len(), c.params.len(), "{}: {source}", d.name());
                }
            }
        }
    }
}

#[test]
fn postgres_parameters_survive_nonstandard_strings() {
    let Some(url) = std::env::var("ZEN_SQL_POSTGRES")
        .ok()
        .filter(|u| !u.is_empty())
    else {
        eprintln!("ZEN_SQL_POSTGRES is not set; skipping");
        return;
    };
    let table = format!("zen_sql_sec_{}", std::process::id());
    let victim = format!("{table}_victim");
    let run = |sql: &str| -> Result<String, String> {
        let out = Command::new("psql")
            .arg(&url)
            .env("PGOPTIONS", "-c standard_conforming_strings=off")
            .args(["-X", "-A", "-t", "-q", "-c", sql])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    run(&format!(
        "DROP TABLE IF EXISTS {table}; DROP TABLE IF EXISTS {victim}; CREATE TABLE {table} (id int, name text); \
         INSERT INTO {table} VALUES (1, 'a'), (2, 'b'); CREATE TABLE {victim} (x int);"
    ))
    .unwrap();
    let schema = zen_sql::Schema::single(zen_sql::Source::new(
        "t",
        vec![zen_sql::Field::new("name", zen_sql::Ty::String)],
    ));
    let payloads = [
        "\\' OR 1=1) --".to_string(),
        "\\' OR (SELECT rolsuper FROM pg_roles WHERE rolname = current_user)) --".to_string(),
        format!("\\'); DROP TABLE {victim}; --"),
        "\\\\' OR TRUE --".to_string(),
    ];
    for payload in &payloads {
        let c = zen_sql::filter(
            &format!("name == `{payload}`"),
            &schema,
            &Options::new(Dialect::Postgres),
        )
        .unwrap();
        assert_eq!(c.sql, r#"("name" = $1)"#);
        let zen_sql::predicate::Scalar::Str(value) = &c.params[0] else {
            panic!("{:?}", c.params)
        };
        let quoted = value.replace('\\', "\\\\").replace('\'', "''");
        let rows = run(&format!(
            "PREPARE q(text) AS SELECT id FROM {table} WHERE {}; EXECUTE q(E'{quoted}');",
            c.sql
        ))
        .unwrap();
        assert_eq!(rows.trim(), "", "{payload}");
    }
    let alive = run(&format!("SELECT to_regclass('{victim}') IS NOT NULL")).unwrap();
    assert_eq!(alive.trim(), "t");
    let _ = run(&format!(
        "DROP TABLE IF EXISTS {table}; DROP TABLE IF EXISTS {victim}"
    ));
}

#[test]
fn nul_characters_are_rejected() {
    for expr in [
        "name == 'a\0b'",
        "payload['a\0b'] == 1",
        "`x\0${name}` == 'x'",
    ] {
        let e = compile(expr, &Options::default()).unwrap_err();
        assert!(e.message.contains("NUL"), "{expr}: {e}");
    }
}

#[test]
fn long_expressions_are_rejected() {
    let expr = format!("name == '{}'", "x".repeat(10_000));
    let e = compile(&expr, &Options::default()).unwrap_err();
    assert!(e.message.contains("longer than"), "{e}");
}

#[test]
fn deep_nesting_is_rejected_without_overflowing_a_small_stack() {
    let cases = [
        chain(1000, "amount", " + ") + " > 0",
        chain(1000, "amount > 0", " and "),
        format!("{}flag", "not ".repeat(5000)),
        format!("{}amount", "-".repeat(5000)),
        format!("{}1 > 0", "(".repeat(2000)) + &")".repeat(2000),
    ];
    let limits = Limits {
        source: 64 * 1024,
        ..Limits::default()
    };
    for expr in cases {
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || compile(&expr, &Options::default().limits(limits)).map(|_| ()))
            .unwrap();
        let e = handle.join().expect("compiler thread crashed").unwrap_err();
        assert!(e.message.contains("nested deeper"), "{e}");
    }
}

#[test]
fn closures_nest_at_most_three_deep() {
    let ok = "some(tags, some(tags, some(tags, # == 'a')))";
    assert!(compile(ok, &Options::default()).is_ok());
    let deep = "some(tags, some(tags, some(tags, some(tags, # == 'a'))))";
    let e = compile(deep, &Options::default()).unwrap_err();
    assert!(e.message.contains("closures"), "{e}");
}

#[test]
fn sql_grows_linearly_for_chains() {
    for d in Dialect::ALL {
        for (expr, n) in [
            (chain(100, "amount > 0", " or "), 100),
            (chain(100, "flag", " or "), 100),
            (chain(60, "flag ? flag : flag", " == "), 60),
            (chain(100, "contains(name, 'x')", " or "), 100),
        ] {
            let started = Instant::now();
            match compile(&expr, &Options::new(d)) {
                Ok(c) => assert!(
                    c.sql.len() < 400 * n,
                    "{}: {} bytes for {n} terms",
                    d.name(),
                    c.sql.len()
                ),
                Err(e) => assert!(
                    e.message.contains("not supported") || e.message.contains("exceed"),
                    "{}: {e}",
                    d.name()
                ),
            }
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "{}: {expr}",
                d.name()
            );
        }
    }
}

#[test]
fn oversized_sql_fails_fast() {
    let nested = (0..40).fold("name".to_string(), |acc, _| {
        format!("trim(number({acc}) + '')")
    });
    let started = Instant::now();
    let r = compile(&format!("{nested} == 'x'"), &Options::default());
    assert!(started.elapsed() < Duration::from_secs(2));
    if let Err(e) = r {
        assert!(
            e.message.contains("exceed") || e.message.contains("expected"),
            "{e}"
        );
    }
    let limits = Limits {
        sql: 200,
        ..Limits::default()
    };
    let e = compile(
        &chain(20, "amount > 0", " or "),
        &Options::default().limits(limits),
    )
    .unwrap_err();
    assert!(e.message.contains("exceed"), "{e}");
    let limits = Limits {
        params: 3,
        ..Limits::default()
    };
    let many = (0..5)
        .map(|i| format!("name == 'v{i}'"))
        .collect::<Vec<_>>()
        .join(" or ");
    let e = compile(&many, &Options::default().limits(limits)).unwrap_err();
    assert!(e.message.contains("parameters"), "{e}");
}

#[test]
fn powers_need_constant_exponents_and_stay_small() {
    let e = compile("amount ^ amount > 1", &Options::default()).unwrap_err();
    assert!(e.message.contains("constant"), "{e}");
    let nested = (0..12).fold("amount".to_string(), |acc, _| format!("({acc}) ^ 0.5"));
    let c = compile(&format!("{nested} > 1"), &Options::default()).unwrap();
    assert!(c.sql.len() < 1_000_000, "{} bytes", c.sql.len());
}

#[test]
fn constant_folding_skips_expensive_functions() {
    let big = "a".repeat(3000);
    let expr = format!("fuzzyMatch('{big}', '{big}') > 0");
    let started = Instant::now();
    let e = compile(&expr, &Options::default()).unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(e.message.contains("not supported"), "{e}");
}

#[test]
fn date_amounts_are_bounded() {
    for expr in [
        "d(created).add(9223372036854775807, 'week') > d('2024-01-01')",
        "d(created).sub(-9223372036854775808, 'second') > d('2024-01-01')",
        "d(created).add('9223372036854775807 weeks') > d('2024-01-01')",
    ] {
        assert!(compile(expr, &Options::default()).is_err(), "{expr}");
    }
    assert!(
        compile(
            "d(created).add(1000, 'year') > d('2024-01-01')",
            &Options::default()
        )
        .is_ok()
    );
}

#[test]
fn patterns_are_validated() {
    let cases = [
        (Dialect::DuckDb, "matches(name, name)", "constant pattern"),
        (
            Dialect::Postgres,
            "matches(name, '^(.*)(.*)\\2\\1$')",
            "invalid pattern",
        ),
        (
            Dialect::Postgres,
            "matches(name, '(?=a)b')",
            "invalid pattern",
        ),
        (
            Dialect::Postgres,
            "matches(name, '(a+)+$')",
            "very long time",
        ),
        (
            Dialect::Databricks,
            "matches(name, '(a|ab)*c')",
            "very long time",
        ),
        (
            Dialect::Snowflake,
            "matches(name, '(x*)*')",
            "very long time",
        ),
    ];
    for (d, expr, needle) in cases {
        let e = compile(expr, &Options::new(d)).unwrap_err();
        assert!(e.message.contains(needle), "{}: {expr}: {e}", d.name());
    }
    let long = format!("matches(name, '{}')", "a".repeat(300));
    assert!(compile(&long, &Options::default()).is_err());
    assert!(compile("matches(name, '(a+)+$')", &Options::new(Dialect::DuckDb)).is_ok());
    assert!(
        compile(
            "matches(name, '^[A-Z][a-z]+ [0-9]{3}$')",
            &Options::new(Dialect::Postgres)
        )
        .is_ok()
    );
}
