use std::collections::HashMap;

use chrono::{Duration, NaiveTime, TimeZone};
use nohash_hasher::BuildNoHashHasher;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use zen_expression::functions::{
    ClosureFunction, DateMethod, DeprecatedFunction, FunctionKind, InternalFunction, MethodKind,
};
use zen_expression::lexer::{
    ArithmeticOperator, Bracket, ComparisonOperator, LogicalOperator, Operator,
};
use zen_expression::parser::{Node, NodeMetadata};

use crate::Error;
use crate::Options;
use crate::dialect::Dialect;
use crate::fold::{Printer, evaluate, is_const, is_trivial};
use crate::ir::{Arith, Base, Cmp, Each, Expr, Func, Part, Path, Scalar, Step, Unit};
use crate::schema::{DateKind, Schema, Source, Ty};

pub type Meta = HashMap<usize, NodeMetadata, BuildNoHashHasher<usize>>;

#[derive(Clone, Debug)]
pub struct Typed {
    pub expr: Expr,
    pub ty: Ty,
    pub nullable: bool,
    pub dated: bool,
    pub fallible: bool,
    pub errs: Option<Box<Expr>>,
}

impl Typed {
    fn new(expr: Expr, ty: Ty, nullable: bool) -> Typed {
        Typed {
            expr,
            ty,
            nullable,
            dated: false,
            fallible: false,
            errs: None,
        }
    }

    fn errs_when(mut self, errs: Option<Expr>) -> Typed {
        self.errs = errs.map(Expr::boxed);
        self
    }

    fn fails(mut self, fallible: bool) -> Typed {
        self.fallible = fallible;
        self
    }

    fn sql_null(&self) -> bool {
        self.nullable || self.fallible
    }

    fn dated(mut self, dated: bool) -> Typed {
        self.dated = dated;
        self
    }

    fn may_be_null(&self) -> bool {
        self.nullable || self.dated
    }

    fn bool(expr: Expr, nullable: bool) -> Typed {
        Typed::new(expr, Ty::Bool, nullable)
    }

    fn constant(b: bool) -> Typed {
        Typed::new(Expr::Bool(b), Ty::Bool, false)
    }

    fn number(expr: Expr, nullable: bool) -> Typed {
        Typed::new(expr, Ty::Number, nullable)
    }
}

struct Var {
    alias: Option<String>,
    id: usize,
    ty: Ty,
}

enum Seg {
    Name(String),
    Index(i64),
}

pub struct Lower<'a> {
    pub schema: &'a Schema,
    pub options: &'a Options,
    pub meta: Option<&'a Meta>,
    pub subject: Option<Typed>,
    vars: Vec<Var>,
    next_var: usize,
}

type R<T> = Result<T, Error>;

impl<'a> Lower<'a> {
    pub fn new(schema: &'a Schema, options: &'a Options, meta: Option<&'a Meta>) -> Lower<'a> {
        Lower {
            schema,
            options,
            meta,
            subject: None,
            vars: Vec::new(),
            next_var: 0,
        }
    }

    fn err(&self, node: &Node, message: impl Into<String>) -> Error {
        let span = self
            .meta
            .and_then(|m| m.get(&(node as *const Node as usize)))
            .map(|m| m.span);
        Error {
            message: message.into(),
            span,
        }
    }

    pub fn lower(&mut self, node: &Node) -> R<Typed> {
        if is_const(node)
            && !is_trivial(node)
            && !matches!(
                node,
                Node::Array(_) | Node::Interval { .. } | Node::Object(_)
            )
        {
            return self.fold(node);
        }
        match node {
            Node::Null => Ok(Typed::new(Expr::Null, Ty::Null, true)),
            Node::Bool(b) => Ok(Typed::constant(*b)),
            Node::Number(n) => Ok(Typed::number(Expr::Num(*n), false)),
            Node::String(s) => {
                self.text(node, s)?;
                Ok(Typed::new(Expr::Str(s.to_string()), Ty::String, false))
            }
            Node::Parenthesized(inner) => self.lower(inner),
            Node::TemplateString(parts) => {
                let mut pieces = Vec::new();
                let mut fallible = false;
                for part in parts.iter() {
                    match part {
                        Node::String(s) => {
                            self.text(part, s)?;
                            if !s.is_empty() {
                                pieces.push(Expr::Str(s.to_string()));
                            }
                        }
                        other => {
                            let t = self.lower(other)?;
                            let t = self.to_string(other, t)?;
                            fallible |= t.sql_null();
                            pieces.push(t.expr);
                        }
                    }
                }
                Ok(Typed::new(concat(pieces), Ty::String, false).fails(fallible))
            }
            Node::Identifier(_) | Node::Member { .. } | Node::Root | Node::Pointer => {
                self.path(node)
            }
            Node::Binary {
                left,
                operator,
                right,
            } => self.binary(node, left, *operator, right),
            Node::Unary {
                node: inner,
                operator,
            } => self.unary(node, inner, *operator),
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                let c = self.lower(condition)?;
                let c = self.boolean(condition, c)?;
                let t = self.lower(on_true)?;
                let f = self.lower(on_false)?;
                let (t, f) = self.unify(node, t, f)?;
                let nullable = t.nullable || f.nullable;
                let fallible = c.sql_null() || t.fallible || f.fallible;
                let errs = if nullable { error_when(&[&c]) } else { None };
                let ty = if t.ty == Ty::Null {
                    f.ty.clone()
                } else {
                    t.ty.clone()
                };
                Ok(Typed::new(
                    Expr::Case(c.expr.boxed(), t.expr.boxed(), f.expr.boxed()),
                    ty,
                    nullable,
                )
                .fails(fallible)
                .errs_when(errs))
            }
            Node::FunctionCall { kind, arguments } => self.function(node, kind, arguments),
            Node::MethodCall {
                kind,
                this,
                arguments,
            } => {
                let MethodKind::DateMethod(m) = kind;
                self.method(node, *m, this, arguments)
            }
            Node::Array(_) => Err(self.err(node, "arrays can only be used on the right of `in`")),
            Node::Interval { .. } => {
                Err(self.err(node, "intervals can only be used on the right of `in`"))
            }
            Node::Object(_) => Err(self.err(node, "objects cannot be used in a filter")),
            Node::Slice { .. } => Err(self.err(node, "slices are not supported in SQL filters")),
            Node::Assignments { .. } => {
                Err(self.err(node, "assignments are not supported in SQL filters"))
            }
            Node::Closure { .. } => Err(self.err(
                node,
                "closures can only be used inside some, all, none, one, count, filter and map",
            )),
            Node::Error { error, .. } => Err(self.err(node, error.to_string())),
        }
    }

    fn fold(&mut self, node: &Node) -> R<Typed> {
        let mut printer = Printer {
            timezone: self.options.timezone.name(),
            out: String::new(),
        };
        printer.print(node).map_err(|m| self.err(node, m))?;
        let folded = evaluate(&printer.out).map_err(|m| self.err(node, m))?;
        let nullable = folded.expr == Expr::Null;
        let dated = matches!(folded.ty, Ty::Date { .. });
        Ok(Typed::new(folded.expr, folded.ty, nullable).dated(dated))
    }

    fn path(&mut self, node: &Node) -> R<Typed> {
        let mut segs = Vec::new();
        let mut cur = node;
        loop {
            match cur {
                Node::Member {
                    node: inner,
                    property,
                } => {
                    segs.push(self.segment(property)?);
                    cur = inner;
                }
                Node::Parenthesized(inner) => cur = inner,
                _ => break,
            }
        }
        segs.reverse();
        match cur {
            Node::Identifier("$") => {
                let Some(subject) = self.subject.clone() else {
                    return Err(self.err(cur, "`$` is only available in unary expressions"));
                };
                let Expr::Path(path) = subject.expr else {
                    return Err(self.err(cur, "the unary subject is not a path"));
                };
                self.walk(node, path, subject.ty, subject.nullable, &segs)
            }
            Node::Identifier(name) if self.var(Some(name)).is_some() => {
                let (id, ty) = self.var(Some(name)).unwrap();
                let path = Path {
                    base: Base::Var(id),
                    base_ty: ty.clone(),
                    steps: Vec::new(),
                };
                self.walk(node, path, ty, true, &segs)
            }
            Node::Pointer => {
                let Some((id, ty)) = self.var(None) else {
                    return Err(self.err(cur, "`#` is only available inside closures"));
                };
                let path = Path {
                    base: Base::Var(id),
                    base_ty: ty.clone(),
                    steps: Vec::new(),
                };
                self.walk(node, path, ty, true, &segs)
            }
            Node::Identifier(name) => {
                segs.insert(0, Seg::Name(name.to_string()));
                self.root(node, &segs)
            }
            Node::Root => {
                if segs.is_empty() {
                    return Err(self.err(cur, "`$root` must be followed by a field"));
                }
                self.root(node, &segs)
            }
            _ => Err(self.err(node, "property access is only supported on data paths")),
        }
    }

    fn text(&self, node: &Node, s: &str) -> R<()> {
        if s.contains('\0') {
            return Err(self.err(node, "strings cannot contain NUL characters"));
        }
        Ok(())
    }

    fn name(&self, node: &Node, name: &str) -> R<()> {
        if name.is_empty() || !safe_name(name) {
            return Err(self.err(
                node,
                format!("`{name}` cannot be used as a name: names cannot be empty or contain quotes, backticks, backslashes or control characters"),
            ));
        }
        Ok(())
    }

    fn segment(&mut self, property: &Node) -> R<Seg> {
        let value = match property {
            Node::String(s) => {
                self.text(property, s)?;
                return Ok(Seg::Name(s.to_string()));
            }
            Node::Number(n) => Expr::Num(*n),
            other if is_const(other) => self.fold(other)?.expr,
            other => {
                return Err(self.err(
                    other,
                    "dynamic property access is not supported in SQL filters",
                ));
            }
        };
        match value {
            Expr::Str(s) if s.contains('\0') => {
                Err(self.err(property, "strings cannot contain NUL characters"))
            }
            Expr::Str(s) => Ok(Seg::Name(s)),
            Expr::Num(n) if n.fract().is_zero() => {
                let i = n
                    .to_i64()
                    .filter(|i| i.abs() <= i64::from(i32::MAX))
                    .ok_or_else(|| self.err(property, "the index is out of range"))?;
                Ok(Seg::Index(i))
            }
            _ => Err(self.err(property, "properties must be strings or whole numbers")),
        }
    }

    fn var(&self, alias: Option<&str>) -> Option<(usize, Ty)> {
        let found = match alias {
            Some(a) => self
                .vars
                .iter()
                .rev()
                .find(|v| v.alias.as_deref() == Some(a)),
            None => self.vars.last(),
        };
        found.map(|v| (v.id, v.ty.clone()))
    }

    fn root(&mut self, node: &Node, segs: &[Seg]) -> R<Typed> {
        let Some(Seg::Name(first)) = segs.first() else {
            return Err(self.err(node, "paths must start with a name"));
        };
        let fallback = self.schema.fallback();
        let named = self.schema.source(first).filter(|_| segs.len() > 1);
        if let Some((source, k)) = fallback.and_then(|f| longest(f, segs).map(|k| (f, k))) {
            if let Some(other) = named.filter(|n| n.name != source.name) {
                return Err(self.err(
                    node,
                    format!(
                        "`{first}` is both a source and a field of `{}`; write `{}.{first}` or rename the source `{}`",
                        source.name, source.name, other.name
                    ),
                ));
            }
            return self.column(node, source, segs, k);
        }
        if let Some(source) = named {
            let rest = &segs[1..];
            return match longest(source, rest) {
                Some(k) => self.column(node, source, rest, k),
                None => Err(self.unknown(node, source, rest)),
            };
        }
        match fallback {
            Some(source) => Err(self.unknown(node, source, segs)),
            None => {
                let names: Vec<_> = self
                    .schema
                    .sources
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect();
                Err(self.err(
                    node,
                    format!("unknown source `{first}`; available: {}", names.join(", ")),
                ))
            }
        }
    }

    fn unknown(&self, node: &Node, source: &Source, segs: &[Seg]) -> Error {
        let name = match segs.first() {
            Some(Seg::Name(n)) => n.clone(),
            Some(Seg::Index(i)) => format!("[{i}]"),
            None => {
                return self.err(
                    node,
                    format!("`{}` is a source, pick one of its fields", source.name),
                );
            }
        };
        let mut fields: Vec<_> = source.fields.iter().map(|f| f.name.as_str()).collect();
        fields.sort_unstable();
        self.err(
            node,
            format!(
                "unknown field `{name}` in `{}`; available: {}",
                source.name,
                fields.join(", ")
            ),
        )
    }

    fn column(&mut self, node: &Node, source: &Source, segs: &[Seg], k: usize) -> R<Typed> {
        let name = joined(&segs[..k]);
        let field = source.field(&name).unwrap();
        for name in [
            Some(field.name.as_str()),
            source.qualifier.as_deref(),
            Some(source.name.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            self.name(node, name)?;
        }
        let path = Path {
            base: Base::Column {
                qualifier: source
                    .qualifier
                    .clone()
                    .or_else(|| (self.schema.sources.len() > 1).then(|| source.name.clone())),
                source: source.name.clone(),
                name: field.name.clone(),
            },
            base_ty: field.ty.clone(),
            steps: Vec::new(),
        };
        self.walk(node, path, field.ty.clone(), field.nullable, &segs[k..])
    }

    fn walk(
        &mut self,
        node: &Node,
        mut path: Path,
        mut ty: Ty,
        mut nullable: bool,
        segs: &[Seg],
    ) -> R<Typed> {
        for seg in segs {
            let (step, next) = match (&ty, seg) {
                (Ty::Json | Ty::Any, Seg::Name(n)) => (Step::Field(n.clone()), Ty::Json),
                (Ty::Json | Ty::Any, Seg::Index(i)) => {
                    if *i < 0 {
                        return Err(self.err(node, "negative indexes are not supported"));
                    }
                    (Step::Index(*i), Ty::Json)
                }
                (Ty::Struct { fields }, Seg::Name(n)) => match fields.iter().find(|f| f.name == *n)
                {
                    Some(f) => (Step::Field(n.clone()), f.ty.clone()),
                    None => {
                        let mut names: Vec<_> = fields.iter().map(|f| f.name.as_str()).collect();
                        names.sort_unstable();
                        return Err(self.err(
                            node,
                            format!("unknown field `{n}`; available: {}", names.join(", ")),
                        ));
                    }
                },
                (Ty::Map { value }, Seg::Name(n)) => (Step::Key(n.clone()), (**value).clone()),
                (Ty::List { item }, Seg::Index(i)) => {
                    if *i < 0 {
                        return Err(self.err(node, "negative indexes are not supported"));
                    }
                    (Step::Index(*i), (**item).clone())
                }
                (Ty::List { .. }, Seg::Name(n)) => {
                    return Err(self.err(
                        node,
                        format!("`{n}` cannot be read from an array; use map() or an index"),
                    ));
                }
                (other, Seg::Name(n)) => {
                    return Err(self.err(
                        node,
                        format!("`{n}` cannot be read from a {}", other.name()),
                    ));
                }
                (other, Seg::Index(_)) => {
                    return Err(self.err(node, format!("a {} cannot be indexed", other.name())));
                }
            };
            if let Step::Field(n) | Step::Key(n) = &step {
                self.name(node, n)?;
            }
            path.steps.push((step, next.clone()));
            ty = next;
            nullable = true;
        }
        Ok(Typed::new(Expr::Path(path), ty, nullable))
    }

    fn open(&self, node: &Node, t: Typed, want: Scalar) -> R<Typed> {
        let Expr::Path(path) = t.expr else {
            return Err(self.err(node, "the type of this value is unknown"));
        };
        let (ty, nullable) = match want {
            Scalar::Str => (Ty::String, true),
            Scalar::Num => (Ty::Number, true),
            Scalar::Bool => (Ty::Bool, true),
            Scalar::Any => (Ty::String, true),
            Scalar::Text => (Ty::String, false),
            Scalar::Parse => (Ty::Number, false),
            Scalar::Container => (Ty::Bool, false),
        };
        let fallible = matches!(want, Scalar::Text | Scalar::Parse);
        Ok(Typed::new(Expr::Open { path, as_: want }, ty, nullable).fails(fallible))
    }

    fn boolean(&self, node: &Node, t: Typed) -> R<Typed> {
        match t.ty {
            Ty::Bool => Ok(t),
            Ty::Json | Ty::Any => self.open(node, t, Scalar::Bool),
            other => Err(self.err(node, format!("expected a boolean, got a {}", other.name()))),
        }
    }

    fn number(&self, node: &Node, t: Typed) -> R<Typed> {
        match t.ty {
            Ty::Number => Ok(t),
            Ty::Json | Ty::Any => self.open(node, t, Scalar::Num),
            other => Err(self.err(node, format!("expected a number, got a {}", other.name()))),
        }
    }

    fn string(&self, node: &Node, t: Typed) -> R<Typed> {
        match t.ty {
            Ty::String => Ok(t),
            Ty::Json | Ty::Any => self.open(node, t, Scalar::Str),
            other => Err(self.err(node, format!("expected a string, got a {}", other.name()))),
        }
    }

    fn list(&self, node: &Node, t: Typed) -> R<(Typed, Ty)> {
        match &t.ty {
            Ty::List { item } => {
                let item = (**item).clone();
                Ok((t, item))
            }
            Ty::Json | Ty::Any => {
                Err(self.err(node, "arrays inside JSON columns are not supported here"))
            }
            other => Err(self.err(node, format!("expected an array, got a {}", other.name()))),
        }
    }

    fn date(&mut self, node: &Node, t: Typed) -> R<Typed> {
        match &t.ty {
            Ty::Date { .. } => Ok(t),
            Ty::String if matches!(t.expr, Expr::Str(_)) => {
                let Expr::Str(s) = &t.expr else {
                    unreachable!()
                };
                self.parse_date(node, s)
            }
            Ty::Json | Ty::Any => {
                let s = self.open(node, t, Scalar::Str)?;
                Ok(Typed::new(
                    Expr::Parse {
                        value: s.expr.boxed(),
                        from: Ty::String,
                    },
                    Ty::instant(),
                    false,
                )
                .dated(true))
            }
            other => Err(self.err(
                node,
                format!("expected a date, got a {}; wrap it with d()", other.name()),
            )),
        }
    }

    fn parse_date(&self, node: &Node, text: &str) -> R<Typed> {
        let mut printer = Printer {
            timezone: self.options.timezone.name(),
            out: String::new(),
        };
        printer.out.push_str("d(");
        let lit = Node::String(text);
        printer.print(&lit).map_err(|m| self.err(node, m))?;
        printer.out.push_str(", ");
        printer
            .print(&Node::String(self.options.timezone.name()))
            .map_err(|m| self.err(node, m))?;
        printer.out.push(')');
        let folded = evaluate(&printer.out).map_err(|m| self.err(node, m))?;
        if !matches!(folded.expr, Expr::Time { .. }) {
            return Err(self.err(node, format!("`{text}` is not a valid date")));
        }
        Ok(Typed::new(folded.expr, folded.ty, false))
    }

    fn to_string(&self, node: &Node, t: Typed) -> R<Typed> {
        let text = match t.ty {
            Ty::String => t,
            Ty::Number | Ty::Bool => {
                let fallible = t.fallible;
                Typed::new(
                    Expr::Call(Func::ToString, vec![t.expr]),
                    Ty::String,
                    t.nullable,
                )
                .fails(fallible)
            }
            Ty::Json | Ty::Any => return self.open(node, t, Scalar::Text),
            Ty::Null => return Ok(Typed::new(Expr::Str("null".into()), Ty::String, false)),
            ref other => {
                return Err(self.err(
                    node,
                    format!("a {} cannot be turned into a string in SQL", other.name()),
                ));
            }
        };
        if text.nullable {
            let fallible = text.fallible;
            return Ok(Typed::new(
                Expr::Coalesce(text.expr.boxed(), Expr::Str("null".into()).boxed()),
                Ty::String,
                false,
            )
            .fails(fallible));
        }
        Ok(text)
    }

    fn unify(&self, node: &Node, a: Typed, b: Typed) -> R<(Typed, Typed)> {
        match (&a.ty, &b.ty) {
            (x, y) if x == y => Ok((a, b)),
            (Ty::Null, _) | (_, Ty::Null) => Ok((a, b)),
            (Ty::Date { .. }, Ty::Date { .. }) => Ok((a, b)),
            (Ty::Json | Ty::Any, Ty::Json | Ty::Any) => Err(self.err(
                node,
                "the types of both sides are unknown; wrap one with number(), string() or bool()",
            )),
            (Ty::Json | Ty::Any, other) => {
                let other = other.clone();
                Ok((self.coerce(node, a, &other)?, b))
            }
            (other, Ty::Json | Ty::Any) => {
                let other = other.clone();
                let b = self.coerce(node, b, &other)?;
                Ok((a, b))
            }
            (x, y) => Err(self.err(
                node,
                format!(
                    "both sides must have the same type, got {} and {}",
                    x.name(),
                    y.name()
                ),
            )),
        }
    }

    fn coerce(&self, node: &Node, t: Typed, want: &Ty) -> R<Typed> {
        match want {
            Ty::Bool => self.open(node, t, Scalar::Bool),
            Ty::Number => self.open(node, t, Scalar::Num),
            Ty::String => self.open(node, t, Scalar::Str),
            Ty::Date { .. } => {
                let s = self.open(node, t, Scalar::Str)?;
                Ok(Typed::new(
                    Expr::Parse {
                        value: s.expr.boxed(),
                        from: Ty::String,
                    },
                    Ty::instant(),
                    true,
                ))
            }
            other => Err(self.err(
                node,
                format!("cannot read a JSON value as {}", other.name()),
            )),
        }
    }

    fn binary(&mut self, node: &Node, left: &Node, op: Operator, right: &Node) -> R<Typed> {
        match op {
            Operator::Logical(LogicalOperator::And) | Operator::Logical(LogicalOperator::Or) => {
                let l = self.lower(left)?;
                let l = self.boolean(left, l)?;
                let r = self.lower(right)?;
                let r = self.boolean(right, r)?;
                let and = op == Operator::Logical(LogicalOperator::And);
                let guard = l.sql_null();
                let fallible = l.sql_null() || r.sql_null();
                let expr = match (l.expr, r.expr, and) {
                    (Expr::Bool(true), e, true) | (e, Expr::Bool(true), true) => e,
                    (Expr::Bool(false), _, true) => Expr::Bool(false),
                    (Expr::Bool(false), e, false) | (e, Expr::Bool(false), false) => e,
                    (Expr::Bool(true), _, false) => Expr::Bool(true),
                    (e, Expr::Bool(b), _) if guard => {
                        Expr::Case(e.boxed(), Expr::Bool(b).boxed(), Expr::Bool(b).boxed())
                    }
                    (_, Expr::Bool(b), _) => Expr::Bool(b),
                    (a, b, true) => Expr::And(a.boxed(), b.boxed(), guard),
                    (a, b, false) if guard => {
                        Expr::Case(a.boxed(), Expr::Bool(true).boxed(), b.boxed())
                    }
                    (a, b, false) => Expr::Or(a.boxed(), b.boxed()),
                };
                Ok(Typed::bool(expr, false).fails(fallible))
            }
            Operator::Logical(LogicalOperator::NullishCoalescing) => {
                let l = self.lower(left)?;
                let r = self.lower(right)?;
                if l.ty == Ty::Null {
                    return Ok(r);
                }
                if !l.nullable {
                    return Ok(l);
                }
                let (l, r) = self.unify(node, l, r)?;
                let ty = if l.ty == Ty::Null {
                    r.ty.clone()
                } else {
                    l.ty.clone()
                };
                let nullable = r.nullable;
                let fallible = l.fallible || r.fallible;
                let coalesced = Expr::Coalesce(l.expr.boxed(), r.expr.boxed());
                let expr = match l.errs {
                    Some(errs) => Expr::Case(errs, Expr::Null.boxed(), coalesced.boxed()),
                    None => coalesced,
                };
                Ok(Typed::new(expr, ty, nullable).fails(fallible))
            }
            Operator::Arithmetic(a) => {
                let l = self.lower(left)?;
                let r = self.lower(right)?;
                if a == ArithmeticOperator::Add {
                    let stringy = |t: &Ty| *t == Ty::String;
                    let numbery = |t: &Ty| *t == Ty::Number;
                    if stringy(&l.ty) || stringy(&r.ty) {
                        let l = self.string(left, l)?;
                        let r = self.string(right, r)?;
                        let fallible = l.sql_null() || r.sql_null();
                        return Ok(Typed::new(concat(vec![l.expr, r.expr]), Ty::String, false)
                            .fails(fallible));
                    }
                    if !(numbery(&l.ty) || numbery(&r.ty)) && l.ty.is_open() && r.ty.is_open() {
                        return Err(self.err(node, "cannot tell whether `+` adds numbers or joins strings; wrap a side with number() or string()"));
                    }
                }
                let l = self.number(left, l)?;
                let r = self.number(right, r)?;
                if a == ArithmeticOperator::Power && !matches!(r.expr, Expr::Num(_)) {
                    return Err(self.err(right, "the exponent of ^ must be a constant number"));
                }
                let op = match a {
                    ArithmeticOperator::Add => Arith::Add,
                    ArithmeticOperator::Subtract => Arith::Sub,
                    ArithmeticOperator::Multiply => Arith::Mul,
                    ArithmeticOperator::Divide => Arith::Div,
                    ArithmeticOperator::Modulus => Arith::Mod,
                    ArithmeticOperator::Power => Arith::Pow,
                };
                let fallible = l.sql_null() || r.sql_null();
                let safe_divisor = matches!(r.expr, Expr::Num(n) if !n.is_zero());
                let nullable = matches!(op, Arith::Div | Arith::Mod) && !safe_divisor;
                let errs = if nullable {
                    error_when(&[&l, &r])
                } else {
                    None
                };
                Ok(
                    Typed::number(Expr::Arith(op, l.expr.boxed(), r.expr.boxed()), nullable)
                        .fails(fallible)
                        .errs_when(errs),
                )
            }
            Operator::Comparison(c) => match c {
                ComparisonOperator::Equal => self.equal(node, left, right),
                ComparisonOperator::NotEqual => {
                    let eq = self.equal(node, left, right)?;
                    let fallible = eq.fallible;
                    Ok(Typed::bool(eq.expr.not(), false).fails(fallible))
                }
                ComparisonOperator::LessThan => self.compare(node, left, Cmp::Lt, right),
                ComparisonOperator::LessThanOrEqual => self.compare(node, left, Cmp::Le, right),
                ComparisonOperator::GreaterThan => self.compare(node, left, Cmp::Gt, right),
                ComparisonOperator::GreaterThanOrEqual => self.compare(node, left, Cmp::Ge, right),
                ComparisonOperator::In => self.within(node, left, right),
                ComparisonOperator::NotIn => {
                    let t = self.within(node, left, right)?;
                    let fallible = t.sql_null();
                    Ok(Typed::bool(t.expr.not(), false).fails(fallible))
                }
            },
            other => Err(self.err(
                node,
                format!("operator `{other}` is not supported in SQL filters"),
            )),
        }
    }

    fn equal(&mut self, node: &Node, left: &Node, right: &Node) -> R<Typed> {
        let l = self.lower(left)?;
        let r = self.lower(right)?;
        self.equal_typed(node, l, r)
    }

    fn equal_typed(&mut self, node: &Node, l: Typed, r: Typed) -> R<Typed> {
        match (&l.ty, &r.ty) {
            (Ty::Null, Ty::Null) => return Ok(Typed::constant(true)),
            (Ty::Null, _) => return self.is_null(node, r),
            (_, Ty::Null) => return self.is_null(node, l),
            _ => {}
        }
        if l.expr.is_literal() && r.expr.is_literal() {
            return Ok(Typed::constant(l.expr == r.expr));
        }
        let (l, r) = match (&l.ty, &r.ty) {
            (Ty::Date { .. }, Ty::String) => {
                let r = self.date(node, r)?;
                (l, r)
            }
            (Ty::String, Ty::Date { .. }) => {
                let l = self.date(node, l)?;
                (l, r)
            }
            (Ty::Json | Ty::Any, Ty::Json | Ty::Any) => {
                return Err(self.err(node, "the types of both sides are unknown; wrap one with number(), string() or bool()"));
            }
            (Ty::Json | Ty::Any, other) if other.is_scalar() => {
                let other = other.clone();
                (self.coerce(node, l, &other)?, r)
            }
            (other, Ty::Json | Ty::Any) if other.is_scalar() => {
                let other = other.clone();
                let r = self.coerce(node, r, &other)?;
                (l, r)
            }
            (a, b) if a.is_scalar() && b.is_scalar() => {
                let same = matches!((a, b), (Ty::Date { .. }, Ty::Date { .. })) || a == b;
                if !same {
                    return Ok(Typed::constant(false));
                }
                (l, r)
            }
            (a, b) => {
                return Err(self.err(
                    node,
                    format!("cannot compare a {} with a {} in SQL", a.name(), b.name()),
                ));
            }
        };
        let (l, r) = align_dates(l, r);
        let nullable = (l.may_be_null(), r.may_be_null());
        let fallible = l.fallible || r.fallible;
        let errs = match (l.errs.clone(), r.errs.clone()) {
            (Some(a), Some(b)) => Some(Expr::Or(a, b)),
            (Some(a), None) | (None, Some(a)) => Some(*a),
            (None, None) => None,
        };
        let eq = Expr::Eq {
            left: l.expr.boxed(),
            right: r.expr.boxed(),
            nullable,
        };
        let expr = match errs {
            Some(errs) => Expr::Case(errs.boxed(), Expr::Null.boxed(), eq.boxed()),
            None => eq,
        };
        Ok(Typed::bool(expr, false).fails(fallible))
    }

    fn is_null(&self, node: &Node, t: Typed) -> R<Typed> {
        if t.dated {
            return Ok(Typed::constant(false));
        }
        let t = if t.ty.is_open() {
            self.open(node, t, Scalar::Any)?
        } else {
            t
        };
        if let Some(errs) = t.errs.clone() {
            return Ok(Typed::bool(
                Expr::Case(
                    errs,
                    Expr::Null.boxed(),
                    Expr::IsNull(t.expr.boxed()).boxed(),
                ),
                false,
            )
            .fails(true));
        }
        if t.nullable {
            return Ok(Typed::bool(Expr::IsNull(t.expr.boxed()), false));
        }
        if t.expr.is_literal() || !t.fallible {
            return Ok(Typed::constant(false));
        }
        Ok(Typed::bool(
            Expr::Case(
                Expr::IsNull(t.expr.boxed()).boxed(),
                Expr::Null.boxed(),
                Expr::Bool(false).boxed(),
            ),
            false,
        ))
    }

    fn compare(&mut self, node: &Node, left: &Node, cmp: Cmp, right: &Node) -> R<Typed> {
        let l = self.lower(left)?;
        let r = self.lower(right)?;
        self.compare_typed(node, l, cmp, r, false)
    }

    fn compare_typed(
        &mut self,
        node: &Node,
        l: Typed,
        cmp: Cmp,
        r: Typed,
        method: bool,
    ) -> R<Typed> {
        let (l, r) = match (&l.ty, &r.ty) {
            (Ty::Number, Ty::Number) => (l, r),
            (Ty::Date { .. }, Ty::Date { .. }) => (l, r),
            (Ty::Date { .. }, Ty::String | Ty::Json | Ty::Any) => {
                let r = self.date(node, r)?;
                (l, r)
            }
            (Ty::String | Ty::Json | Ty::Any, Ty::Date { .. }) => {
                let l = self.date(node, l)?;
                (l, r)
            }
            (Ty::Json | Ty::Any, Ty::Number) => (self.number(node, l)?, r),
            (Ty::Number, Ty::Json | Ty::Any) => {
                let r = self.number(node, r)?;
                (l, r)
            }
            (Ty::Null, _) | (_, Ty::Null) => {
                return Err(self.err(node, "null cannot be ordered"));
            }
            (Ty::String, _) | (_, Ty::String) => {
                return Err(self.err(
                    node,
                    "ZEN cannot order strings; compare them with == or use startsWith()",
                ));
            }
            (a, b) => {
                return Err(self.err(
                    node,
                    format!("cannot order a {} and a {}", a.name(), b.name()),
                ));
            }
        };
        let any_dated = l.dated || r.dated;
        let both_dated = l.dated && r.dated;
        let (l, r) = align_dates(l, r);
        let nullable = l.nullable || r.nullable;
        if method && any_dated {
            let c = Expr::Cmp(cmp, l.expr.boxed(), r.expr.boxed());
            return Ok(Typed::bool(Expr::BoolOf(c.boxed()), false));
        }
        if both_dated {
            return Ok(Typed::bool(invalid_first(cmp, l.expr, r.expr), false));
        }
        let _ = nullable;
        let fallible = l.sql_null() || r.sql_null();
        Ok(Typed::bool(Expr::Cmp(cmp, l.expr.boxed(), r.expr.boxed()), false).fails(fallible))
    }

    fn within(&mut self, node: &Node, left: &Node, right: &Node) -> R<Typed> {
        let mut right = right;
        while let Node::Parenthesized(inner) = right {
            right = inner;
        }
        let item = self.lower(left)?;
        match right {
            Node::Array(items) => {
                let mut list = Vec::new();
                for it in items.iter() {
                    list.push((it, self.lower(it)?));
                }
                if item.ty.is_open() {
                    let want = list
                        .iter()
                        .map(|(_, t)| t.ty.clone())
                        .find(|t| t.is_scalar());
                    let Some(want) = want else {
                        return Err(self.err(node, "cannot tell the type of the left side of `in`; wrap it with number() or string()"));
                    };
                    let item = self.coerce(left, item, &want)?;
                    return self.in_list(node, item, list);
                }
                self.in_list(node, item, list)
            }
            Node::Interval {
                left: low,
                right: high,
                left_bracket,
                right_bracket,
            } => {
                let lo = self.lower(low)?;
                let hi = self.lower(high)?;
                let low_open = *left_bracket == Bracket::LeftParenthesis;
                let high_open = *right_bracket == Bracket::RightParenthesis;
                let definite = item.dated;
                let a = self.compare_typed(
                    node,
                    item.clone(),
                    if low_open { Cmp::Gt } else { Cmp::Ge },
                    lo,
                    true,
                )?;
                let b = self.compare_typed(
                    node,
                    item,
                    if high_open { Cmp::Lt } else { Cmp::Le },
                    hi,
                    true,
                )?;
                let unwrap = |e: Expr| match e {
                    Expr::BoolOf(inner) => *inner,
                    other => other,
                };
                let (Expr::Cmp(_, item, lo), Expr::Cmp(_, _, hi)) =
                    (unwrap(a.expr), unwrap(b.expr))
                else {
                    unreachable!()
                };
                let range = Expr::Range {
                    item,
                    low: lo,
                    high: hi,
                    low_open,
                    high_open,
                };
                if definite {
                    return Ok(Typed::bool(Expr::BoolOf(range.boxed()), false));
                }
                let fallible = a.fallible || b.fallible;
                Ok(Typed::bool(range, false).fails(fallible))
            }
            other => {
                let arr = self.lower(other)?;
                match &arr.ty {
                    Ty::List { item: it } => {
                        let it = (**it).clone();
                        let item = if item.ty.is_open() {
                            self.coerce(left, item, &it)?
                        } else {
                            item
                        };
                        if item.ty.is_scalar() && it.is_scalar() && !same_family(&item.ty, &it) {
                            return Ok(Typed::constant(false));
                        }
                        let fallible = arr.sql_null() || item.fallible;
                        Ok(Typed::bool(
                            Expr::InArray {
                                item: item.expr.boxed(),
                                array: arr.expr.boxed(),
                            },
                            false,
                        )
                        .fails(fallible))
                    }
                    Ty::String => {
                        Err(self.err(node, "ZEN does not support `in` on strings; use contains()"))
                    }
                    other => Err(self.err(
                        node,
                        format!("`in` needs an array or an interval, got a {}", other.name()),
                    )),
                }
            }
        }
    }

    fn in_list(&mut self, node: &Node, item: Typed, list: Vec<(&&Node, Typed)>) -> R<Typed> {
        let mut values = Vec::new();
        let mut has_null = false;
        for (n, t) in list {
            match &t.ty {
                Ty::Null if item.dated => {}
                Ty::Null => has_null = true,
                Ty::String if matches!(item.ty, Ty::Date { .. }) => values.push(self.date(n, t)?),
                ty if ty.is_scalar() && item.ty.is_scalar() => {
                    if same_family(ty, &item.ty) {
                        values.push(t);
                    }
                }
                Ty::Json | Ty::Any if item.ty.is_scalar() => {
                    let want = item.ty.clone();
                    values.push(self.coerce(n, t, &want)?);
                }
                other => {
                    return Err(self.err(
                        node,
                        format!(
                            "cannot look for a {} in a list of {}",
                            item.ty.name(),
                            other.name()
                        ),
                    ));
                }
            }
        }
        let mut parts = Vec::new();
        let fallible = item.fallible || values.iter().any(|v| v.fallible);
        let container = match &item.expr {
            Expr::Open { path, .. } => Some(Expr::Open {
                path: path.clone(),
                as_: Scalar::Container,
            }),
            _ => None,
        };
        if has_null {
            parts.push(self.is_null(node, item.clone())?.expr);
        }
        let pairwise = item.may_be_null()
            && values
                .iter()
                .any(|v| v.may_be_null() && !v.expr.is_literal());
        if pairwise {
            for v in values.drain(..) {
                parts.push(self.equal_typed(node, item.clone(), v)?.expr);
            }
        }
        if values.len() == 1 {
            let only = values.pop().unwrap();
            parts.push(self.equal_typed(node, item, only)?.expr);
        } else if !values.is_empty() {
            let nullable = item.may_be_null() || values.iter().any(|v| v.may_be_null());
            let kind = match &item.ty {
                Ty::Date { kind } => Some(*kind),
                _ => None,
            };
            let list = values
                .into_iter()
                .map(|v| match (v.expr, kind) {
                    (Expr::Time { micros, .. }, Some(k)) => Expr::Time { micros, kind: k },
                    (e, _) => e,
                })
                .collect();
            parts.push(Expr::In {
                item: item.expr.boxed(),
                list,
                nullable,
            });
        }
        let mut parts = parts.into_iter();
        let Some(first) = parts.next() else {
            return Ok(Typed::constant(false));
        };
        let expr = parts.fold(first, |a, b| Expr::Or(a.boxed(), b.boxed()));
        let expr = match container {
            Some(c) => Expr::Case(c.boxed(), Expr::Null.boxed(), expr.boxed()),
            None => expr,
        };
        Ok(Typed::bool(expr, false).fails(fallible))
    }

    fn unary(&mut self, node: &Node, inner: &Node, op: Operator) -> R<Typed> {
        let t = self.lower(inner)?;
        match op {
            Operator::Logical(LogicalOperator::Not) => {
                let t = self.boolean(inner, t)?;
                let fallible = t.sql_null();
                Ok(Typed::bool(t.expr.not(), false).fails(fallible))
            }
            Operator::Arithmetic(ArithmeticOperator::Subtract) => {
                let t = self.number(inner, t)?;
                let fallible = t.sql_null();
                Ok(Typed::number(Expr::Neg(t.expr.boxed()), false).fails(fallible))
            }
            Operator::Arithmetic(ArithmeticOperator::Add) => self.number(inner, t),
            other => Err(self.err(
                node,
                format!("operator `{other}` is not supported in SQL filters"),
            )),
        }
    }

    fn function(&mut self, node: &Node, kind: &FunctionKind, args: &[&Node]) -> R<Typed> {
        match kind {
            FunctionKind::Internal(f) => self.internal(node, *f, args),
            FunctionKind::Closure(c) => self.closure(node, *c, args),
            FunctionKind::Deprecated(d) => Err(self.err(
                node,
                format!(
                    "{} is deprecated and not supported in SQL filters; use d() and its methods",
                    deprecated_name(*d)
                ),
            )),
        }
    }

    fn args(&mut self, node: &Node, args: &[&Node], min: usize, max: usize) -> R<Vec<Typed>> {
        if args.len() < min || args.len() > max {
            return Err(self.err(
                node,
                format!("expected {min}–{max} arguments, got {}", args.len()),
            ));
        }
        args.iter().map(|a| self.lower(a)).collect()
    }

    fn internal(&mut self, node: &Node, f: InternalFunction, args: &[&Node]) -> R<Typed> {
        use InternalFunction as F;
        match f {
            F::Len => {
                let mut a = self.args(node, args, 1, 1)?;
                let x = a.remove(0);
                let fallible = x.sql_null();
                match &x.ty {
                    Ty::String => Ok(Typed::number(Expr::Call(Func::StrLen, vec![x.expr]), false)
                        .fails(fallible)),
                    Ty::List { .. } => Ok(Typed::number(
                        Expr::Call(Func::ListLen, vec![x.expr]),
                        false,
                    )
                    .fails(fallible)),
                    Ty::Json | Ty::Any => {
                        Err(self.err(node, "len() of a JSON value needs string() around it"))
                    }
                    other => Err(self.err(
                        node,
                        format!("len() needs a string or an array, got a {}", other.name()),
                    )),
                }
            }
            F::Contains => {
                let mut a = self.args(node, args, 2, 2)?;
                let needle = a.pop().unwrap();
                let hay = a.pop().unwrap();
                match &hay.ty {
                    Ty::String | Ty::Json | Ty::Any if needle.ty != Ty::Number => {
                        let hay = self.string(args[0], hay)?;
                        let needle = self.string(args[1], needle)?;
                        let fallible = hay.sql_null() || needle.sql_null();
                        Ok(Typed::bool(
                            Expr::Call(Func::StrContains, vec![hay.expr, needle.expr]),
                            false,
                        )
                        .fails(fallible))
                    }
                    Ty::List { item } => {
                        let it = (**item).clone();
                        let needle = if needle.ty.is_open() {
                            self.coerce(args[1], needle, &it)?
                        } else {
                            needle
                        };
                        if needle.ty == Ty::Null {
                            return Err(self.err(
                                node,
                                "contains() with null is not supported in SQL filters",
                            ));
                        }
                        if needle.ty.is_scalar() && it.is_scalar() && !same_family(&needle.ty, &it)
                        {
                            return Ok(Typed::constant(false));
                        }
                        let fallible = hay.sql_null() || needle.fallible;
                        Ok(Typed::bool(
                            Expr::InArray {
                                item: needle.expr.boxed(),
                                array: hay.expr.boxed(),
                            },
                            false,
                        )
                        .fails(fallible))
                    }
                    other => Err(self.err(
                        node,
                        format!(
                            "contains() needs a string or an array, got a {}",
                            other.name()
                        ),
                    )),
                }
            }
            F::Upper | F::Lower | F::Trim => {
                let mut a = self.args(node, args, 1, 1)?;
                let x = self.string(args[0], a.remove(0))?;
                let fallible = x.sql_null();
                let func = match f {
                    F::Upper => Func::Upper,
                    F::Lower => Func::Lower,
                    _ => Func::Trim,
                };
                Ok(Typed::new(Expr::Call(func, vec![x.expr]), Ty::String, false).fails(fallible))
            }
            F::StartsWith | F::EndsWith | F::Matches => {
                let mut a = self.args(node, args, 2, 2)?;
                let p = self.string(args[1], a.pop().unwrap())?;
                let s = self.string(args[0], a.pop().unwrap())?;
                if f == F::Matches {
                    self.pattern(args[1], &p.expr)?;
                }
                let func = match f {
                    F::StartsWith => Func::StartsWith,
                    F::EndsWith => Func::EndsWith,
                    _ => Func::Matches,
                };
                let fallible = s.sql_null() || p.sql_null();
                Ok(Typed::bool(Expr::Call(func, vec![s.expr, p.expr]), false).fails(fallible))
            }
            F::Abs | F::Floor | F::Ceil | F::Trunc => {
                let mut a = self.args(node, args, 1, 1)?;
                let x = self.number(args[0], a.remove(0))?;
                let fallible = x.sql_null();
                let func = match f {
                    F::Abs => Func::Abs,
                    F::Floor => Func::Floor,
                    F::Ceil => Func::Ceil,
                    _ => Func::Trunc,
                };
                Ok(Typed::number(Expr::Call(func, vec![x.expr]), false).fails(fallible))
            }
            F::Round => {
                let mut a = self.args(node, args, 1, 2)?;
                let digits = if a.len() == 2 {
                    let d = a.pop().unwrap();
                    match d.expr {
                        Expr::Num(n)
                            if n.fract().is_zero()
                                && !n.is_sign_negative()
                                && n <= Decimal::from(28) =>
                        {
                            Expr::Num(n)
                        }
                        _ => {
                            return Err(self.err(
                                args[1],
                                "round() needs a constant whole number of digits between 0 and 28",
                            ));
                        }
                    }
                } else {
                    Expr::Num(Decimal::ZERO)
                };
                let x = self.number(args[0], a.remove(0))?;
                let fallible = x.sql_null();
                Ok(
                    Typed::number(Expr::Call(Func::Round, vec![x.expr, digits]), false)
                        .fails(fallible),
                )
            }
            F::Sum | F::Avg | F::Min | F::Max | F::Median => {
                let mut a = self.args(node, args, 1, 1)?;
                let (x, item) = self.list(args[0], a.remove(0))?;
                if item != Ty::Number {
                    return Err(self.err(node, format!("{f}() needs an array of numbers")));
                }
                let func = match f {
                    F::Sum => Func::Sum,
                    F::Avg => Func::Avg,
                    F::Min => Func::Min,
                    F::Max => Func::Max,
                    _ => Func::Median,
                };
                Ok(Typed::number(Expr::Call(func, vec![x.expr]), false).fails(true))
            }
            F::IsNumeric => {
                let mut a = self.args(node, args, 1, 1)?;
                let x = a.remove(0);
                match &x.ty {
                    Ty::Number => Ok(Typed::bool(Expr::IsNull(x.expr.boxed()).not(), false)),
                    Ty::String => Ok(Typed::bool(
                        Expr::Call(Func::IsNumeric, vec![x.expr]),
                        false,
                    )),
                    Ty::Json | Ty::Any => Err(self.err(
                        node,
                        "isNumeric() of a JSON value is not supported; use number() or string()",
                    )),
                    _ => Ok(Typed::constant(false)),
                }
            }
            F::String => {
                let mut a = self.args(node, args, 1, 1)?;
                let x = a.remove(0);
                self.to_string(args[0], x)
            }
            F::Number => {
                let mut a = self.args(node, args, 1, 1)?;
                let x = a.remove(0);
                match &x.ty {
                    Ty::Number => Ok(x),
                    Ty::String => Ok(Typed::number(
                        Expr::Call(Func::ToNumber, vec![x.expr]),
                        false,
                    )
                    .fails(true)),
                    Ty::Bool => {
                        let fallible = x.sql_null();
                        Ok(Typed::number(
                            Expr::Case(
                                x.expr.boxed(),
                                Expr::Num(Decimal::ONE).boxed(),
                                Expr::Num(Decimal::ZERO).boxed(),
                            ),
                            false,
                        )
                        .fails(fallible))
                    }
                    Ty::Json | Ty::Any => self.open(args[0], x, Scalar::Parse),
                    other => Err(self.err(
                        node,
                        format!(
                            "number() of a {} is not supported in SQL filters",
                            other.name()
                        ),
                    )),
                }
            }
            F::Bool => {
                let mut a = self.args(node, args, 1, 1)?;
                let x = a.remove(0);
                match &x.ty {
                    Ty::Bool if x.fallible && !x.nullable => Ok(x),
                    Ty::Bool => {
                        let fallible = x.fallible;
                        Ok(Typed::bool(Expr::BoolOf(x.expr.boxed()), false).fails(fallible))
                    }
                    Ty::Null => Ok(Typed::constant(false)),
                    Ty::Json | Ty::Any => Err(self.err(
                        node,
                        "bool() of a JSON value is not supported; compare it with true or false",
                    )),
                    other => Err(self.err(
                        node,
                        format!(
                            "bool() of a {} is not supported in SQL filters",
                            other.name()
                        ),
                    )),
                }
            }
            F::Date => {
                if args.len() == 2 {
                    return Err(self.err(node, "d() with a timezone is only supported on constant values; the filter timezone applies to data"));
                }
                let mut a = self.args(node, args, 1, 1)?;
                let x = a.remove(0);
                match &x.ty {
                    Ty::Date { .. } => Ok(Typed {
                        nullable: false,
                        ..x
                    }
                    .dated(true)),
                    Ty::String | Ty::Number => {
                        let from = x.ty.clone();
                        Ok(Typed::new(
                            Expr::Parse {
                                value: x.expr.boxed(),
                                from,
                            },
                            Ty::instant(),
                            false,
                        )
                        .dated(true))
                    }
                    Ty::Json | Ty::Any => self.date(args[0], x),
                    other => {
                        Err(self.err(node, format!("d() of a {} is not supported", other.name())))
                    }
                }
            }
            F::Rand => Err(self.err(node, "rand() is not supported in SQL filters")),
            other => Err(self.err(node, format!("{other}() is not supported in SQL filters"))),
        }
    }

    fn pattern(&self, node: &Node, pattern: &Expr) -> R<()> {
        let Expr::Str(p) = pattern else {
            return Err(self.err(node, "matches() needs a constant pattern"));
        };
        let limit = self.options.limits.pattern;
        if p.len() > limit {
            return Err(self.err(
                node,
                format!("the pattern is longer than {limit} characters"),
            ));
        }
        let hir = regex_syntax::ParserBuilder::new()
            .nest_limit(32)
            .build()
            .parse(p)
            .map_err(|e| self.err(node, format!("invalid pattern: {e}")))?;
        let backtracking = !matches!(self.options.dialect, Dialect::DuckDb | Dialect::BigQuery);
        if backtracking && risky(&hir, false) {
            return Err(self.err(
                node,
                format!(
                    "the pattern repeats a group that itself repeats or branches, which can run for a very long time on {}",
                    self.options.dialect.name()
                ),
            ));
        }
        Ok(())
    }

    fn closure(&mut self, node: &Node, c: ClosureFunction, args: &[&Node]) -> R<Typed> {
        if args.len() != 2 {
            return Err(self.err(node, format!("{c}() takes an array and a closure")));
        }
        let kind = match c {
            ClosureFunction::All => Each::All,
            ClosureFunction::None => Each::None,
            ClosureFunction::Some => Each::Some,
            ClosureFunction::One => Each::One,
            ClosureFunction::Filter => Each::Filter,
            ClosureFunction::Map => Each::Map,
            ClosureFunction::Count => Each::Count,
            ClosureFunction::FlatMap => {
                return Err(self.err(node, "flatMap() is not supported in SQL filters"));
            }
        };
        let Node::Closure { body, alias } = args[1] else {
            return Err(self.err(args[1], "expected a closure"));
        };
        let arr = self.lower(args[0])?;
        let (arr, item) = self.list(args[0], arr)?;
        let id = self.next_var;
        self.next_var += 1;
        self.vars.push(Var {
            alias: alias.map(str::to_string),
            id,
            ty: item.clone(),
        });
        let result = self.lower(body);
        self.vars.pop();
        let body_t = result?;
        let (body_t, ty) = match kind {
            Each::Map => {
                if !body_t.ty.is_scalar() {
                    return Err(
                        self.err(body, "map() must return a number, string, boolean or date")
                    );
                }
                let ty = Ty::list(body_t.ty.clone());
                (body_t, ty)
            }
            Each::Filter => (self.boolean(body, body_t)?, Ty::list(item.clone())),
            Each::Count => (self.boolean(body, body_t)?, Ty::Number),
            _ => (self.boolean(body, body_t)?, Ty::Bool),
        };
        Ok(Typed::new(
            Expr::Each {
                kind,
                list: arr.expr.boxed(),
                var: id,
                item,
                fallible: body_t.sql_null(),
                body: body_t.expr.boxed(),
            },
            ty,
            false,
        )
        .fails(true))
    }

    fn method(&mut self, node: &Node, m: DateMethod, this: &Node, args: &[&Node]) -> R<Typed> {
        use DateMethod as M;
        let t = self.lower(this)?;
        let t = self.date(this, t)?;
        let kind = match &t.ty {
            Ty::Date { kind } => *kind,
            _ => unreachable!(),
        };
        let gettable = t.nullable || t.dated;
        let part =
            |p: Part| Typed::number(Expr::DatePart(p, local(t.clone(), kind).boxed()), gettable);
        match m {
            M::Second => Ok(part(Part::Second)),
            M::Minute => Ok(part(Part::Minute)),
            M::Hour => Ok(part(Part::Hour)),
            M::Day => Ok(part(Part::Day)),
            M::DayOfYear => Ok(part(Part::DayOfYear)),
            M::Week => Ok(part(Part::Week)),
            M::Weekday => Ok(part(Part::Weekday)),
            M::Month => Ok(part(Part::Month)),
            M::Quarter => Ok(part(Part::Quarter)),
            M::Year => Ok(part(Part::Year)),
            M::Timestamp => Ok(Typed::number(
                Expr::Epoch {
                    value: t.expr.boxed(),
                    kind,
                },
                gettable,
            )),
            M::IsValid => Ok(Typed::bool(Expr::IsNull(t.expr.boxed()).not(), false)),
            M::Add | M::Sub => {
                let (amount, unit) = self.duration(node, args)?;
                let amount = if m == M::Sub { -amount } else { amount };
                let added = Expr::DateAdd {
                    value: t.expr.clone().boxed(),
                    amount,
                    unit,
                    kind,
                };
                let expr = if unit == Unit::Year && amount != 0 {
                    let wall = local(t.clone(), kind);
                    let leap_day = Expr::And(
                        Expr::Eq {
                            left: Expr::DatePart(Part::Month, wall.clone().boxed()).boxed(),
                            right: Expr::Num(Decimal::from(2)).boxed(),
                            nullable: (false, false),
                        }
                        .boxed(),
                        Expr::Eq {
                            left: Expr::DatePart(Part::Day, wall.boxed()).boxed(),
                            right: Expr::Num(Decimal::from(29)).boxed(),
                            nullable: (false, false),
                        }
                        .boxed(),
                        false,
                    );
                    Expr::Case(leap_day.boxed(), Expr::Null.boxed(), added.boxed())
                } else {
                    added
                };
                Ok(Typed::new(expr, t.ty.clone(), t.nullable).dated(t.dated))
            }
            M::StartOf => {
                let unit = self.unit(node, args.first().copied())?;
                Ok(Typed::new(
                    Expr::DateTrunc(unit, local(t.clone(), kind).boxed()),
                    Ty::local(),
                    t.nullable,
                )
                .dated(t.dated))
            }
            M::Diff => {
                if args.is_empty() || args.len() > 2 {
                    return Err(self.err(node, "diff() takes a date and an optional unit"));
                }
                let other = self.lower(args[0])?;
                let other = self.date(args[0], other)?;
                let okind = match &other.ty {
                    Ty::Date { kind } => *kind,
                    _ => unreachable!(),
                };
                let nullable = t.may_be_null() || other.may_be_null();
                let millis = Expr::Arith(
                    Arith::Sub,
                    Expr::Epoch {
                        value: t.expr.boxed(),
                        kind,
                    }
                    .boxed(),
                    Expr::Epoch {
                        value: other.expr.boxed(),
                        kind: okind,
                    }
                    .boxed(),
                );
                if args.len() == 1 {
                    return Ok(Typed::number(millis, nullable));
                }
                let unit = self.unit(node, Some(args[1]))?;
                let Some(secs) = unit.seconds() else {
                    return Err(self.err(node, format!("diff() in {}s is not supported in SQL filters; use second, minute, hour, day or week", unit.name())));
                };
                Ok(Typed::number(
                    Expr::Call(
                        Func::Trunc,
                        vec![Expr::Arith(
                            Arith::Div,
                            millis.boxed(),
                            Expr::Num(Decimal::from(secs * 1000)).boxed(),
                        )],
                    ),
                    nullable,
                ))
            }
            M::IsSame | M::IsBefore | M::IsAfter | M::IsSameOrBefore | M::IsSameOrAfter => {
                if args.is_empty() || args.len() > 2 {
                    return Err(self.err(node, format!("{m}() takes a date and an optional unit")));
                }
                let other = self.lower(args[0])?;
                let other = self.date(args[0], other)?.dated(true);
                let (l, r) = if args.len() == 2 {
                    let unit = self.unit(node, Some(args[1]))?;
                    let okind = match &other.ty {
                        Ty::Date { kind } => *kind,
                        _ => unreachable!(),
                    };
                    let l = Typed::new(
                        Expr::DateTrunc(unit, local(t.clone(), kind).boxed()),
                        Ty::local(),
                        t.nullable,
                    )
                    .dated(t.dated);
                    let r = self
                        .truncate_const(node, other.clone(), unit, okind)?
                        .dated(other.dated);
                    (l, r)
                } else {
                    (t, other)
                };
                match m {
                    M::IsSame => {
                        let (l, r) = align_dates(l, r);
                        let eq = Expr::Eq {
                            left: l.expr.boxed(),
                            right: r.expr.boxed(),
                            nullable: (false, false),
                        };
                        Ok(Typed::bool(Expr::BoolOf(eq.boxed()), false))
                    }
                    M::IsBefore => self.compare_typed(node, l, Cmp::Lt, r, true),
                    M::IsAfter => self.compare_typed(node, l, Cmp::Gt, r, true),
                    M::IsSameOrBefore => self.compare_typed(node, l, Cmp::Le, r, true),
                    _ => self.compare_typed(node, l, Cmp::Ge, r, true),
                }
            }
            M::IsToday | M::IsYesterday | M::IsTomorrow => {
                let tz = self.options.timezone;
                let today = chrono::Utc::now().with_timezone(&tz).date_naive();
                let day = match m {
                    M::IsYesterday => today - Duration::days(1),
                    M::IsTomorrow => today + Duration::days(1),
                    _ => today,
                };
                let start = midnight(tz, day);
                let end = midnight(tz, day + Duration::days(1));
                let range = Expr::Range {
                    item: t.expr.boxed(),
                    low: Expr::Time {
                        micros: start,
                        kind,
                    }
                    .boxed(),
                    high: Expr::Time { micros: end, kind }.boxed(),
                    low_open: false,
                    high_open: true,
                };
                Ok(Typed::bool(range, false).fails(true))
            }
            other => Err(self.err(
                node,
                format!("the date method {other}() is not supported in SQL filters"),
            )),
        }
    }

    fn truncate_const(&mut self, node: &Node, t: Typed, unit: Unit, kind: DateKind) -> R<Typed> {
        if matches!(t.expr, Expr::Time { .. }) {
            return self.fold_trunc(node, &t, unit);
        }
        Ok(Typed::new(
            Expr::DateTrunc(unit, local(t.clone(), kind).boxed()),
            Ty::local(),
            t.nullable,
        ))
    }

    fn fold_trunc(&self, node: &Node, t: &Typed, unit: Unit) -> R<Typed> {
        let Expr::Time { micros, .. } = t.expr else {
            unreachable!()
        };
        let src = format!(
            "d({}, \"{}\").startOf(\"{}\")",
            micros / 1000,
            self.options.timezone.name(),
            unit.name()
        );
        let folded = evaluate(&src).map_err(|m| self.err(node, m))?;
        let Expr::Time { micros, .. } = folded.expr else {
            return Err(self.err(node, "could not truncate the date"));
        };
        Ok(Typed::new(
            Expr::Time {
                micros,
                kind: DateKind::Local,
            },
            Ty::local(),
            false,
        ))
    }

    fn unit(&mut self, node: &Node, arg: Option<&Node>) -> R<Unit> {
        let Some(arg) = arg else {
            return Err(self.err(node, "a unit is required"));
        };
        let t = self.lower(arg)?;
        match t.expr {
            Expr::Str(s) => {
                Unit::parse(&s).ok_or_else(|| self.err(arg, format!("unknown unit `{s}`")))
            }
            _ => Err(self.err(arg, "the unit must be a constant string")),
        }
    }

    fn duration(&mut self, node: &Node, args: &[&Node]) -> R<(i64, Unit)> {
        match args.len() {
            2 => {
                let n = self.lower(args[0])?;
                let Expr::Num(n) = n.expr else {
                    return Err(self.err(args[0], "the amount must be a constant number"));
                };
                if !n.fract().is_zero() {
                    return Err(self.err(args[0], "the amount must be a whole number"));
                }
                let n = n
                    .to_i64()
                    .filter(|n| n.unsigned_abs() <= MAX_AMOUNT as u64)
                    .ok_or_else(|| self.err(args[0], "the amount is out of range"))?;
                let unit = self.unit(node, Some(args[1]))?;
                Ok((n, unit))
            }
            1 => {
                let t = self.lower(args[0])?;
                let Expr::Str(s) = t.expr else {
                    return Err(self.err(
                        args[0],
                        "the duration must be a constant string like \"3d\"",
                    ));
                };
                parse_duration(&s).ok_or_else(|| {
                    self.err(
                        args[0],
                        format!("`{s}` is not a single-unit duration like \"3d\" or \"2 hours\""),
                    )
                })
            }
            _ => Err(self.err(node, "expected an amount and a unit")),
        }
    }
}

fn invalid_first(cmp: Cmp, l: Expr, r: Expr) -> Expr {
    let c = Expr::Cmp(cmp, l.clone().boxed(), r.clone().boxed());
    let null = |e: &Expr| Expr::IsNull(e.clone().boxed());
    match (l.is_literal(), r.is_literal(), cmp) {
        (false, true, Cmp::Lt | Cmp::Le) => Expr::Or(null(&l).boxed(), c.boxed()),
        (false, true, _) => Expr::BoolOf(c.boxed()),
        (true, false, Cmp::Gt | Cmp::Ge) => Expr::Or(null(&r).boxed(), c.boxed()),
        (true, false, _) => Expr::BoolOf(c.boxed()),
        _ => {
            let when_left_null = match cmp {
                Cmp::Lt => null(&r).not(),
                Cmp::Le => Expr::Bool(true),
                Cmp::Gt => Expr::Bool(false),
                Cmp::Ge => null(&r),
            };
            let when_right_null = Expr::Bool(matches!(cmp, Cmp::Gt | Cmp::Ge));
            Expr::Case(
                null(&l).boxed(),
                when_left_null.boxed(),
                Expr::Case(null(&r).boxed(), when_right_null.boxed(), c.boxed()).boxed(),
            )
        }
    }
}

const MAX_AMOUNT: i64 = 1_000_000;

pub fn safe_name(name: &str) -> bool {
    !name
        .chars()
        .any(|c| matches!(c, '\'' | '"' | '`' | '\\') || c.is_control())
}

fn parse_duration(s: &str) -> Option<(i64, Unit)> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (num, unit) = s.split_at(split);
    let n: i64 = num.parse().ok().filter(|n: &i64| *n <= MAX_AMOUNT)?;
    Some((n, Unit::parse(unit.trim())?))
}

fn risky(hir: &regex_syntax::hir::Hir, repeated: bool) -> bool {
    use regex_syntax::hir::HirKind;
    match hir.kind() {
        HirKind::Repetition(r) => {
            let unbounded = r.max.is_none_or(|m| m > 1);
            if repeated && unbounded {
                return true;
            }
            risky(&r.sub, repeated || unbounded)
        }
        HirKind::Alternation(alts) => repeated || alts.iter().any(|a| risky(a, repeated)),
        HirKind::Capture(c) => risky(&c.sub, repeated),
        HirKind::Concat(parts) => parts.iter().any(|p| risky(p, repeated)),
        _ => false,
    }
}

fn midnight(tz: chrono_tz::Tz, day: chrono::NaiveDate) -> i64 {
    let naive = day.and_time(NaiveTime::MIN);
    tz.from_local_datetime(&naive)
        .earliest()
        .map(|d| d.timestamp_micros())
        .unwrap_or_else(|| naive.and_utc().timestamp_micros())
}

fn local(t: Typed, kind: DateKind) -> Expr {
    match t.expr {
        Expr::Time { micros, .. } => Expr::Time {
            micros,
            kind: DateKind::Local,
        },
        e => Expr::LocalTime {
            value: e.boxed(),
            kind,
        },
    }
}

fn align_dates(l: Typed, r: Typed) -> (Typed, Typed) {
    let (l, r) = mix_kinds(l, r);
    let kind = |t: &Typed| match t.ty {
        Ty::Date { kind } => Some(kind),
        _ => None,
    };
    match (&l.expr, &r.expr) {
        (Expr::Time { micros, .. }, other) if !matches!(other, Expr::Time { .. }) => {
            if let Some(k) = kind(&r) {
                let micros = *micros;
                return (
                    Typed::new(Expr::Time { micros, kind: k }, Ty::Date { kind: k }, false),
                    r,
                );
            }
        }
        (other, Expr::Time { micros, .. }) if !matches!(other, Expr::Time { .. }) => {
            if let Some(k) = kind(&l) {
                let micros = *micros;
                return (
                    l,
                    Typed::new(Expr::Time { micros, kind: k }, Ty::Date { kind: k }, false),
                );
            }
        }
        _ => {}
    }
    (l, r)
}

fn error_when(sources: &[&Typed]) -> Option<Expr> {
    sources
        .iter()
        .filter(|t| t.sql_null())
        .map(|t| Expr::IsNull(t.expr.clone().boxed()))
        .reduce(|a, b| Expr::Or(a.boxed(), b.boxed()))
}

fn mix_kinds(l: Typed, r: Typed) -> (Typed, Typed) {
    let kind = |t: &Typed| match t.ty {
        Ty::Date { kind } if !t.expr.is_literal() => Some(kind),
        _ => None,
    };
    let instant = |t: Typed, k: DateKind| Typed {
        expr: Expr::ToInstant {
            value: t.expr.boxed(),
            kind: k,
        },
        ty: Ty::instant(),
        ..t
    };
    match (kind(&l), kind(&r)) {
        (Some(DateKind::Instant), Some(k)) if k != DateKind::Instant => (l, instant(r, k)),
        (Some(k), Some(DateKind::Instant)) if k != DateKind::Instant => (instant(l, k), r),
        (Some(DateKind::Date), Some(DateKind::Local)) => {
            (instant(l, DateKind::Date), instant(r, DateKind::Local))
        }
        (Some(DateKind::Local), Some(DateKind::Date)) => {
            (instant(l, DateKind::Local), instant(r, DateKind::Date))
        }
        _ => (l, r),
    }
}

fn same_family(a: &Ty, b: &Ty) -> bool {
    matches!((a, b), (Ty::Date { .. }, Ty::Date { .. })) || a == b
}

fn concat(mut pieces: Vec<Expr>) -> Expr {
    let mut flat = Vec::new();
    for p in pieces.drain(..) {
        match p {
            Expr::Concat(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }
    match flat.len() {
        0 => Expr::Str(String::new()),
        1 => flat.pop().unwrap(),
        _ => Expr::Concat(flat),
    }
}

fn joined(segs: &[Seg]) -> String {
    segs.iter()
        .map(|s| match s {
            Seg::Name(n) => n.as_str(),
            Seg::Index(_) => "",
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn longest(source: &Source, segs: &[Seg]) -> Option<usize> {
    let names = segs
        .iter()
        .take_while(|s| matches!(s, Seg::Name(_)))
        .count();
    let dotted = |k: usize| {
        segs[..k]
            .iter()
            .any(|s| matches!(s, Seg::Name(n) if n.contains('.')))
    };
    (1..=names)
        .rev()
        .find(|&k| !dotted(k) && source.field(&joined(&segs[..k])).is_some())
}

fn deprecated_name(d: DeprecatedFunction) -> String {
    format!("{d}()")
}
