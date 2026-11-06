mod dialect;
mod fold;
mod guard;
mod ir;
mod lower;
pub mod predicate;
mod pushdown;
mod render;
mod schema;
mod split;

use bumpalo::Bump;
use chrono_tz::Tz;
use predicate::Scalar as Param;
use serde::Serialize;
use zen_expression::lexer::Lexer;
use zen_expression::parser::{Node, Parser};

pub use dialect::Dialect;
pub use pushdown::{PathPushdown, Pushdown};
pub use schema::{DateKind, Field, Schema, Source, Ty};
pub use split::{Parts, filter_parts};

use ir::Expr;
use lower::{Lower, Typed};
use render::Render;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct Error {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub dialect: Dialect,
    pub timezone: Tz,
    pub limits: Limits,
    pub inline: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub source: usize,
    pub depth: usize,
    pub closures: usize,
    pub sql: usize,
    pub params: usize,
    pub pattern: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            source: 8 * 1024,
            depth: 128,
            closures: 3,
            sql: 256 * 1024,
            params: 512,
            pattern: 256,
        }
    }
}

impl Default for Options {
    fn default() -> Options {
        Options {
            dialect: Dialect::DuckDb,
            timezone: Tz::UTC,
            limits: Limits::default(),
            inline: false,
        }
    }
}

impl Options {
    pub fn new(dialect: Dialect) -> Options {
        Options {
            dialect,
            ..Options::default()
        }
    }

    pub fn timezone(mut self, timezone: Tz) -> Options {
        self.timezone = timezone;
        self
    }

    pub fn limits(mut self, limits: Limits) -> Options {
        self.limits = limits;
        self
    }

    pub fn inline(mut self) -> Options {
        self.inline = true;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Compiled {
    pub dialect: Dialect,
    pub sql: String,
    pub params: Vec<Param>,
    pub ty: Ty,
    pub pushdown: Vec<Pushdown>,
    pub paths: Vec<PathPushdown>,
    pub paths_exact: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Filter,
    Value,
}

pub fn filter(source: &str, schema: &Schema, options: &Options) -> Result<Compiled, Error> {
    compile(source, None, schema, options, Mode::Filter)
}

pub fn unary(
    source: &str,
    subject: &str,
    schema: &Schema,
    options: &Options,
) -> Result<Compiled, Error> {
    compile(source, Some(subject), schema, options, Mode::Filter)
}

pub fn value(source: &str, schema: &Schema, options: &Options) -> Result<Compiled, Error> {
    compile(source, None, schema, options, Mode::Value)
}

fn parse<'a>(
    bump: &'a Bump,
    source: &'a str,
    unary: bool,
    limits: &Limits,
) -> Result<(&'a Node<'a>, lower::Meta), Error> {
    if source.len() > limits.source {
        return Err(Error {
            message: format!("the expression is longer than {} bytes", limits.source),
            span: None,
        });
    }
    let mut lexer = Lexer::new();
    let tokens = lexer.tokenize(bump, source).map_err(|e| Error {
        message: e.to_string(),
        span: None,
    })?;
    let parser = Parser::try_new(&tokens, bump)
        .map_err(|e| Error {
            message: e.to_string(),
            span: None,
        })?
        .with_metadata();
    let result = if unary {
        parser.unary().parse()
    } else {
        parser.standard().parse()
    };
    result.error().map_err(|e| Error {
        message: e.to_string(),
        span: None,
    })?;
    if !result.is_complete {
        return Err(Error {
            message: "the expression is incomplete".into(),
            span: None,
        });
    }
    guard::check(result.root, limits)?;
    Ok((result.root, result.metadata.unwrap_or_default()))
}

fn compile(
    source: &str,
    subject: Option<&str>,
    schema: &Schema,
    options: &Options,
    mode: Mode,
) -> Result<Compiled, Error> {
    let bump = Bump::new();
    let subject = match subject {
        Some(path) => {
            let (node, meta) = parse(&bump, path, false, &options.limits)?;
            let mut lower = Lower::new(schema, options, Some(&meta));
            let t = lower.lower(node)?;
            if !matches!(t.expr, Expr::Path(_)) {
                return Err(Error {
                    message: format!("the subject `{path}` must be a field path"),
                    span: None,
                });
            }
            Some(t)
        }
        None => None,
    };
    let (node, meta) = parse(&bump, source, subject.is_some(), &options.limits)?;
    let mut lower = Lower::new(schema, options, Some(&meta));
    lower.subject = subject;
    let typed = lower.lower(node)?;
    let Typed { expr, ty, .. } = match mode {
        Mode::Filter => match typed.ty {
            Ty::Bool => typed,
            Ty::Json | Ty::Any => Typed {
                expr: Expr::Open {
                    path: match typed.expr {
                        Expr::Path(p) => p,
                        _ => {
                            return Err(Error {
                                message: "a filter must be a boolean expression".into(),
                                span: None,
                            });
                        }
                    },
                    as_: ir::Scalar::Bool,
                },
                ty: Ty::Bool,
                nullable: true,
                dated: false,
                fallible: false,
                errs: None,
            },
            other => {
                return Err(Error {
                    message: format!(
                        "a filter must be a boolean expression, got a {}",
                        other.name()
                    ),
                    span: None,
                });
            }
        },
        Mode::Value if typed.ty.is_open() => match typed.expr {
            Expr::Path(path) => Typed {
                expr: Expr::Open {
                    path,
                    as_: ir::Scalar::Text,
                },
                ty: Ty::String,
                nullable: false,
                dated: false,
                fallible: true,
                errs: None,
            },
            _ => typed,
        },
        Mode::Value => typed,
    };
    let mut render = Render::new(options.dialect, options.timezone);
    render.limits = options.limits;
    render.inline = options.inline;
    render.expr(&expr, mode == Mode::Value)?;
    render.finish()?;
    let (pushdown, (paths, paths_exact)) = match mode {
        Mode::Filter => (
            pushdown::extract(&expr, options.timezone),
            pushdown::extract_paths(&expr, options.timezone),
        ),
        Mode::Value => (Vec::new(), (Vec::new(), false)),
    };
    Ok(Compiled {
        dialect: options.dialect,
        sql: render.sql,
        params: render.params,
        ty,
        pushdown,
        paths,
        paths_exact,
    })
}
