use crate::predicate::{Op, Predicate, Scalar};
use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;

use crate::ir::{Base, Cmp, Expr, Func, Path, Scalar as Read, Step};
use crate::schema::DateKind;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pushdown {
    pub source: String,
    pub predicate: Predicate,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PathPushdown {
    pub source: String,
    pub path: Vec<String>,
    pub predicate: Predicate,
}

pub fn extract(e: &Expr, timezone: Tz) -> Vec<Pushdown> {
    let mut out = Collected::default();
    conjuncts(e, timezone, Reach::Column, &mut out);
    out.found
        .into_iter()
        .map(|p| Pushdown {
            source: p.source,
            predicate: p.predicate,
        })
        .collect()
}

pub fn extract_paths(e: &Expr, timezone: Tz) -> (Vec<PathPushdown>, bool) {
    let mut out = Collected::default();
    conjuncts(e, timezone, Reach::Nested, &mut out);
    (out.found, !out.missed)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    Column,
    Nested,
}

#[derive(Default)]
struct Collected {
    found: Vec<PathPushdown>,
    missed: bool,
}

fn conjuncts(e: &Expr, tz: Tz, reach: Reach, out: &mut Collected) {
    match e {
        Expr::And(a, b, _) => {
            conjuncts(a, tz, reach, out);
            conjuncts(b, tz, reach, out);
        }
        Expr::BoolOf(inner) => conjuncts(inner, tz, reach, out),
        Expr::Case(_, then, otherwise) if reach == Reach::Nested => {
            match (then.as_ref(), otherwise.as_ref()) {
                (Expr::Null, inner) | (inner, Expr::Null) => conjuncts(inner, tz, reach, out),
                _ => out.missed = true,
            }
        }
        other => match simple(other, tz, reach) {
            Some(found) => out.found.extend(found),
            None => out.missed = true,
        },
    }
}

struct Subject {
    source: String,
    path: Vec<String>,
}

fn steps(p: &Path) -> Option<Subject> {
    let Base::Column { source, name, .. } = &p.base else {
        return None;
    };
    if name.contains(['.', '[', ']']) {
        return None;
    }
    let mut path = vec![name.clone()];
    for (step, _) in &p.steps {
        match step {
            Step::Field(f) | Step::Key(f) if !f.is_empty() && !f.contains(['.', '[', ']']) => {
                path.push(f.clone())
            }
            _ => return None,
        }
    }
    Some(Subject {
        source: source.clone(),
        path,
    })
}

fn subject(e: &Expr, reach: Reach) -> Option<Subject> {
    match (e, reach) {
        (Expr::Path(p), Reach::Column) => p.is_plain_column().map(|(source, name)| Subject {
            source: source.to_string(),
            path: vec![name.to_string()],
        }),
        (Expr::Path(p), Reach::Nested) => steps(p),
        (
            Expr::Open {
                path,
                as_: Read::Str | Read::Num | Read::Bool,
            },
            Reach::Nested,
        ) => steps(path),
        _ => None,
    }
}

fn scalar(e: &Expr, tz: Tz, exact: bool) -> Option<Scalar> {
    Some(match e {
        Expr::Bool(b) => Scalar::Bool(*b),
        Expr::Str(s) => Scalar::Str(s.clone()),
        Expr::Num(n) => {
            let limit = Decimal::from(i32::MAX);
            if n.abs() > limit {
                return None;
            }
            if n.fract().is_zero() && !n.is_sign_negative() {
                Scalar::Int(n.to_i64()?)
            } else if n.fract().is_zero() {
                Scalar::Float(n.to_f64()?)
            } else if exact {
                return None;
            } else {
                let f = n.to_f64()?;
                if Decimal::from_f64_retain(f)? != *n {
                    return None;
                }
                Scalar::Float(f)
            }
        }
        Expr::Time { micros, kind } => {
            let instant = DateTime::<Utc>::from_timestamp_micros(*micros)?;
            match kind {
                DateKind::Instant => Scalar::Timestamp(*micros),
                DateKind::Local => Scalar::Timestamp(
                    instant
                        .with_timezone(&tz)
                        .naive_local()
                        .and_utc()
                        .timestamp_micros(),
                ),
                DateKind::Date => {
                    let wall = instant.with_timezone(&tz).naive_local();
                    if wall.time() != NaiveTime::MIN {
                        return None;
                    }
                    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?;
                    Scalar::Date((wall.date() - epoch).num_days().try_into().ok()?)
                }
            }
        }
        _ => return None,
    })
}

fn flip(c: Cmp) -> Cmp {
    match c {
        Cmp::Lt => Cmp::Gt,
        Cmp::Le => Cmp::Ge,
        Cmp::Gt => Cmp::Lt,
        Cmp::Ge => Cmp::Le,
    }
}

fn op(c: Cmp) -> Op {
    match c {
        Cmp::Lt => Op::Lt,
        Cmp::Le => Op::Le,
        Cmp::Gt => Op::Gt,
        Cmp::Ge => Op::Ge,
    }
}

fn nullable(e: &Expr, reach: Reach) -> Option<Subject> {
    match (e, reach) {
        (
            Expr::Open {
                path,
                as_: Read::Any,
            },
            Reach::Nested,
        ) => steps(path),
        _ => subject(e, reach),
    }
}

fn one(subject: Subject, op: Op, values: Vec<Scalar>) -> Vec<PathPushdown> {
    vec![at(subject, op, values)]
}

fn at(subject: Subject, op: Op, values: Vec<Scalar>) -> PathPushdown {
    let mut predicate = Predicate::unary(subject.path[0].clone(), op);
    predicate.values = values;
    PathPushdown {
        source: subject.source,
        path: subject.path,
        predicate,
    }
}

fn simple(e: &Expr, tz: Tz, reach: Reach) -> Option<Vec<PathPushdown>> {
    let column = |e: &Expr| subject(e, reach);
    match e {
        Expr::Eq { left, right, .. } => {
            let (col, lit) = match (column(left), column(right)) {
                (Some(c), None) => (c, right),
                (None, Some(c)) => (c, left),
                _ => return None,
            };
            let value = scalar(lit, tz, true)?;
            Some(one(col, Op::Eq, vec![value]))
        }
        Expr::Cmp(c, a, b) => {
            let (col, lit, c) = match (column(a), column(b)) {
                (Some(col), None) => (col, b, *c),
                (None, Some(col)) => (col, a, flip(*c)),
                _ => return None,
            };
            let value = scalar(lit, tz, false)?;
            Some(one(col, op(c), vec![value]))
        }
        Expr::Range {
            item,
            low,
            high,
            low_open,
            high_open,
        } => {
            let lo = scalar(low, tz, false)?;
            let hi = scalar(high, tz, false)?;
            let low_op = if *low_open { Op::Gt } else { Op::Ge };
            let high_op = if *high_open { Op::Lt } else { Op::Le };
            Some(vec![
                at(column(item)?, low_op, vec![lo]),
                at(column(item)?, high_op, vec![hi]),
            ])
        }
        Expr::In { item, list, .. } => {
            let col = nullable(item, reach)?;
            let values = list
                .iter()
                .map(|v| scalar(v, tz, true))
                .collect::<Option<Vec<_>>>()?;
            Some(one(col, Op::In, values))
        }
        Expr::IsNull(x) => Some(one(nullable(x, reach)?, Op::IsNull, Vec::new())),
        Expr::Not(inner) => match inner.as_ref() {
            Expr::IsNull(x) => Some(one(nullable(x, reach)?, Op::IsNotNull, Vec::new())),
            _ => None,
        },
        Expr::Call(Func::StartsWith, args) => {
            let col = column(&args[0])?;
            let Expr::Str(prefix) = &args[1] else {
                return None;
            };
            Some(one(col, Op::Prefix, vec![Scalar::Str(prefix.clone())]))
        }
        Expr::Path(_)
        | Expr::Open {
            as_: Read::Bool, ..
        } => Some(one(column(e)?, Op::Eq, vec![Scalar::Bool(true)])),
        _ => None,
    }
}
