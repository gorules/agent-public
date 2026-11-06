use crate::predicate::Scalar as Param;
use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Tz;

use crate::dialect::Dialect;
use crate::ir::{Arith, Base, Each, Expr, Func, Part, Path, Scalar, Step, Unit};
use crate::schema::{DateKind, Ty};
use crate::{Error, Limits};

type R = Result<(), Error>;

pub struct Render {
    pub dialect: Dialect,
    pub timezone: Tz,
    pub sql: String,
    pub params: Vec<Param>,
    pub limits: Limits,
    pub inline: bool,
    overflow: bool,
}

fn unsupported(dialect: Dialect, what: &str) -> Error {
    Error {
        message: format!("{what} is not supported on {}", dialect.name()),
        span: None,
    }
}

const NUMBER: &str = r"^\s*[+-]?([0-9][0-9_]*(\.[0-9_]*)?|\.[0-9][0-9_]*)([eE][+-]?[0-9]+)?\s*$";
const NUMBER_POSIX: &str =
    r"^[[:space:]]*[+-]?([0-9][0-9_]*(\.[0-9_]*)?|\.[0-9][0-9_]*)([eE][+-]?[0-9]+)?[[:space:]]*$";
const NAIVE_DATE: &str =
    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}( ([01][0-9]|2[0-3]):[0-5][0-9](:[0-5][0-9](\.[0-9]+)?)?)?$";
const NAIVE_DATE_T: &str =
    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T([01][0-9]|2[0-3]):[0-5][0-9](:[0-5][0-9](\.[0-9]+)?)?$";
const OFFSET_DATE: &str = r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T([01][0-9]|2[0-3]):[0-5][0-9](:[0-5][0-9](\.[0-9]+)?)?(Z|[+-][0-9]{2}(:?[0-9]{2})?)$";
const EPOCH_MIN: &str = "-8334601228800000";
const EPOCH_MAX: &str = "8210266876799999";

struct Kinds {
    expr: String,
    string: &'static [&'static str],
    number: &'static [&'static str],
    boolean: &'static [&'static str],
    null: &'static str,
}

fn quoted(names: &[&str]) -> String {
    names
        .iter()
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Render {
    pub fn new(dialect: Dialect, timezone: Tz) -> Render {
        Render {
            dialect,
            timezone,
            sql: String::new(),
            params: Vec::new(),
            limits: Limits::default(),
            inline: false,
            overflow: false,
        }
    }

    fn nested(&self) -> Render {
        let mut inner = Render::new(self.dialect, self.timezone);
        inner.limits = self.limits;
        inner.inline = self.inline;
        inner
    }

    fn push(&mut self, s: &str) {
        if self.overflow {
            return;
        }
        if self.sql.len() + s.len() > self.limits.sql {
            self.overflow = true;
            return;
        }
        self.sql.push_str(s);
    }

    fn too_big(&self) -> Error {
        Error {
            message: format!(
                "the generated SQL would exceed {} bytes or {} parameters; simplify the expression",
                self.limits.sql, self.limits.params
            ),
            span: None,
        }
    }

    pub fn finish(&self) -> R {
        if self.overflow {
            return Err(self.too_big());
        }
        Ok(())
    }

    fn d(&self) -> Dialect {
        self.dialect
    }

    fn tz(&self) -> String {
        self.d().string(self.timezone.name())
    }

    pub fn expr(&mut self, e: &Expr, strict: bool) -> R {
        if self.overflow {
            return Err(self.too_big());
        }
        match e {
            Expr::Null => self.push("NULL"),
            Expr::Bool(b) => self.push(if *b { "TRUE" } else { "FALSE" }),
            Expr::Num(n) => {
                let text = n.normalize().to_string();
                if n.is_sign_negative() {
                    self.push(&format!("({text})"));
                } else {
                    self.push(&text);
                }
            }
            Expr::Str(s) => self.string(s),
            Expr::Time { micros, kind } => self.time(*micros, *kind)?,
            Expr::Path(p) => self.path(p)?,
            Expr::Open { path, as_ } => self.open(path, *as_)?,
            Expr::Not(inner) => {
                self.push("(NOT ");
                self.expr(inner, true)?;
                self.push(")");
            }
            Expr::And(a, b, true) if strict => {
                self.push("(CASE ");
                self.expr(a, true)?;
                self.push(" WHEN TRUE THEN ");
                self.expr(b, true)?;
                self.push(" WHEN FALSE THEN FALSE END)");
            }
            Expr::And(a, b, _) | Expr::Or(a, b) => {
                let op = if matches!(e, Expr::And(..)) {
                    " AND "
                } else {
                    " OR "
                };
                self.push("(");
                self.expr(a, strict)?;
                self.push(op);
                self.expr(b, strict)?;
                self.push(")");
            }
            Expr::Eq {
                left,
                right,
                nullable,
            } => self.eq(left, right, *nullable, strict)?,
            Expr::Cmp(c, a, b) => {
                self.push("(");
                self.expr(a, true)?;
                self.push(&format!(" {} ", c.sql()));
                self.expr(b, true)?;
                self.push(")");
            }
            Expr::IsNull(x) => {
                self.push("(");
                self.expr(x, true)?;
                self.push(" IS NULL)");
            }
            Expr::In {
                item,
                list,
                nullable,
            } => {
                let wrap = strict && *nullable;
                if wrap {
                    self.push("COALESCE(");
                }
                self.push("(");
                self.expr(item, true)?;
                self.push(" IN (");
                for (i, v) in list.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.expr(v, true)?;
                }
                self.push("))");
                if wrap {
                    self.push(", FALSE)");
                }
            }
            Expr::InArray { item, array } => self.in_array(item, array)?,
            Expr::Range {
                item,
                low,
                high,
                low_open,
                high_open,
            } => {
                self.push("(");
                if !low_open && !high_open {
                    self.expr(item, true)?;
                    self.push(" BETWEEN ");
                    self.expr(low, true)?;
                    self.push(" AND ");
                    self.expr(high, true)?;
                } else {
                    self.expr(item, true)?;
                    self.push(if *low_open { " > " } else { " >= " });
                    self.expr(low, true)?;
                    self.push(" AND ");
                    self.expr(item, true)?;
                    self.push(if *high_open { " < " } else { " <= " });
                    self.expr(high, true)?;
                }
                self.push(")");
            }
            Expr::Arith(op, a, b) => self.arith(*op, a, b)?,
            Expr::Neg(x) => {
                self.push("(-");
                self.expr(x, true)?;
                self.push(")");
            }
            Expr::Concat(parts) => {
                self.push("(");
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        self.push(" || ");
                    }
                    self.expr(p, true)?;
                }
                self.push(")");
            }
            Expr::Coalesce(a, b) => {
                self.push("COALESCE(");
                self.expr(a, true)?;
                self.push(", ");
                self.expr(b, true)?;
                self.push(")");
            }
            Expr::Case(c, t, f) => {
                self.push("(CASE ");
                self.expr(c, true)?;
                self.push(" WHEN TRUE THEN ");
                self.expr(t, strict)?;
                self.push(" WHEN FALSE THEN ");
                self.expr(f, strict)?;
                self.push(" END)");
            }
            Expr::Call(f, args) => self.call(*f, args)?,
            Expr::BoolOf(x) => {
                self.push("COALESCE(");
                self.expr(x, true)?;
                self.push(", FALSE)");
            }
            Expr::LocalTime { value, kind } => self.local(value, *kind)?,
            Expr::Parse { value, from } => self.parse(value, from)?,
            Expr::DatePart(part, x) => self.part(*part, x)?,
            Expr::DateTrunc(unit, x) => self.trunc(*unit, x)?,
            Expr::DateAdd {
                value,
                amount,
                unit,
                kind,
            } => self.add(value, *amount, *unit, *kind)?,
            Expr::Epoch { value, kind } => self.epoch(value, *kind)?,
            Expr::ToInstant { value, kind } => self.instant_of(value, *kind)?,
            Expr::Raw(sql) => self.push(sql),
            Expr::Each {
                kind,
                list,
                var,
                item,
                fallible,
                body,
            } => self.each(*kind, list, *var, item, body, *fallible, strict)?,
        }
        Ok(())
    }

    fn string(&mut self, s: &str) {
        if !s.is_empty() && !self.inline {
            let numbered = !matches!(self.d(), Dialect::DuckDb | Dialect::Snowflake);
            let value = Param::Str(s.to_string());
            if let Some(i) = self
                .params
                .iter()
                .position(|p| *p == value)
                .filter(|_| numbered)
            {
                let marker = self.d().marker(i);
                self.push(&marker);
                return;
            }
            if self.params.len() >= self.limits.params {
                self.overflow = true;
                return;
            }
            let marker = self.d().marker(self.params.len());
            self.push(&marker);
            self.params.push(value);
        } else {
            let lit = self.d().string(s);
            self.push(&lit);
        }
    }

    fn time(&mut self, micros: i64, kind: DateKind) -> R {
        let d = self.d();
        let instant = DateTime::<Utc>::from_timestamp_micros(micros).ok_or_else(|| Error {
            message: "the date is out of range".into(),
            span: None,
        })?;
        if kind == DateKind::Instant {
            let text = d.string(&instant.format("%Y-%m-%d %H:%M:%S%.6f+00:00").to_string());
            let lit = match d {
                Dialect::DuckDb | Dialect::Postgres => format!("TIMESTAMPTZ {text}"),
                Dialect::BigQuery | Dialect::Databricks => format!("TIMESTAMP {text}"),
                Dialect::Snowflake => format!("TO_TIMESTAMP_TZ({text})"),
                Dialect::Redshift => format!("CAST({text} AS TIMESTAMPTZ)"),
            };
            self.push(&lit);
            return Ok(());
        }
        let wall = instant.with_timezone(&self.timezone).naive_local();
        if kind == DateKind::Date && wall.time() == NaiveTime::MIN {
            let text = d.string(&wall.format("%Y-%m-%d").to_string());
            self.push(&format!("DATE {text}"));
            return Ok(());
        }
        let text = d.string(&wall.format("%Y-%m-%d %H:%M:%S%.6f").to_string());
        let lit = match d {
            Dialect::DuckDb | Dialect::Postgres | Dialect::Redshift => format!("TIMESTAMP {text}"),
            Dialect::BigQuery => format!("DATETIME {text}"),
            Dialect::Databricks => format!("TIMESTAMP_NTZ {text}"),
            Dialect::Snowflake => format!("TO_TIMESTAMP_NTZ({text})"),
        };
        self.push(&lit);
        Ok(())
    }

    fn base(&mut self, base: &Base) {
        match base {
            Base::Column {
                qualifier, name, ..
            } => {
                if let Some(q) = qualifier {
                    let q = self.d().ident(q);
                    self.push(&q);
                    self.push(".");
                }
                let n = self.d().ident(name);
                self.push(&n);
            }
            Base::Var(id) => self.push(&format!("_z{id}")),
        }
    }

    fn steps(&mut self, path: &Path, steps: &[(Step, Ty)]) -> R {
        let d = self.d();
        let mut out = String::new();
        std::mem::swap(&mut out, &mut self.sql);
        self.base(&path.base);
        let mut x = std::mem::replace(&mut self.sql, out);
        for (step, _) in steps {
            x = match (step, d) {
                (Step::Field(f), Dialect::DuckDb) => {
                    format!("struct_extract({x}, {})", d.string(f))
                }
                (Step::Field(f), Dialect::Postgres) => format!("({x}).{}", d.ident(f)),
                (Step::Field(f), Dialect::Snowflake) => format!("GET({x}, {})", d.string(f)),
                (Step::Field(f), _) => format!("{x}.{}", d.ident(f)),
                (Step::Key(k), Dialect::DuckDb) => format!("{x}[{}]", d.string(k)),
                (Step::Key(k), Dialect::Snowflake) => format!("GET({x}, {})", d.string(k)),
                (Step::Key(k), Dialect::Databricks) => {
                    format!("try_element_at({x}, {})", d.string(k))
                }
                (Step::Key(_), _) => return Err(unsupported(d, "reading map keys")),
                (Step::Index(i), Dialect::DuckDb) => format!("list_extract({x}, {})", i + 1),
                (Step::Index(i), Dialect::Postgres) => format!("({x})[{}]", i + 1),
                (Step::Index(i), Dialect::BigQuery) => format!("{x}[SAFE_OFFSET({i})]"),
                (Step::Index(i), Dialect::Snowflake) => format!("GET({x}, {i})"),
                (Step::Index(i), Dialect::Databricks) => format!("try_element_at({x}, {})", i + 1),
                (Step::Index(i), Dialect::Redshift) => format!("{x}[{i}]"),
            };
        }
        self.push(&x);
        Ok(())
    }

    fn leaf_ty(path: &Path) -> &Ty {
        path.steps.last().map(|(_, t)| t).unwrap_or(&path.base_ty)
    }

    fn path(&mut self, path: &Path) -> R {
        let d = self.d();
        let semi = !path.steps.is_empty() || matches!(path.base, Base::Var(_));
        let cast = match (d, semi) {
            (Dialect::Snowflake, true) => match Self::leaf_ty(path) {
                Ty::Number => Some("::FLOAT"),
                Ty::String => Some("::VARCHAR"),
                Ty::Bool => Some("::BOOLEAN"),
                Ty::Date {
                    kind: DateKind::Instant,
                } => Some("::TIMESTAMP_TZ"),
                Ty::Date {
                    kind: DateKind::Local,
                } => Some("::TIMESTAMP_NTZ"),
                Ty::Date {
                    kind: DateKind::Date,
                } => Some("::DATE"),
                _ => None,
            },
            (Dialect::Redshift, true) if !path.steps.is_empty() => match Self::leaf_ty(path) {
                Ty::Number => Some("::DOUBLE PRECISION"),
                Ty::String => Some("::VARCHAR"),
                Ty::Bool => Some("::BOOLEAN"),
                _ => None,
            },
            _ => None,
        };
        self.steps(path, &path.steps)?;
        if let Some(c) = cast {
            self.push(c);
        }
        Ok(())
    }

    fn json_split(&mut self, path: &Path) -> Result<(String, Vec<Step>), Error> {
        let mut split = path.steps.len();
        let mut container = &path.base_ty;
        for (i, (_, ty)) in path.steps.iter().enumerate() {
            if container.is_open() {
                split = i;
                break;
            }
            container = ty;
        }
        let mut col = String::new();
        std::mem::swap(&mut col, &mut self.sql);
        let rendered = self.steps(path, &path.steps[..split]);
        let col = std::mem::replace(&mut self.sql, col);
        rendered?;
        let steps = path.steps[split..].iter().map(|(s, _)| s.clone()).collect();
        Ok((col, steps))
    }

    fn json_key(&self, k: &str) -> R {
        if k.is_empty() && !matches!(self.d(), Dialect::DuckDb | Dialect::Postgres) {
            return Err(unsupported(self.d(), "empty JSON keys"));
        }
        Ok(())
    }

    fn json_path(&self, steps: &[Step]) -> Result<String, Error> {
        let d = self.d();
        Ok(match d {
            Dialect::DuckDb => {
                if steps.is_empty() {
                    return Ok(d.string("$"));
                }
                let mut p = String::new();
                for s in steps {
                    match s {
                        Step::Field(k) | Step::Key(k) => {
                            p.push('/');
                            p.push_str(&k.replace('~', "~0").replace('/', "~1"));
                        }
                        Step::Index(i) => p.push_str(&format!("/{i}")),
                    }
                }
                d.string(&p)
            }
            Dialect::BigQuery => {
                let mut p = String::from("$");
                for s in steps {
                    match s {
                        Step::Field(k) | Step::Key(k) => {
                            self.json_key(k)?;
                            p.push_str(&format!(
                                ".\"{}\"",
                                k.replace('\\', "\\\\").replace('"', "\\\"")
                            ))
                        }
                        Step::Index(i) => p.push_str(&format!("[{i}]")),
                    }
                }
                d.string(&p)
            }
            Dialect::Databricks => {
                let mut p = String::from("$");
                for s in steps {
                    match s {
                        Step::Field(k) | Step::Key(k) => {
                            self.json_key(k)?;
                            if k.contains('\'') || k.contains(']') {
                                return Err(unsupported(d, "JSON keys with quotes or brackets"));
                            }
                            p.push_str(&format!("['{k}']"))
                        }
                        Step::Index(i) => p.push_str(&format!("[{i}]")),
                    }
                }
                d.string(&p)
            }
            Dialect::Snowflake => {
                let mut p = String::new();
                for s in steps {
                    match s {
                        Step::Field(k) | Step::Key(k) => {
                            self.json_key(k)?;
                            if !p.is_empty() {
                                p.push('.');
                            }
                            p.push_str(&format!("\"{}\"", k.replace('"', "\"\"")));
                        }
                        Step::Index(i) => p.push_str(&format!("[{i}]")),
                    }
                }
                d.string(&p)
            }
            Dialect::Postgres => {
                let items: Vec<String> = steps
                    .iter()
                    .map(|s| match s {
                        Step::Field(k) | Step::Key(k) => {
                            format!("\"{}\"", k.replace('\\', "\\\\").replace('"', "\\\""))
                        }
                        Step::Index(i) => i.to_string(),
                    })
                    .collect();
                d.string(&format!("{{{}}}", items.join(",")))
            }
            Dialect::Redshift => {
                let mut args = Vec::new();
                for s in steps {
                    match s {
                        Step::Field(k) | Step::Key(k) => {
                            self.json_key(k)?;
                            args.push(d.string(k))
                        }
                        Step::Index(_) => return Err(unsupported(d, "indexing into JSON arrays")),
                    }
                }
                args.join(", ")
            }
        })
    }

    fn json_text(&self, col: &str, steps: &[Step]) -> Result<String, Error> {
        let d = self.d();
        let p = self.json_path(steps)?;
        Ok(match d {
            Dialect::DuckDb => format!("json_extract_string({col}, {p})"),
            Dialect::BigQuery => format!("JSON_VALUE({col}, {p})"),
            Dialect::Databricks => format!("get_json_object({col}, {p})"),
            Dialect::Snowflake if steps.is_empty() => format!("{col}::VARCHAR"),
            Dialect::Snowflake => format!("GET_PATH({col}, {p})::VARCHAR"),
            Dialect::Postgres => format!("(CAST({col} AS JSONB) #>> {p})"),
            Dialect::Redshift if steps.is_empty() => col.to_string(),
            Dialect::Redshift => format!("json_extract_path_text({col}, {p})"),
        })
    }

    fn json_kinds(&self, col: &str, steps: &[Step]) -> Result<Option<Kinds>, Error> {
        let d = self.d();
        let p = self.json_path(steps)?;
        Ok(match d {
            Dialect::DuckDb => Some(Kinds {
                expr: format!("json_type({col}, {p})"),
                string: &["VARCHAR"],
                number: &["BIGINT", "UBIGINT", "DOUBLE", "HUGEINT"],
                boolean: &["BOOLEAN"],
                null: "NULL",
            }),
            Dialect::Postgres => Some(Kinds {
                expr: format!("jsonb_typeof(CAST({col} AS JSONB) #> {p})"),
                string: &["string"],
                number: &["number"],
                boolean: &["boolean"],
                null: "null",
            }),
            Dialect::BigQuery => Some(Kinds {
                expr: format!("JSON_TYPE(JSON_QUERY({col}, {p}))"),
                string: &["string"],
                number: &["number"],
                boolean: &["boolean"],
                null: "null",
            }),
            Dialect::Snowflake => Some(Kinds {
                expr: if steps.is_empty() {
                    format!("TYPEOF({col})")
                } else {
                    format!("TYPEOF(GET_PATH({col}, {p}))")
                },
                string: &["VARCHAR"],
                number: &["INTEGER", "DECIMAL", "DOUBLE"],
                boolean: &["BOOLEAN"],
                null: "NULL_VALUE",
            }),
            Dialect::Databricks | Dialect::Redshift => None,
        })
    }

    fn open(&mut self, path: &Path, as_: Scalar) -> R {
        let d = self.d();
        let (col, steps) = self.json_split(path)?;
        let t = self.json_text(&col, &steps)?;
        let kinds = self.json_kinds(&col, &steps)?;
        let double = d.double_type();
        let sql = match (as_, kinds) {
            (Scalar::Str, Some(k)) => format!(
                "(CASE WHEN {} IN ({}) THEN {t} END)",
                k.expr,
                quoted(k.string)
            ),
            (Scalar::Num, Some(k)) => {
                format!(
                    "(CASE WHEN {} IN ({}) THEN CAST({t} AS {double}) END)",
                    k.expr,
                    quoted(k.number)
                )
            }
            (Scalar::Bool, Some(k)) => {
                format!(
                    "(CASE WHEN {} IN ({}) THEN {t} = 'true' END)",
                    k.expr,
                    quoted(k.boolean)
                )
            }
            (Scalar::Any, Some(k)) => format!("NULLIF({}, '{}')", k.expr, k.null),
            (Scalar::Text, Some(k)) => {
                let mut scalars: Vec<&str> = k.string.to_vec();
                scalars.extend(k.number);
                scalars.extend(k.boolean);
                format!(
                    "(CASE WHEN {e} IN ({}) THEN {t} WHEN {e} IS NULL OR {e} = '{}' THEN 'null' END)",
                    quoted(&scalars),
                    k.null,
                    e = k.expr
                )
            }
            (Scalar::Parse, Some(k)) => {
                let mut parsed = self.nested();
                parsed.zen_number(|r| {
                    r.push(&t);
                    Ok(())
                })?;
                format!(
                    "(CASE WHEN {e} IN ({}) THEN CAST({t} AS {double}) WHEN {e} IN ({}) THEN {} WHEN {e} IN ({}) THEN (CASE {t} WHEN 'true' THEN 1 ELSE 0 END) END)",
                    quoted(k.number),
                    quoted(k.string),
                    parsed.sql,
                    quoted(k.boolean),
                    e = k.expr
                )
            }
            (Scalar::Container, Some(k)) => {
                let names = match d {
                    Dialect::DuckDb | Dialect::Snowflake => "'OBJECT', 'ARRAY'",
                    _ => "'object', 'array'",
                };
                format!("COALESCE({} IN ({names}), FALSE)", k.expr)
            }
            (Scalar::Container, None) => "FALSE".to_string(),
            (Scalar::Str | Scalar::Any, None) => t,
            (Scalar::Text, None) => format!("COALESCE({t}, 'null')"),
            (Scalar::Num | Scalar::Parse, None) => {
                let mut parsed = self.nested();
                parsed.zen_number(|r| {
                    r.push(&t);
                    Ok(())
                })?;
                parsed.sql
            }
            (Scalar::Bool, None) => {
                format!("(CASE {t} WHEN 'true' THEN TRUE WHEN 'false' THEN FALSE END)")
            }
        };
        self.push(&sql);
        Ok(())
    }

    fn wrap(&mut self, open: &str, inner: impl FnOnce(&mut Render) -> R, close: &str) -> R {
        self.push(open);
        inner(self)?;
        self.push(close);
        Ok(())
    }

    fn matches_re(&mut self, inner: &impl Fn(&mut Render) -> R, re: &str, posix: &str) -> R {
        let d = self.d();
        let lit = d.string(if d == Dialect::Redshift { posix } else { re });
        match d {
            Dialect::DuckDb => self.wrap("regexp_full_match(", inner, &format!(", {lit})")),
            Dialect::BigQuery => self.wrap("REGEXP_CONTAINS(", inner, &format!(", {lit})")),
            Dialect::Snowflake => self.wrap("REGEXP_LIKE(", inner, &format!(", {lit})")),
            Dialect::Databricks => self.wrap("(", inner, &format!(" RLIKE {lit})")),
            Dialect::Postgres | Dialect::Redshift => self.wrap("(", inner, &format!(" ~ {lit})")),
        }
    }

    fn trim_ws(&mut self, inner: &impl Fn(&mut Render) -> R) -> R {
        let d = self.d();
        let re = d.string(if d == Dialect::Redshift {
            "^[[:space:]]+|[[:space:]]+$"
        } else {
            r"^\s+|\s+$"
        });
        match d {
            Dialect::DuckDb | Dialect::Postgres => {
                self.wrap("regexp_replace(", inner, &format!(", {re}, '', 'g')"))
            }
            Dialect::BigQuery | Dialect::Snowflake | Dialect::Redshift => {
                self.wrap("REGEXP_REPLACE(", inner, &format!(", {re}, '')"))
            }
            Dialect::Databricks => self.wrap("regexp_replace(", inner, &format!(", {re}, '')")),
        }
    }

    fn zen_number(&mut self, inner: impl Fn(&mut Render) -> R) -> R {
        let double = self.d().double_type();
        self.push("(CASE WHEN ");
        self.matches_re(&inner, NUMBER, NUMBER_POSIX)?;
        self.push(" THEN CAST(REPLACE(");
        self.trim_ws(&inner)?;
        self.push(&format!(", '_', '') AS {double}) END)"));
        Ok(())
    }

    fn parse_text(&mut self, inner: impl Fn(&mut Render) -> R) -> R {
        let d = self.d();
        let utc = self.timezone == Tz::UTC;
        let tz = self.tz();
        match d {
            Dialect::DuckDb => {
                self.push("(CASE WHEN ");
                self.matches_re(&inner, OFFSET_DATE, OFFSET_DATE)?;
                self.push(" THEN TRY_CAST(");
                inner(self)?;
                self.push(" AS TIMESTAMPTZ) WHEN ");
                self.matches_re(&inner, NAIVE_DATE, NAIVE_DATE)?;
                self.push(" OR ");
                self.matches_re(&inner, NAIVE_DATE_T, NAIVE_DATE_T)?;
                if utc {
                    self.push(" THEN CAST(TRY_CAST(");
                    inner(self)?;
                    self.push(" AS TIMESTAMP) AS TIMESTAMPTZ) END)");
                } else {
                    self.push(&format!(" THEN timezone({tz}, TRY_CAST("));
                    inner(self)?;
                    self.push(" AS TIMESTAMP)) END)");
                }
                Ok(())
            }
            Dialect::Postgres => {
                self.push("(CASE WHEN ");
                self.matches_re(&inner, OFFSET_DATE, OFFSET_DATE)?;
                self.push(" AND pg_input_is_valid(");
                inner(self)?;
                self.push(", 'timestamptz') THEN CAST(");
                inner(self)?;
                self.push(" AS TIMESTAMPTZ) WHEN (");
                self.matches_re(&inner, NAIVE_DATE, NAIVE_DATE)?;
                self.push(" OR ");
                self.matches_re(&inner, NAIVE_DATE_T, NAIVE_DATE_T)?;
                self.push(") AND pg_input_is_valid(");
                inner(self)?;
                self.push(", 'timestamp') THEN (CAST(");
                inner(self)?;
                self.push(&format!(" AS TIMESTAMP) AT TIME ZONE {tz}) END)"));
                Ok(())
            }
            Dialect::Redshift => {
                self.push("(CASE WHEN ");
                self.matches_re(&inner, OFFSET_DATE, OFFSET_DATE)?;
                self.push(" OR ");
                self.matches_re(&inner, NAIVE_DATE, NAIVE_DATE)?;
                self.push(" OR ");
                self.matches_re(&inner, NAIVE_DATE_T, NAIVE_DATE_T)?;
                self.push(" THEN CAST(");
                inner(self)?;
                self.push(" AS TIMESTAMPTZ) END)");
                Ok(())
            }
            Dialect::BigQuery => self.wrap("SAFE_CAST(", inner, " AS TIMESTAMP)"),
            Dialect::Snowflake => self.wrap("TRY_TO_TIMESTAMP_TZ(", inner, ")"),
            Dialect::Databricks => self.wrap("try_to_timestamp(", inner, ")"),
        }
    }

    fn eq(&mut self, l: &Expr, r: &Expr, nullable: (bool, bool), strict: bool) -> R {
        let d = self.d();
        let plain = |r_: &mut Render| -> R {
            r_.push("(");
            r_.expr(l, true)?;
            r_.push(" = ");
            r_.expr(r, true)?;
            r_.push(")");
            Ok(())
        };
        let (ln, rn) = nullable;
        let literal = l.is_literal() || r.is_literal();
        if (!ln && !rn) || (!strict && literal) {
            return plain(self);
        }
        if ln && rn {
            if d == Dialect::Redshift {
                self.push("COALESCE(");
                self.expr(l, true)?;
                self.push(" = ");
                self.expr(r, true)?;
                self.push(", (");
                self.expr(l, true)?;
                self.push(" IS NULL AND ");
                self.expr(r, true)?;
                self.push(" IS NULL))");
                return Ok(());
            }
            self.push("(");
            self.expr(l, true)?;
            self.push(" IS NOT DISTINCT FROM ");
            self.expr(r, true)?;
            self.push(")");
            return Ok(());
        }
        if !strict {
            return plain(self);
        }
        let other = if ln { r } else { l };
        if other.is_literal() {
            self.push("COALESCE(");
            plain(self)?;
            self.push(", FALSE)");
            return Ok(());
        }
        self.push("(CASE WHEN ");
        self.expr(other, true)?;
        self.push(" IS NULL THEN NULL ELSE COALESCE(");
        plain(self)?;
        self.push(", FALSE) END)");
        Ok(())
    }

    fn in_array(&mut self, item: &Expr, array: &Expr) -> R {
        let d = self.d();
        if d == Dialect::Redshift {
            return Err(unsupported(d, "searching arrays"));
        }
        self.push("(CASE WHEN ");
        self.expr(array, true)?;
        self.push(" IS NULL THEN NULL ELSE ");
        match d {
            Dialect::DuckDb => {
                self.push("(len(list_filter(");
                self.expr(array, true)?;
                self.push(", lambda _e: _e IS NOT DISTINCT FROM ");
                self.expr(item, true)?;
                self.push(")) > 0)");
            }
            Dialect::Postgres => {
                self.push("(array_position(");
                self.expr(array, true)?;
                self.push(", ");
                self.expr(item, true)?;
                self.push(") IS NOT NULL)");
            }
            Dialect::BigQuery => {
                self.push("EXISTS (SELECT 1 FROM UNNEST(");
                self.expr(array, true)?;
                self.push(") AS _e WHERE _e IS NOT DISTINCT FROM ");
                self.expr(item, true)?;
                self.push(")");
            }
            Dialect::Snowflake => {
                self.push("(ARRAY_SIZE(FILTER(");
                self.expr(array, true)?;
                self.push(", _e -> EQUAL_NULL(_e, ");
                self.expr(item, true)?;
                self.push("::VARIANT))) > 0)");
            }
            Dialect::Databricks => {
                self.push("exists(");
                self.expr(array, true)?;
                self.push(", _e -> _e <=> ");
                self.expr(item, true)?;
                self.push(")");
            }
            Dialect::Redshift => unreachable!(),
        }
        self.push(" END)");
        Ok(())
    }

    fn arith(&mut self, op: Arith, a: &Expr, b: &Expr) -> R {
        let d = self.d();
        let nonzero = matches!(b, Expr::Num(n) if !n.is_zero());
        let divisor = |r: &mut Render| -> R {
            if nonzero {
                r.expr(b, true)
            } else {
                r.push("NULLIF(");
                r.expr(b, true)?;
                r.push(", 0)");
                Ok(())
            }
        };
        match op {
            Arith::Add | Arith::Sub | Arith::Mul => {
                let sym = match op {
                    Arith::Add => " + ",
                    Arith::Sub => " - ",
                    _ => " * ",
                };
                self.push("(");
                self.expr(a, true)?;
                self.push(sym);
                self.expr(b, true)?;
                self.push(")");
            }
            Arith::Div => {
                self.push("(");
                match d {
                    Dialect::Postgres => self.wrap("CAST(", |r| r.expr(a, true), " AS NUMERIC)")?,
                    Dialect::Redshift => {
                        self.wrap("CAST(", |r| r.expr(a, true), " AS DOUBLE PRECISION)")?
                    }
                    _ => self.expr(a, true)?,
                }
                self.push(" / ");
                divisor(self)?;
                self.push(")");
            }
            Arith::Mod if d == Dialect::Postgres => {
                self.push("MOD(CAST(");
                self.expr(a, true)?;
                self.push(" AS NUMERIC), CAST(");
                divisor(self)?;
                self.push(" AS NUMERIC))");
            }
            Arith::Mod => {
                if d == Dialect::DuckDb {
                    self.push("(");
                    self.expr(a, true)?;
                    self.push(" % ");
                    divisor(self)?;
                    self.push(")");
                } else {
                    self.push("MOD(");
                    self.expr(a, true)?;
                    self.push(", ");
                    divisor(self)?;
                    self.push(")");
                }
            }
            Arith::Pow => {
                let Expr::Num(exp) = b else {
                    return Err(Error {
                        message: "the exponent of ^ must be a constant number".into(),
                        span: None,
                    });
                };
                let whole = exp.fract().is_zero();
                let negative = exp.is_sign_negative();
                if negative {
                    self.push("(CASE ");
                    self.expr(a, true)?;
                    self.push(" WHEN 0 THEN 0 ELSE ");
                }
                if whole {
                    self.push("POWER(");
                    self.expr(a, true)?;
                    self.push(", ");
                    self.expr(b, true)?;
                    self.push(")");
                } else {
                    self.push("(SIGN(");
                    self.expr(a, true)?;
                    self.push(") * POWER(ABS(");
                    self.expr(a, true)?;
                    self.push("), ");
                    self.expr(b, true)?;
                    self.push("))");
                }
                if negative {
                    self.push(" END)");
                }
            }
        }
        Ok(())
    }

    fn fn1(&mut self, name: &str, x: &Expr) -> R {
        self.push(name);
        self.push("(");
        self.expr(x, true)?;
        self.push(")");
        Ok(())
    }

    fn fn2(&mut self, name: &str, a: &Expr, b: &Expr) -> R {
        self.push(name);
        self.push("(");
        self.expr(a, true)?;
        self.push(", ");
        self.expr(b, true)?;
        self.push(")");
        Ok(())
    }

    fn call(&mut self, f: Func, args: &[Expr]) -> R {
        let d = self.d();
        match f {
            Func::Upper => self.fn1("UPPER", &args[0]),
            Func::Lower => self.fn1("LOWER", &args[0]),
            Func::Trim => self.trim_ws(&|r: &mut Render| r.expr(&args[0], true)),
            Func::StartsWith | Func::EndsWith => {
                let start = f == Func::StartsWith;
                let name = match (d, start) {
                    (Dialect::DuckDb, true) | (Dialect::Postgres, true) => Some("starts_with"),
                    (Dialect::DuckDb, false) => Some("ends_with"),
                    (Dialect::BigQuery, true) => Some("STARTS_WITH"),
                    (Dialect::BigQuery, false) => Some("ENDS_WITH"),
                    (Dialect::Snowflake, true) => Some("STARTSWITH"),
                    (Dialect::Snowflake, false) => Some("ENDSWITH"),
                    (Dialect::Databricks, true) => Some("startswith"),
                    (Dialect::Databricks, false) => Some("endswith"),
                    _ => None,
                };
                if let Some(name) = name {
                    return self.fn2(name, &args[0], &args[1]);
                }
                let len = if d == Dialect::Redshift {
                    "LEN"
                } else {
                    "LENGTH"
                };
                self.push(if start { "(LEFT(" } else { "(RIGHT(" });
                self.expr(&args[0], true)?;
                self.push(&format!(", {len}("));
                self.expr(&args[1], true)?;
                self.push(")) = ");
                self.expr(&args[1], true)?;
                self.push(")");
                Ok(())
            }
            Func::StrContains => match d {
                Dialect::DuckDb | Dialect::Databricks => self.fn2("contains", &args[0], &args[1]),
                Dialect::Snowflake => self.fn2("CONTAINS", &args[0], &args[1]),
                _ => {
                    self.push("(");
                    self.fn2("STRPOS", &args[0], &args[1])?;
                    self.push(" > 0)");
                    Ok(())
                }
            },
            Func::Matches => match d {
                Dialect::DuckDb => self.fn2("regexp_matches", &args[0], &args[1]),
                Dialect::BigQuery => self.fn2("REGEXP_CONTAINS", &args[0], &args[1]),
                Dialect::Snowflake => {
                    self.push("(");
                    self.fn2("REGEXP_INSTR", &args[0], &args[1])?;
                    self.push(" > 0)");
                    Ok(())
                }
                Dialect::Databricks | Dialect::Postgres | Dialect::Redshift => {
                    let op = if d == Dialect::Databricks {
                        " RLIKE "
                    } else {
                        " ~ "
                    };
                    self.push("(");
                    self.expr(&args[0], true)?;
                    self.push(op);
                    self.expr(&args[1], true)?;
                    self.push(")");
                    Ok(())
                }
            },
            Func::StrLen => match d {
                Dialect::DuckDb => self.fn1("strlen", &args[0]),
                Dialect::BigQuery => self.fn1("BYTE_LENGTH", &args[0]),
                Dialect::Snowflake => self.wrap(
                    "LENGTH(TO_BINARY(",
                    |r| r.expr(&args[0], true),
                    ", 'UTF-8'))",
                ),
                _ => self.fn1("OCTET_LENGTH", &args[0]),
            },
            Func::ListLen => match d {
                Dialect::DuckDb => self.fn1("len", &args[0]),
                Dialect::Postgres => self.fn1("cardinality", &args[0]),
                Dialect::BigQuery => self.fn1("ARRAY_LENGTH", &args[0]),
                Dialect::Snowflake => self.fn1("ARRAY_SIZE", &args[0]),
                Dialect::Databricks => {
                    self.push("(CASE WHEN ");
                    self.expr(&args[0], true)?;
                    self.push(" IS NULL THEN NULL ELSE size(");
                    self.expr(&args[0], true)?;
                    self.push(") END)");
                    Ok(())
                }
                Dialect::Redshift => self.fn1("get_array_length", &args[0]),
            },
            Func::Abs => self.fn1("ABS", &args[0]),
            Func::Floor => self.fn1("FLOOR", &args[0]),
            Func::Ceil => self.fn1("CEIL", &args[0]),
            Func::Round => {
                if d == Dialect::Postgres {
                    self.push("ROUND(CAST(");
                    self.expr(&args[0], true)?;
                    self.push(" AS NUMERIC), ");
                    self.expr(&args[1], true)?;
                    self.push(")");
                    Ok(())
                } else {
                    self.fn2("ROUND", &args[0], &args[1])
                }
            }
            Func::Trunc => {
                if d == Dialect::Databricks {
                    self.push("(CASE WHEN ");
                    self.expr(&args[0], true)?;
                    self.push(" < 0 THEN CEIL(");
                    self.expr(&args[0], true)?;
                    self.push(") ELSE FLOOR(");
                    self.expr(&args[0], true)?;
                    self.push(") END)");
                    Ok(())
                } else {
                    self.fn1("TRUNC", &args[0])
                }
            }
            Func::ToString => {
                let t = d.text_type();
                self.wrap(
                    "LOWER(CAST(",
                    |r| r.expr(&args[0], true),
                    &format!(" AS {t}))"),
                )
            }
            Func::ToNumber => self.zen_number(|r| r.expr(&args[0], true)),
            Func::IsNumeric => {
                self.push("COALESCE(");
                self.matches_re(
                    &|r: &mut Render| r.expr(&args[0], true),
                    NUMBER,
                    NUMBER_POSIX,
                )?;
                self.push(", FALSE)");
                Ok(())
            }
            Func::Sum | Func::Avg | Func::Min | Func::Max | Func::Median => {
                self.aggregate(f, &args[0])
            }
        }
    }

    fn aggregate(&mut self, f: Func, list: &Expr) -> R {
        let d = self.d();
        if d == Dialect::Redshift {
            return self.aggregate_plain(f, list);
        }
        self.push("(CASE WHEN ");
        self.has_null(list)?;
        self.push(" THEN NULL ELSE ");
        self.aggregate_plain(f, list)?;
        self.push(" END)");
        Ok(())
    }

    fn has_null(&mut self, list: &Expr) -> R {
        match self.d() {
            Dialect::DuckDb => self.wrap(
                "(len(list_filter(",
                |r| r.expr(list, true),
                ", lambda _e: _e IS NULL)) > 0)",
            ),
            Dialect::Postgres => self.wrap(
                "EXISTS (SELECT 1 FROM unnest(",
                |r| r.expr(list, true),
                ") AS _e WHERE _e IS NULL)",
            ),
            Dialect::BigQuery => self.wrap(
                "EXISTS (SELECT 1 FROM UNNEST(",
                |r| r.expr(list, true),
                ") AS _e WHERE _e IS NULL)",
            ),
            Dialect::Snowflake => self.wrap(
                "(ARRAY_SIZE(FILTER(",
                |r| r.expr(list, true),
                ", _e -> _e IS NULL OR IS_NULL_VALUE(_e))) > 0)",
            ),
            Dialect::Databricks => {
                self.wrap("exists(", |r| r.expr(list, true), ", _e -> _e IS NULL)")
            }
            Dialect::Redshift => Err(unsupported(Dialect::Redshift, "arrays")),
        }
    }

    fn aggregate_plain(&mut self, f: Func, list: &Expr) -> R {
        let d = self.d();
        let what = match f {
            Func::Sum => "sum",
            Func::Avg => "avg",
            Func::Min => "min",
            Func::Max => "max",
            _ => "median",
        };
        match d {
            Dialect::DuckDb => {
                if f == Func::Sum {
                    self.push("(CASE WHEN len(");
                    self.expr(list, true)?;
                    self.push(") = 0 THEN 0 ELSE list_sum(");
                    self.expr(list, true)?;
                    self.push(") END)");
                    return Ok(());
                }
                self.fn1(&format!("list_{what}"), list)
            }
            Dialect::BigQuery | Dialect::Postgres => {
                let agg = match (f, d) {
                    (Func::Median, Dialect::BigQuery) => {
                        return Err(unsupported(d, "median() over arrays"));
                    }
                    (Func::Median, _) => {
                        "percentile_cont(0.5) WITHIN GROUP (ORDER BY _e)".to_string()
                    }
                    (Func::Sum, _) => "COALESCE(SUM(_e), 0)".to_string(),
                    _ => format!("{}(_e)", what.to_uppercase()),
                };
                let unnest = if d == Dialect::BigQuery {
                    "UNNEST"
                } else {
                    "unnest"
                };
                self.push("(CASE WHEN ");
                self.expr(list, true)?;
                self.push(&format!(
                    " IS NULL THEN NULL ELSE (SELECT {agg} FROM {unnest}("
                ));
                self.expr(list, true)?;
                self.push(") AS _e) END)");
                Ok(())
            }
            Dialect::Snowflake => match f {
                Func::Min => self.wrap("ARRAY_MIN(", |r| r.expr(list, true), ")::FLOAT"),
                Func::Max => self.wrap("ARRAY_MAX(", |r| r.expr(list, true), ")::FLOAT"),
                Func::Sum => self.wrap(
                    "REDUCE(",
                    |r| r.expr(list, true),
                    ", 0, (_a, _e) -> _a + _e::FLOAT)",
                ),
                Func::Avg => {
                    self.push("(REDUCE(");
                    self.expr(list, true)?;
                    self.push(", 0, (_a, _e) -> _a + _e::FLOAT) / NULLIF(ARRAY_SIZE(");
                    self.expr(list, true)?;
                    self.push("), 0))");
                    Ok(())
                }
                _ => Err(unsupported(d, "median() over arrays")),
            },
            Dialect::Databricks => match f {
                Func::Min => self.fn1("array_min", list),
                Func::Max => self.fn1("array_max", list),
                Func::Sum => self.wrap(
                    "aggregate(",
                    |r| r.expr(list, true),
                    ", CAST(0 AS DOUBLE), (_a, _e) -> _a + _e)",
                ),
                Func::Avg => {
                    self.push("(aggregate(");
                    self.expr(list, true)?;
                    self.push(", CAST(0 AS DOUBLE), (_a, _e) -> _a + _e) / NULLIF(size(");
                    self.expr(list, true)?;
                    self.push("), 0))");
                    Ok(())
                }
                _ => Err(unsupported(d, "median() over arrays")),
            },
            Dialect::Redshift => Err(unsupported(d, &format!("{what}() over arrays"))),
        }
    }

    fn local(&mut self, value: &Expr, kind: DateKind) -> R {
        let d = self.d();
        let tz = self.tz();
        match (kind, d) {
            (DateKind::Instant, Dialect::DuckDb) if self.timezone == Tz::UTC => {
                self.wrap("CAST(", |r| r.expr(value, true), " AS TIMESTAMP)")
            }
            (DateKind::Instant, Dialect::DuckDb) => {
                self.wrap(&format!("timezone({tz}, "), |r| r.expr(value, true), ")")
            }
            (DateKind::Instant, Dialect::Postgres) => self.wrap(
                "(",
                |r| r.expr(value, true),
                &format!(" AT TIME ZONE {tz})"),
            ),
            (DateKind::Instant, Dialect::Redshift) => self.wrap(
                &format!("CONVERT_TIMEZONE({tz}, "),
                |r| r.expr(value, true),
                ")",
            ),
            (DateKind::Instant, Dialect::BigQuery) => {
                self.wrap("DATETIME(", |r| r.expr(value, true), &format!(", {tz})"))
            }
            (DateKind::Instant, Dialect::Snowflake) => self.wrap(
                &format!("CONVERT_TIMEZONE({tz}, "),
                |r| r.expr(value, true),
                ")::TIMESTAMP_NTZ",
            ),
            (DateKind::Instant, Dialect::Databricks) => self.wrap(
                "from_utc_timestamp(",
                |r| r.expr(value, true),
                &format!(", {tz})"),
            ),
            (DateKind::Date, Dialect::BigQuery) => self.fn1("DATETIME", value),
            (DateKind::Date, Dialect::Snowflake) => {
                self.wrap("", |r| r.expr(value, true), "::TIMESTAMP_NTZ")
            }
            (DateKind::Date, Dialect::Databricks) => {
                self.wrap("CAST(", |r| r.expr(value, true), " AS TIMESTAMP_NTZ)")
            }
            (DateKind::Date, _) => self.wrap("CAST(", |r| r.expr(value, true), " AS TIMESTAMP)"),
            _ => self.expr(value, true),
        }
    }

    fn instant_of(&mut self, value: &Expr, kind: DateKind) -> R {
        let d = self.d();
        let tz = self.tz();
        let utc = self.timezone == Tz::UTC;
        if kind == DateKind::Instant {
            return self.expr(value, true);
        }
        match d {
            Dialect::DuckDb if utc => self.wrap(
                "CAST(CAST(",
                |r| r.expr(value, true),
                " AS TIMESTAMP) AS TIMESTAMPTZ)",
            ),
            Dialect::DuckDb => self.wrap(
                &format!("timezone({tz}, CAST("),
                |r| r.expr(value, true),
                " AS TIMESTAMP))",
            ),
            Dialect::Postgres => self.wrap(
                "(CAST(",
                |r| r.expr(value, true),
                &format!(" AS TIMESTAMP) AT TIME ZONE {tz})"),
            ),
            Dialect::BigQuery => {
                self.wrap("TIMESTAMP(", |r| r.expr(value, true), &format!(", {tz})"))
            }
            Dialect::Snowflake => self.wrap(
                &format!("TO_TIMESTAMP_TZ(TO_VARCHAR(CONVERT_TIMEZONE({tz}, 'UTC', "),
                |r| r.expr(value, true),
                "::TIMESTAMP_NTZ), 'YYYY-MM-DD HH24:MI:SS.FF9') || ' +00:00', 'YYYY-MM-DD HH24:MI:SS.FF9 TZH:TZM')",
            ),
            Dialect::Databricks => self.wrap(
                "to_utc_timestamp(",
                |r| r.expr(value, true),
                &format!(", {tz})"),
            ),
            Dialect::Redshift => self.wrap(
                &format!("CONVERT_TIMEZONE({tz}, 'UTC', "),
                |r| r.expr(value, true),
                ")",
            ),
        }
    }

    fn parse(&mut self, value: &Expr, from: &Ty) -> R {
        let d = self.d();
        match from {
            Ty::String => self.parse_text(|r| r.expr(value, true)),
            Ty::Number => {
                self.push("(CASE WHEN ");
                self.expr(value, true)?;
                self.push(&format!(" BETWEEN {EPOCH_MIN} AND {EPOCH_MAX} THEN "));
                match d {
                    Dialect::DuckDb => {
                        self.wrap("to_timestamp(TRUNC(", |r| r.expr(value, true), ") / 1000)")?
                    }
                    Dialect::Postgres => self.wrap(
                        "to_timestamp(TRUNC(",
                        |r| r.expr(value, true),
                        ") / 1000.0)",
                    )?,
                    Dialect::BigQuery => self.wrap(
                        "SAFE.TIMESTAMP_MILLIS(CAST(TRUNC(",
                        |r| r.expr(value, true),
                        ") AS INT64))",
                    )?,
                    Dialect::Snowflake => self.wrap(
                        "TO_TIMESTAMP_TZ(CAST(TRUNC(",
                        |r| r.expr(value, true),
                        ") AS NUMBER), 3)",
                    )?,
                    Dialect::Databricks => self.wrap(
                        "timestamp_millis(CAST(",
                        |r| r.expr(value, true),
                        " AS BIGINT))",
                    )?,
                    Dialect::Redshift => self.wrap(
                        "(TIMESTAMP 'epoch' + TRUNC(",
                        |r| r.expr(value, true),
                        ") / 1000.0 * INTERVAL '1 second')",
                    )?,
                }
                self.push(" END)");
                Ok(())
            }
            other => Err(Error {
                message: format!("cannot read a {} as a date", other.name()),
                span: None,
            }),
        }
    }

    fn part(&mut self, part: Part, x: &Expr) -> R {
        let d = self.d();
        let std_name = match part {
            Part::Second => "second",
            Part::Minute => "minute",
            Part::Hour => "hour",
            Part::Day => "day",
            Part::DayOfYear => "doy",
            Part::Week => "week",
            Part::Weekday => "isodow",
            Part::Month => "month",
            Part::Quarter => "quarter",
            Part::Year => "year",
        };
        match d {
            Dialect::DuckDb => self.wrap(
                &format!("EXTRACT({std_name} FROM "),
                |r| r.expr(x, true),
                ")",
            ),
            Dialect::Postgres => {
                let (open, close) = if part == Part::Second {
                    ("FLOOR(", ")")
                } else {
                    ("", "")
                };
                self.wrap(
                    &format!("{open}EXTRACT({std_name} FROM "),
                    |r| r.expr(x, true),
                    &format!("){close}"),
                )
            }
            Dialect::BigQuery => match part {
                Part::Weekday => self.wrap(
                    "(MOD(EXTRACT(DAYOFWEEK FROM ",
                    |r| r.expr(x, true),
                    ") + 5, 7) + 1)",
                ),
                _ => {
                    let name = match part {
                        Part::DayOfYear => "DAYOFYEAR",
                        Part::Week => "ISOWEEK",
                        other => part_upper(other),
                    };
                    self.wrap(&format!("EXTRACT({name} FROM "), |r| r.expr(x, true), ")")
                }
            },
            Dialect::Snowflake => match part {
                Part::Weekday => self.fn1("DAYOFWEEKISO", x),
                Part::Week => self.fn1("WEEKISO", x),
                Part::DayOfYear => self.fn1("DAYOFYEAR", x),
                other => self.wrap(
                    &format!("DATE_PART({}, ", part_upper(other).to_lowercase()),
                    |r| r.expr(x, true),
                    ")",
                ),
            },
            Dialect::Databricks => match part {
                Part::Weekday => self.wrap("(weekday(", |r| r.expr(x, true), ") + 1)"),
                Part::Week => self.fn1("weekofyear", x),
                Part::DayOfYear => self.fn1("dayofyear", x),
                Part::Second => self.wrap("FLOOR(extract(SECOND FROM ", |r| r.expr(x, true), "))"),
                other => self.wrap(
                    &format!("extract({} FROM ", part_upper(other)),
                    |r| r.expr(x, true),
                    ")",
                ),
            },
            Dialect::Redshift => match part {
                Part::Weekday => {
                    self.push("(CASE WHEN DATE_PART(dow, ");
                    self.expr(x, true)?;
                    self.push(") = 0 THEN 7 ELSE DATE_PART(dow, ");
                    self.expr(x, true)?;
                    self.push(") END)");
                    Ok(())
                }
                Part::Second => self.wrap("FLOOR(DATE_PART(second, ", |r| r.expr(x, true), "))"),
                _ => self.wrap(&format!("DATE_PART({std_name}, "), |r| r.expr(x, true), ")"),
            },
        }
    }

    fn trunc(&mut self, unit: Unit, x: &Expr) -> R {
        let d = self.d();
        match d {
            Dialect::BigQuery => {
                let u = if unit == Unit::Week {
                    "ISOWEEK".to_string()
                } else {
                    unit.name().to_uppercase()
                };
                self.wrap("DATETIME_TRUNC(", |r| r.expr(x, true), &format!(", {u})"))
            }
            Dialect::Databricks => self.wrap(
                &format!("date_trunc('{}', ", unit.name().to_uppercase()),
                |r| r.expr(x, true),
                ")",
            ),
            Dialect::Snowflake => self.wrap(
                &format!("DATE_TRUNC('{}', ", unit.name()),
                |r| r.expr(x, true),
                ")",
            ),
            _ => self.wrap(
                &format!("date_trunc('{}', ", unit.name()),
                |r| r.expr(x, true),
                ")",
            ),
        }
    }

    fn add(&mut self, value: &Expr, amount: i64, unit: Unit, kind: DateKind) -> R {
        let utc = self.timezone == Tz::UTC;
        if let Some(secs) = unit.seconds() {
            let total = amount * secs;
            return match kind {
                DateKind::Instant => self.add_seconds(value, total, true),
                _ if utc => self.add_seconds(value, total, false),
                _ => {
                    let mut inner = self.nested();
                    inner.params = std::mem::take(&mut self.params);
                    inner.add_seconds(
                        &Expr::ToInstant {
                            value: value.clone().boxed(),
                            kind,
                        },
                        total,
                        true,
                    )?;
                    self.params = std::mem::take(&mut inner.params);
                    let sql = inner.sql;
                    self.local_raw(&sql)
                }
            };
        }
        let months = match unit {
            Unit::Month => amount,
            Unit::Quarter => amount * 3,
            _ => amount * 12,
        };
        match kind {
            DateKind::Instant => {
                let wall = Expr::LocalTime {
                    value: value.clone().boxed(),
                    kind: DateKind::Instant,
                };
                let mut inner = self.nested();
                inner.params = std::mem::take(&mut self.params);
                inner.add_months(&wall, months)?;
                self.params = std::mem::take(&mut inner.params);
                let sql = inner.sql;
                self.instant_raw(&sql)
            }
            _ => self.add_months(value, months),
        }
    }

    fn local_raw(&mut self, sql: &str) -> R {
        let raw = Expr::Raw(sql.to_string());
        self.local(&raw, DateKind::Instant)
    }

    fn instant_raw(&mut self, sql: &str) -> R {
        let raw = Expr::Raw(sql.to_string());
        self.instant_of(&raw, DateKind::Local)
    }

    fn add_seconds(&mut self, value: &Expr, n: i64, instant: bool) -> R {
        let d = self.d();
        let utc = self.timezone == Tz::UTC;
        match d {
            Dialect::DuckDb if instant && utc => self.wrap(
                "CAST(CAST(",
                |r| r.expr(value, true),
                &format!(" AS TIMESTAMP) + INTERVAL '{n} seconds' AS TIMESTAMPTZ)"),
            ),
            Dialect::DuckDb | Dialect::Postgres => self.wrap(
                "(",
                |r| r.expr(value, true),
                &format!(" + INTERVAL '{n} seconds')"),
            ),
            Dialect::BigQuery if instant => self.wrap(
                "TIMESTAMP_ADD(",
                |r| r.expr(value, true),
                &format!(", INTERVAL {n} SECOND)"),
            ),
            Dialect::BigQuery => self.wrap(
                "DATETIME_ADD(DATETIME(",
                |r| r.expr(value, true),
                &format!("), INTERVAL {n} SECOND)"),
            ),
            Dialect::Snowflake | Dialect::Redshift => self.wrap(
                &format!("DATEADD(second, {n}, "),
                |r| r.expr(value, true),
                ")",
            ),
            Dialect::Databricks => self.wrap(
                &format!("timestampadd(SECOND, {n}, "),
                |r| r.expr(value, true),
                ")",
            ),
        }
    }

    fn add_months(&mut self, value: &Expr, n: i64) -> R {
        match self.d() {
            Dialect::DuckDb | Dialect::Postgres => self.wrap(
                "(",
                |r| r.expr(value, true),
                &format!(" + INTERVAL '{n} months')"),
            ),
            Dialect::BigQuery => self.wrap(
                "DATETIME_ADD(DATETIME(",
                |r| r.expr(value, true),
                &format!("), INTERVAL {n} MONTH)"),
            ),
            Dialect::Snowflake | Dialect::Redshift => self.wrap(
                &format!("DATEADD(month, {n}, "),
                |r| r.expr(value, true),
                ")",
            ),
            Dialect::Databricks => self.wrap(
                &format!("timestampadd(MONTH, {n}, "),
                |r| r.expr(value, true),
                ")",
            ),
        }
    }

    fn epoch(&mut self, value: &Expr, kind: DateKind) -> R {
        let d = self.d();
        let tz = self.tz();
        if let Expr::Time { micros, .. } = value {
            self.push(&(micros / 1000).to_string());
            return Ok(());
        }
        match (d, kind) {
            (Dialect::DuckDb, _) if self.timezone == Tz::UTC => {
                self.wrap("epoch_ms(CAST(", |r| r.expr(value, true), " AS TIMESTAMP))")
            }
            (Dialect::DuckDb, DateKind::Instant) => {
                self.wrap("epoch_ms(timezone('UTC', ", |r| r.expr(value, true), "))")
            }
            (Dialect::DuckDb, _) => self.wrap(
                &format!("epoch_ms(timezone('UTC', timezone({tz}, CAST("),
                |r| r.expr(value, true),
                " AS TIMESTAMP))))",
            ),
            (Dialect::Postgres, DateKind::Instant) => {
                self.wrap("(EXTRACT(EPOCH FROM ", |r| r.expr(value, true), ") * 1000)")
            }
            (Dialect::Postgres, _) => self.wrap(
                "(EXTRACT(EPOCH FROM (CAST(",
                |r| r.expr(value, true),
                &format!(" AS TIMESTAMP) AT TIME ZONE {tz})) * 1000)"),
            ),
            (Dialect::BigQuery, DateKind::Instant) => self.fn1("UNIX_MILLIS", value),
            (Dialect::BigQuery, _) => self.wrap(
                "UNIX_MILLIS(TIMESTAMP(",
                |r| r.expr(value, true),
                &format!(", {tz}))"),
            ),
            (Dialect::Snowflake, DateKind::Instant) => self.wrap(
                "DATE_PART(epoch_millisecond, ",
                |r| r.expr(value, true),
                ")",
            ),
            (Dialect::Snowflake, _) => self.wrap(
                &format!("DATE_PART(epoch_millisecond, CONVERT_TIMEZONE({tz}, 'UTC', "),
                |r| r.expr(value, true),
                "::TIMESTAMP_NTZ))",
            ),
            (Dialect::Databricks, DateKind::Instant) => self.fn1("unix_millis", value),
            (Dialect::Databricks, _) => self.wrap(
                "unix_millis(to_utc_timestamp(",
                |r| r.expr(value, true),
                &format!(", {tz}))"),
            ),
            (Dialect::Redshift, DateKind::Instant) => {
                self.wrap("(EXTRACT(EPOCH FROM ", |r| r.expr(value, true), ") * 1000)")
            }
            (Dialect::Redshift, _) => self.wrap(
                &format!("(EXTRACT(EPOCH FROM CONVERT_TIMEZONE({tz}, 'UTC', "),
                |r| r.expr(value, true),
                ")) * 1000)",
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn each(
        &mut self,
        kind: Each,
        list: &Expr,
        var: usize,
        item: &Ty,
        body: &Expr,
        fallible: bool,
        strict: bool,
    ) -> R {
        let d = self.d();
        if d == Dialect::Redshift {
            return Err(unsupported(d, "closures over arrays"));
        }
        let _ = strict;
        let null_list = matches!(d, Dialect::Postgres | Dialect::BigQuery);
        if !fallible && !null_list {
            return self.each_simple(kind, list, var, item, body);
        }
        self.push("(CASE WHEN ");
        self.expr(list, true)?;
        self.push(" IS NULL THEN NULL");
        if !fallible {
            self.push(" ELSE ");
            self.each_simple(kind, list, var, item, body)?;
            self.push(" END)");
            return Ok(());
        }
        match kind {
            Each::Some | Each::None | Each::All => {
                let decisive = if kind == Each::All { "NOT " } else { "" };
                let (hit, miss) = match kind {
                    Each::Some => ("TRUE", "FALSE"),
                    _ => ("FALSE", "TRUE"),
                };
                self.push(" ELSE (CASE ");
                self.first_code(list, var, body, decisive)?;
                self.push(&format!(
                    " WHEN 1 THEN {hit} WHEN 2 THEN NULL ELSE {miss} END)"
                ));
            }
            Each::Map if !fallible => {
                self.push(" ELSE ");
                self.each_plain(kind, list, var, item, body)?;
            }
            _ => {
                self.push(" WHEN ");
                self.any_null(list, var, body)?;
                self.push(" THEN NULL ELSE ");
                self.each_plain(kind, list, var, item, body)?;
            }
        }
        self.push(" END)");
        Ok(())
    }

    fn each_simple(&mut self, kind: Each, list: &Expr, var: usize, item: &Ty, body: &Expr) -> R {
        let d = self.d();
        let v = format!("_z{var}");
        let exists = |r: &mut Render, negate: bool| -> R {
            match d {
                Dialect::DuckDb | Dialect::Snowflake => {
                    let (len, filter, lambda) = if d == Dialect::DuckDb {
                        ("len", "list_filter", format!("lambda {v}: "))
                    } else {
                        ("ARRAY_SIZE", "FILTER", format!("{v} -> "))
                    };
                    r.push(&format!("({len}({filter}("));
                    r.expr(list, true)?;
                    r.push(&format!(", {lambda}"));
                    if negate {
                        r.push("NOT (");
                        r.expr(body, true)?;
                        r.push(")");
                    } else {
                        r.expr(body, false)?;
                    }
                    r.push(")) > 0)");
                    Ok(())
                }
                Dialect::Databricks => {
                    r.push("exists(");
                    r.expr(list, true)?;
                    r.push(&format!(", {v} -> "));
                    if negate {
                        r.push("NOT (");
                        r.expr(body, true)?;
                        r.push(")");
                    } else {
                        r.expr(body, false)?;
                    }
                    r.push(")");
                    Ok(())
                }
                Dialect::Postgres | Dialect::BigQuery => {
                    let unnest = if d == Dialect::BigQuery {
                        "UNNEST"
                    } else {
                        "unnest"
                    };
                    r.push(&format!("EXISTS (SELECT 1 FROM {unnest}("));
                    r.expr(list, true)?;
                    r.push(&format!(") AS {v} WHERE "));
                    if negate {
                        r.push("NOT (");
                        r.expr(body, true)?;
                        r.push(")");
                    } else {
                        r.expr(body, false)?;
                    }
                    r.push(")");
                    Ok(())
                }
                Dialect::Redshift => unreachable!(),
            }
        };
        match kind {
            Each::Some => exists(self, false),
            Each::None => {
                self.push("(NOT ");
                exists(self, false)?;
                self.push(")");
                Ok(())
            }
            Each::All => {
                self.push("(NOT ");
                exists(self, true)?;
                self.push(")");
                Ok(())
            }
            _ => self.each_plain(kind, list, var, item, body),
        }
    }

    fn code(&mut self, body: &Expr, decisive: &str) -> R {
        let (hit, miss) = if decisive.is_empty() {
            ("TRUE", "FALSE")
        } else {
            ("FALSE", "TRUE")
        };
        self.push("(CASE ");
        self.expr(body, true)?;
        self.push(&format!(
            " WHEN {hit} THEN 1 WHEN {miss} THEN 0 ELSE 2 END)"
        ));
        Ok(())
    }

    fn first_code(&mut self, list: &Expr, var: usize, body: &Expr, decisive: &str) -> R {
        let d = self.d();
        let v = format!("_z{var}");
        match d {
            Dialect::DuckDb => {
                self.push("list_filter(list_transform(");
                self.expr(list, true)?;
                self.push(&format!(", lambda {v}: "));
                self.code(body, decisive)?;
                self.push("), lambda _c: _c <> 0)[1]");
            }
            Dialect::Postgres => {
                self.push(&format!("(SELECT _c{var} FROM (SELECT _o{var}, "));
                self.code(body, decisive)?;
                self.push(&format!(" AS _c{var} FROM generate_subscripts("));
                self.expr(list, true)?;
                self.push(&format!(", 1) AS _o{var}, LATERAL (SELECT ("));
                self.expr(list, true)?;
                self.push(&format!(")[_o{var}] AS {v}) AS _t{var}) AS _s{var} WHERE _c{var} <> 0 ORDER BY _o{var} LIMIT 1)"));
            }
            Dialect::BigQuery => {
                self.push(&format!("(SELECT _c{var} FROM (SELECT _o{var}, "));
                self.code(body, decisive)?;
                self.push(&format!(" AS _c{var} FROM UNNEST("));
                self.expr(list, true)?;
                self.push(&format!(
                    ") AS {v} WITH OFFSET AS _o{var}) WHERE _c{var} <> 0 ORDER BY _o{var} LIMIT 1)"
                ));
            }
            Dialect::Snowflake => {
                self.push("GET(FILTER(TRANSFORM(");
                self.expr(list, true)?;
                self.push(&format!(", {v} -> "));
                self.code(body, decisive)?;
                self.push("), _c -> _c <> 0), 0)::INT");
            }
            Dialect::Databricks => {
                self.push("try_element_at(filter(transform(");
                self.expr(list, true)?;
                self.push(&format!(", {v} -> "));
                self.code(body, decisive)?;
                self.push("), _c -> _c <> 0), 1)");
            }
            Dialect::Redshift => unreachable!(),
        }
        Ok(())
    }

    fn any_null(&mut self, list: &Expr, var: usize, body: &Expr) -> R {
        let d = self.d();
        let v = format!("_z{var}");
        match d {
            Dialect::DuckDb => {
                self.push("(len(list_filter(");
                self.expr(list, true)?;
                self.push(&format!(", lambda {v}: ("));
                self.expr(body, true)?;
                self.push(") IS NULL)) > 0)");
            }
            Dialect::Postgres | Dialect::BigQuery => {
                let unnest = if d == Dialect::BigQuery {
                    "UNNEST"
                } else {
                    "unnest"
                };
                self.push(&format!("EXISTS (SELECT 1 FROM {unnest}("));
                self.expr(list, true)?;
                self.push(&format!(") AS {v} WHERE ("));
                self.expr(body, true)?;
                self.push(") IS NULL)");
            }
            Dialect::Snowflake => {
                self.push("(ARRAY_SIZE(FILTER(");
                self.expr(list, true)?;
                self.push(&format!(", {v} -> ("));
                self.expr(body, true)?;
                self.push(") IS NULL)) > 0)");
            }
            Dialect::Databricks => {
                self.push("exists(");
                self.expr(list, true)?;
                self.push(&format!(", {v} -> ("));
                self.expr(body, true)?;
                self.push(") IS NULL)");
            }
            Dialect::Redshift => unreachable!(),
        }
        Ok(())
    }

    fn each_plain(&mut self, kind: Each, list: &Expr, var: usize, _item: &Ty, body: &Expr) -> R {
        let d = self.d();
        let v = format!("_z{var}");
        match d {
            Dialect::DuckDb => {
                let f = if kind == Each::Map {
                    "list_transform"
                } else {
                    "list_filter"
                };
                let counted = matches!(kind, Each::One | Each::Count);
                if counted {
                    self.push("len(");
                }
                self.push(&format!("{f}("));
                self.expr(list, true)?;
                self.push(&format!(", lambda {v}: "));
                self.expr(body, kind == Each::Map)?;
                self.push(")");
                if counted {
                    self.push(")");
                }
            }
            Dialect::Postgres | Dialect::BigQuery => {
                let unnest = if d == Dialect::BigQuery {
                    "UNNEST"
                } else {
                    "unnest"
                };
                let ord = if d == Dialect::BigQuery {
                    format!(" WITH OFFSET AS _o{var}")
                } else {
                    String::new()
                };
                let order = if d == Dialect::BigQuery {
                    format!(" ORDER BY _o{var}")
                } else {
                    String::new()
                };
                match kind {
                    Each::Map => {
                        self.push("ARRAY(SELECT ");
                        self.expr(body, true)?;
                        self.push(&format!(" FROM {unnest}("));
                        self.expr(list, true)?;
                        self.push(&format!(") AS {v}{ord}{order})"));
                    }
                    Each::Filter => {
                        self.push(&format!("ARRAY(SELECT {v} FROM {unnest}("));
                        self.expr(list, true)?;
                        self.push(&format!(") AS {v}{ord} WHERE "));
                        self.expr(body, false)?;
                        self.push(&format!("{order})"));
                    }
                    _ => {
                        self.push(&format!("(SELECT COUNT(*) FROM {unnest}("));
                        self.expr(list, true)?;
                        self.push(&format!(") AS {v} WHERE "));
                        self.expr(body, false)?;
                        self.push(")");
                    }
                }
            }
            Dialect::Snowflake | Dialect::Databricks => {
                let (filter, transform, size) = if d == Dialect::Snowflake {
                    ("FILTER", "TRANSFORM", "ARRAY_SIZE")
                } else {
                    ("filter", "transform", "size")
                };
                let counted = matches!(kind, Each::One | Each::Count);
                if counted {
                    self.push(&format!("{size}("));
                }
                self.push(&format!(
                    "{}(",
                    if kind == Each::Map { transform } else { filter }
                ));
                self.expr(list, true)?;
                self.push(&format!(", {v} -> "));
                self.expr(body, kind == Each::Map)?;
                self.push(")");
                if counted {
                    self.push(")");
                }
            }
            Dialect::Redshift => unreachable!(),
        }
        if kind == Each::One {
            self.push(" = 1");
        }
        Ok(())
    }
}

fn part_upper(part: Part) -> &'static str {
    match part {
        Part::Second => "SECOND",
        Part::Minute => "MINUTE",
        Part::Hour => "HOUR",
        Part::Day => "DAY",
        Part::DayOfYear => "DAYOFYEAR",
        Part::Week => "WEEK",
        Part::Weekday => "DAYOFWEEK",
        Part::Month => "MONTH",
        Part::Quarter => "QUARTER",
        Part::Year => "YEAR",
    }
}
