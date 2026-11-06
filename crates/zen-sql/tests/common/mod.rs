#![allow(dead_code)]

use std::sync::Once;

use serde_json::{Value, json};
use zen_expression::{evaluate_expression, evaluate_unary_expression};
use zen_sql::{Field, Schema, Source, Ty};

pub static INIT: Once = Once::new();

pub fn init() {
    INIT.call_once(|| unsafe { std::env::set_var("TZ", "UTC") });
}

pub fn rows() -> Vec<Value> {
    vec![
        json!({
            "id": 1, "amount": 10.5, "qty": 1, "name": "Alice", "tier": "gold", "flag": true,
            "created": "2024-03-05T10:00:00Z", "created_text": "2024-03-05T10:00:00Z", "created_ms": 1709632800000i64,
            "day": "2024-03-05", "local": "2024-03-05 10:00:00",
            "tags": ["a", "b"], "scores": [1.0, 2.0, 3.0],
            "items": [{"sku": "A", "price": 5.5}, {"sku": "B", "price": 7.0}],
            "customer": {"tier": "gold", "age": 34, "address": {"city": "Paris"}},
            "attrs": {"color": "red", "size": "L"},
            "payload": {"score": 7, "kind": "x", "nested": {"ok": true}, "list": [1, 2]},
            "input.amount": 3.0
        }),
        json!({
            "id": 2, "amount": 250.0, "qty": 5, "name": "bob", "tier": "silver", "flag": false,
            "created": "2024-12-31T23:30:15Z", "created_text": "2024-12-31T23:30:15Z", "created_ms": 1735687815000i64,
            "day": "2024-12-31", "local": "2024-12-31 23:30:00",
            "tags": [], "scores": [],
            "items": [{"sku": "C", "price": 1.0}],
            "customer": {"tier": "silver", "age": 17, "address": {"city": "Berlin"}},
            "attrs": {"color": "blue"},
            "payload": {"score": "n/a", "kind": "y", "nested": {"ok": false}, "list": []},
            "input.amount": 0.5
        }),
        json!({
            "id": 3, "amount": 1000.0, "qty": null, "name": "O'Brien", "tier": "platinum", "flag": null,
            "created": "2025-01-01T00:00:00Z", "created_text": "2025-01-01T00:00:00Z", "created_ms": 1735689600000i64,
            "day": "2025-01-01", "local": "2025-01-01 00:00:00",
            "tags": ["vip"], "scores": [10.0],
            "items": [],
            "customer": {"tier": null, "age": null, "address": null},
            "attrs": {},
            "payload": {"score": 12.5, "nested": {}},
            "input.amount": null
        }),
        json!({
            "id": 4, "amount": null, "qty": 0, "name": null, "tier": null, "flag": true,
            "created": null, "created_text": null, "created_ms": null,
            "day": null, "local": null,
            "tags": null, "scores": null,
            "items": null,
            "customer": null,
            "attrs": null,
            "payload": null,
            "input.amount": 2.0
        }),
        json!({
            "id": 5, "amount": -3.7, "qty": 12, "name": "héllo", "tier": "gold", "flag": false,
            "created": "2024-06-15T12:45:00Z", "created_text": "2024-06-15T12:45:00Z", "created_ms": 1718455500000i64,
            "day": "2024-06-15", "local": "2024-06-15 12:45:00",
            "tags": ["b", "c", "vip"], "scores": [0.5, 9.5],
            "items": [{"sku": "A", "price": 12.0}, {"sku": "D", "price": 0.0}, {"sku": "E", "price": 3.0}],
            "customer": {"tier": "gold", "age": 61, "address": {"city": "Paris"}},
            "attrs": {"size": "M"},
            "payload": {"score": 3, "kind": "x", "list": [5, 2, 9]},
            "input.amount": 10.0
        }),
        json!({
            "id": 6, "amount": 0.0, "qty": 3, "name": "back\\slash", "tier": "bronze", "flag": true,
            "created": "2024-03-05T23:59:59Z", "created_text": "2024-03-05 23:59:59", "created_ms": 1709683199000i64,
            "day": "2024-03-04", "local": "2024-03-05 00:00:00",
            "tags": ["a"], "scores": [2.0, 2.0],
            "items": [{"sku": "B", "price": null}],
            "customer": {"tier": "bronze", "age": 18, "address": {"city": null}},
            "attrs": {"color": "red"},
            "payload": {"score": 5, "kind": null},
            "input.amount": 1.0
        }),
        json!({
            "id": 7, "amount": 99.99, "qty": 2, "name": "", "tier": "Gold", "flag": null,
            "created": "2023-11-11T11:11:11Z", "created_text": "not a date", "created_ms": 0,
            "day": "2023-11-11", "local": "2023-11-11 11:11:11",
            "tags": ["A", "vip"], "scores": [100.0, -1.0],
            "items": [{"sku": "Z", "price": 99.0}],
            "customer": {"tier": "Gold", "age": 45, "address": {"city": "Ljubljana"}},
            "attrs": {"color": "Red"},
            "payload": {"score": 7.0, "kind": "X", "nested": {"ok": null}},
            "input.amount": 7.0
        }),
    ]
}

pub fn schema() -> Schema {
    let item = Ty::object(vec![
        Field::new("sku", Ty::String),
        Field::new("price", Ty::Number),
    ]);
    let customer = Ty::object(vec![
        Field::new("tier", Ty::String),
        Field::new("age", Ty::Number),
        Field::new("address", Ty::object(vec![Field::new("city", Ty::String)])),
    ]);
    Schema::single(Source::new(
        "row",
        vec![
            Field::required("id", Ty::Number),
            Field::new("amount", Ty::Number),
            Field::new("qty", Ty::Number),
            Field::new("name", Ty::String),
            Field::new("tier", Ty::String),
            Field::new("flag", Ty::Bool),
            Field::new("created", Ty::instant()),
            Field::new("created_text", Ty::String),
            Field::new("created_ms", Ty::Number),
            Field::new("day", Ty::date()),
            Field::new("local", Ty::local()),
            Field::new("tags", Ty::list(Ty::String)),
            Field::new("scores", Ty::list(Ty::Number)),
            Field::new("items", Ty::list(item)),
            Field::new("customer", customer),
            Field::new("attrs", Ty::map(Ty::String)),
            Field::new("payload", Ty::Json),
            Field::new("input.amount", Ty::Number),
        ],
    ))
}

pub fn context(row: &Value) -> Value {
    let mut ctx = row.clone();
    let obj = ctx.as_object_mut().unwrap();
    let dotted = obj.remove("input.amount").unwrap();
    obj.insert("input".into(), json!({ "amount": dotted }));
    ctx
}

pub fn zen_ids(expr: &str) -> Vec<i64> {
    rows()
        .iter()
        .filter(|row| {
            let ctx = context(row);
            matches!(
                evaluate_expression(expr, ctx.into()),
                Ok(zen_expression::Variable::Bool(true))
            )
        })
        .map(|row| row["id"].as_i64().unwrap())
        .collect()
}

pub fn zen_unary_ids(expr: &str, subject: &str) -> Vec<i64> {
    rows()
        .iter()
        .filter(|row| {
            let mut ctx = context(row);
            let value = ctx[subject].clone();
            ctx.as_object_mut().unwrap().insert("$".into(), value);
            matches!(evaluate_unary_expression(expr, ctx.into()), Ok(true))
        })
        .map(|row| row["id"].as_i64().unwrap())
        .collect()
}

pub const FILTERS: &[&str] = &[
    "amount > 100",
    "amount >= 10.5 and qty < 10",
    "amount == null",
    "amount != null",
    "amount != 250",
    "amount == 250 or qty == 0",
    "qty == 5",
    "qty != 5",
    "qty > 3",
    "amount in [10.5, 250]",
    "amount not in [10.5, 250]",
    "qty in [1..5]",
    "qty in (1..5)",
    "qty in [1..5)",
    "qty not in [1..5]",
    "qty in [1, null]",
    "tier in ['gold', 'platinum']",
    "tier not in ['gold']",
    "tier in ['gold', 5]",
    "tier == 'gold' or flag",
    "flag",
    "flag == false",
    "flag != true",
    "flag and qty > 0",
    "name == \"O'Brien\"",
    "name == 'back\\slash'",
    "name == ''",
    "startsWith(name, 'A')",
    "endsWith(name, 'ob')",
    "contains(name, 'li')",
    "lower(name) == 'bob'",
    "upper(tier) == 'GOLD'",
    "len(name) > 4",
    "len(name) == 6",
    "matches(name, '^[A-Z]')",
    "trim(name) == name",
    "name + '!' == 'bob!'",
    "`${tier}-${qty}` == 'gold-1'",
    "amount * 2 > 300",
    "amount / qty > 2",
    "qty % 2 == 1",
    "-amount < 0",
    "abs(amount) > 5",
    "round(amount) == 11",
    "round(amount, 1) == 10.5",
    "floor(amount) == 99",
    "ceil(amount) == 100",
    "trunc(amount) == -3",
    "amount ^ 2 > 100",
    "(amount ?? 0) > 5",
    "(tier ?? 'none') == 'none'",
    "qty > 3 ? tier == 'gold' : true",
    "string(qty) == '5'",
    "string(flag) == 'true'",
    "number(created_text) == null",
    "isNumeric(name)",
    "bool(flag)",
    "1 + 1 == 2",
    "amount > 2 * 50",
    "customer.tier == 'gold'",
    "customer.age >= 18",
    "customer.address.city == 'Paris'",
    "customer.address.city != 'Paris'",
    "$root.amount > 100",
    "input.amount > 1",
    "attrs.color == 'red'",
    "attrs['size'] == 'L'",
    "tags[0] == 'a'",
    "tags[5] == null",
    "items[0].price > 5",
    "items[1].sku == 'B'",
    "'vip' in tags",
    "'vip' not in tags",
    "contains(tags, 'a')",
    "some(tags, # == 'b')",
    "all(scores, # > 0)",
    "none(scores, # > 5)",
    "one(items, #.price > 5)",
    "count(items, #.price > 1) == 2",
    "some(items as it, it.sku == 'A' and it.price < 10)",
    "len(tags) == 0",
    "len(filter(scores, # > 1)) == 2",
    "sum(scores) > 5",
    "avg(scores) == 2",
    "max(scores) == 10",
    "min(scores) == 1",
    "median(scores) == 2",
    "sum(map(items, #.price)) > 10",
    "some(map(items, #.sku), # == 'A')",
    "payload.score > 5",
    "payload.kind == 'x'",
    "payload.nested.ok == true",
    "payload.list[1] == 2",
    "number(payload.score) > 5",
    "payload.kind == null",
    "payload.missing == null",
    "d(created) > d('2024-06-01')",
    "d(created) < '2025-01-01'",
    "d(created) == d('2024-03-05T10:00:00Z')",
    "d(created).year() == 2024",
    "d(created).month() == 3",
    "d(created).hour() == 10",
    "d(created).weekday() == 2",
    "d(created).week() == 10",
    "d(created).dayOfYear() > 60",
    "d(created).quarter() == 1",
    "d(created).day() == 5",
    "d(created).minute() == 30",
    "d(created).second() == 15",
    "d(created).isBefore(d('2024-12-31'))",
    "d(created).isAfter('2024-12-31')",
    "d(created).isSame(d('2024-03-05'), 'day')",
    "d(created).isSame(d('2024-12-01'), 'month')",
    "d(created).isSameOrAfter('2025-01-01')",
    "d(created).isSameOrBefore('2024-03-05 10:00')",
    "d(created) < d('2024-06-01')",
    "d(created) <= d(created_text)",
    "d(created_text) >= d(created)",
    "d(created) > d(local)",
    "d(created).add(1, 'day') > d('2024-03-06')",
    "d(created).sub(1, 'month') < d('2024-02-06')",
    "d(created).add('2h').hour() == 12",
    "d(created).add(1, 'year').year() == 2025",
    "d(created).startOf('month') == d('2024-03-01')",
    "d(created).startOf('week') == d('2024-03-04')",
    "d(created).startOf('year') == d('2024-01-01')",
    "d(created).diff(d('2024-01-01'), 'day') == 64",
    "d(created).diff(d('2024-03-05'), 'hour') > 5",
    "d(created).timestamp() > 1710000000000",
    "d(created) in [d('2024-01-01')..d('2024-12-31')]",
    "d(created) in [d('2024-03-05'), d('2025-01-01')]",
    "d(created).isValid()",
    "d(created) == null",
    "d(day) == d('2024-03-05')",
    "d(day) > d('2024-03-04 12:00')",
    "d(day) < '2024-06-01'",
    "d(day).year() == 2024",
    "d(day).weekday() == 2",
    "d(day).add(1, 'month') == d('2024-04-05')",
    "d(day).timestamp() == 1709596800000",
    "d(local) > d('2024-03-05 09:00')",
    "d(local).hour() == 10",
    "d(local).timestamp() == 1709632800000",
    "d(created_text) > d('2024-06-01')",
    "d(created_ms) > d('2024-06-01')",
    "d(created_ms).year() == 2024",
    "d(created) > d('2024-03-05') and d(created) < d('2024-03-06')",
];

fn quote(d: zen_sql::Dialect, text: &str) -> String {
    use zen_sql::Dialect as D;
    let backslash = matches!(d, D::BigQuery | D::Databricks | D::Snowflake | D::Redshift);
    if d == D::Postgres && text.contains('\\') {
        return format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"));
    }
    let mut out = String::from("'");
    for c in text.chars() {
        match c {
            '\'' if matches!(d, D::BigQuery | D::Databricks) => out.push_str("\\'"),
            '\'' => out.push_str("''"),
            '\\' if backslash => out.push_str("\\\\"),
            '\n' if backslash => out.push_str("\\n"),
            '\r' if backslash => out.push_str("\\r"),
            '\t' if backslash => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

pub fn shown(c: &zen_sql::Compiled) -> String {
    use zen_sql::Dialect as D;
    let d = c.dialect;
    let value = |i: usize| match &c.params[i] {
        zen_sql::predicate::Scalar::Str(s) => quote(d, s),
        other => panic!("unexpected param {other:?}"),
    };
    let chars: Vec<char> = c.sql.chars().collect();
    let backslash = matches!(d, D::BigQuery | D::Databricks | D::Snowflake | D::Redshift);
    let mut out = String::new();
    let mut next = 0;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' {
            let escaped = backslash || (i > 0 && chars[i - 1] == 'E');
            out.push(ch);
            i += 1;
            while i < chars.len() {
                let c2 = chars[i];
                out.push(c2);
                i += 1;
                if escaped && c2 == '\\' && i < chars.len() {
                    out.push(chars[i]);
                    i += 1;
                } else if c2 == '\'' {
                    if i < chars.len() && chars[i] == '\'' {
                        out.push('\'');
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            continue;
        }
        let digits = |from: usize| {
            let mut j = from;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            j
        };
        match (d, ch) {
            (D::DuckDb | D::Snowflake, '?') => {
                out.push_str(&value(next));
                next += 1;
                i += 1;
            }
            (D::Postgres, '$') if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() => {
                let j = digits(i + 1);
                let n: usize = chars[i + 1..j].iter().collect::<String>().parse().unwrap();
                out.push_str(&value(n - 1));
                i = j;
            }
            (D::BigQuery, '@') if chars.get(i + 1) == Some(&'p') => {
                let j = digits(i + 2);
                let n: usize = chars[i + 2..j].iter().collect::<String>().parse().unwrap();
                out.push_str(&value(n));
                i = j;
            }
            (D::Databricks | D::Redshift, ':')
                if chars.get(i + 1) == Some(&'p')
                    && chars.get(i + 2).is_some_and(|c| c.is_ascii_digit())
                    && (i == 0 || chars[i - 1] != ':') =>
            {
                let j = digits(i + 2);
                let n: usize = chars[i + 2..j].iter().collect::<String>().parse().unwrap();
                out.push_str(&value(n));
                i = j;
            }
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}
