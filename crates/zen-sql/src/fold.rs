use std::fmt::Write;

use zen_expression::Isolate;
use zen_expression::functions::{FunctionKind, InternalFunction, MethodKind};
use zen_expression::lexer::{Bracket, Operator};
use zen_expression::parser::Node;
use zen_expression::variable::Variable;

use crate::ir::Expr;
use crate::schema::{DateKind, Ty};

pub fn is_const(node: &Node) -> bool {
    match node {
        Node::Null | Node::Bool(_) | Node::Number(_) | Node::String(_) => true,
        Node::Identifier(_) | Node::Root | Node::Pointer | Node::Closure { .. } => false,
        Node::Error { .. } | Node::Assignments { .. } => false,
        Node::TemplateString(parts) | Node::Array(parts) => parts.iter().all(|p| is_const(p)),
        Node::Object(pairs) => pairs.iter().all(|(k, v)| is_const(k) && is_const(v)),
        Node::Parenthesized(inner) => is_const(inner),
        Node::Member { node, property } => is_const(node) && is_const(property),
        Node::Slice { node, from, to } => {
            is_const(node) && from.is_none_or(|f| is_const(f)) && to.is_none_or(|t| is_const(t))
        }
        Node::Interval { left, right, .. } => is_const(left) && is_const(right),
        Node::Conditional {
            condition,
            on_true,
            on_false,
        } => is_const(condition) && is_const(on_true) && is_const(on_false),
        Node::Unary { node, .. } => is_const(node),
        Node::Binary { left, right, .. } => is_const(left) && is_const(right),
        Node::FunctionCall { kind, arguments } => {
            !matches!(
                kind,
                FunctionKind::Internal(
                    InternalFunction::Rand
                        | InternalFunction::FuzzyMatch
                        | InternalFunction::Extract
                        | InternalFunction::Split
                        | InternalFunction::Flatten
                        | InternalFunction::Merge
                        | InternalFunction::MergeDeep
                        | InternalFunction::Keys
                        | InternalFunction::Values
                        | InternalFunction::Mode
                        | InternalFunction::Type
                ) | FunctionKind::Closure(_)
            ) && arguments.iter().all(|a| is_const(a))
        }
        Node::MethodCall {
            this, arguments, ..
        } => is_const(this) && arguments.iter().all(|a| is_const(a)),
    }
}

pub fn is_trivial(node: &Node) -> bool {
    match node {
        Node::Null | Node::Bool(_) | Node::Number(_) | Node::String(_) => true,
        Node::Parenthesized(inner) => is_trivial(inner),
        _ => false,
    }
}

pub struct Printer<'t> {
    pub timezone: &'t str,
    pub out: String,
}

impl Printer<'_> {
    pub fn print(&mut self, node: &Node) -> Result<(), String> {
        match node {
            Node::Null => self.out.push_str("null"),
            Node::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
            Node::Number(n) => {
                if n.is_sign_negative() {
                    write!(self.out, "({n})").unwrap();
                } else {
                    write!(self.out, "{n}").unwrap();
                }
            }
            Node::String(s) => self.string(s)?,
            Node::TemplateString(parts) => {
                self.out.push('(');
                if parts.is_empty() {
                    self.out.push_str("\"\"");
                }
                for (i, part) in parts.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(" + ");
                    }
                    match part {
                        Node::String(s) => self.string(s)?,
                        other => {
                            self.out.push_str("string(");
                            self.print(other)?;
                            self.out.push(')');
                        }
                    }
                }
                self.out.push(')');
            }
            Node::Array(items) => {
                self.out.push('[');
                self.list(items)?;
                self.out.push(']');
            }
            Node::Object(pairs) => {
                self.out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.out.push('[');
                    self.print(k)?;
                    self.out.push_str("]: ");
                    self.print(v)?;
                }
                self.out.push('}');
            }
            Node::Parenthesized(inner) => {
                self.out.push('(');
                self.print(inner)?;
                self.out.push(')');
            }
            Node::Member { node, property } => {
                self.operand(node)?;
                self.out.push('[');
                self.print(property)?;
                self.out.push(']');
            }
            Node::Slice { node, from, to } => {
                self.operand(node)?;
                self.out.push('[');
                if let Some(f) = from {
                    self.print(f)?;
                }
                self.out.push(':');
                if let Some(t) = to {
                    self.print(t)?;
                }
                self.out.push(']');
            }
            Node::Interval {
                left,
                right,
                left_bracket,
                right_bracket,
            } => {
                self.out
                    .push_str(if *left_bracket == Bracket::LeftParenthesis {
                        "("
                    } else {
                        "["
                    });
                self.operand(left)?;
                self.out.push_str("..");
                self.operand(right)?;
                self.out
                    .push_str(if *right_bracket == Bracket::RightParenthesis {
                        ")"
                    } else {
                        "]"
                    });
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                self.operand(condition)?;
                self.out.push_str(" ? ");
                self.operand(on_true)?;
                self.out.push_str(" : ");
                self.operand(on_false)?;
            }
            Node::Unary { node, operator } => {
                write!(self.out, "{operator} ").unwrap();
                self.operand(node)?;
            }
            Node::Binary {
                left,
                operator,
                right,
            } => {
                self.operand(left)?;
                write!(self.out, " {operator} ").unwrap();
                if matches!(operator, Operator::Comparison(_))
                    && matches!(right, Node::Interval { .. } | Node::Array(_))
                {
                    self.print(right)?;
                } else {
                    self.operand(right)?;
                }
            }
            Node::FunctionCall { kind, arguments } => {
                write!(self.out, "{kind}(").unwrap();
                self.list(arguments)?;
                if *kind == FunctionKind::Internal(InternalFunction::Date) {
                    if !arguments.is_empty() {
                        self.out.push_str(", ");
                    }
                    if arguments.len() < 2 {
                        self.string(self.timezone)?;
                    }
                }
                self.out.push(')');
            }
            Node::MethodCall {
                kind,
                this,
                arguments,
            } => {
                self.operand(this)?;
                let MethodKind::DateMethod(m) = kind;
                write!(self.out, ".{m}(").unwrap();
                self.list(arguments)?;
                self.out.push(')');
            }
            Node::Identifier(_) | Node::Root | Node::Pointer | Node::Closure { .. } => {
                return Err("the expression refers to data".into());
            }
            Node::Assignments { .. } | Node::Error { .. } => {
                return Err("the expression is not a value".into());
            }
        }
        Ok(())
    }

    fn operand(&mut self, node: &Node) -> Result<(), String> {
        let atom = matches!(
            node,
            Node::Null
                | Node::Bool(_)
                | Node::String(_)
                | Node::Array(_)
                | Node::Parenthesized(_)
                | Node::FunctionCall { .. }
                | Node::MethodCall { .. }
                | Node::Member { .. }
        ) || matches!(node, Node::Number(n) if !n.is_sign_negative());
        if atom {
            self.print(node)
        } else {
            self.out.push('(');
            self.print(node)?;
            self.out.push(')');
            Ok(())
        }
    }

    fn list(&mut self, items: &[&Node]) -> Result<(), String> {
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.print(item)?;
        }
        Ok(())
    }

    fn string(&mut self, s: &str) -> Result<(), String> {
        let quote = if !s.contains('"') {
            '"'
        } else if !s.contains('\'') {
            '\''
        } else {
            return Err("strings with both quote kinds cannot be folded".into());
        };
        self.out.push(quote);
        self.out.push_str(s);
        self.out.push(quote);
        Ok(())
    }
}

pub struct Folded {
    pub expr: Expr,
    pub ty: Ty,
}

pub fn evaluate(source: &str) -> Result<Folded, String> {
    let mut isolate = Isolate::new();
    let value = isolate.run_standard(source).map_err(|e| e.to_string())?;
    Ok(match value {
        Variable::Null => Folded {
            expr: Expr::Null,
            ty: Ty::Null,
        },
        Variable::Bool(b) => Folded {
            expr: Expr::Bool(b),
            ty: Ty::Bool,
        },
        Variable::Number(n) => Folded {
            expr: Expr::Num(n),
            ty: Ty::Number,
        },
        Variable::String(s) => Folded {
            expr: Expr::Str(s.to_string()),
            ty: Ty::String,
        },
        Variable::Dynamic(_) => {
            let stamp = isolate
                .run_standard(&format!("({source}).format(\"%s%.6f\")"))
                .map_err(|e| e.to_string())?;
            let Variable::String(stamp) = stamp else {
                return Err("the date is not valid".into());
            };
            let micros = epoch_micros(&stamp).ok_or_else(|| "the date is not valid".to_string())?;
            Folded {
                expr: Expr::Time {
                    micros,
                    kind: DateKind::Instant,
                },
                ty: Ty::instant(),
            }
        }
        Variable::Array(_) => return Err("arrays can only be used on the right of `in`".into()),
        Variable::Object(_) => return Err("objects cannot be used in a filter".into()),
    })
}

fn epoch_micros(stamp: &str) -> Option<i64> {
    let (secs, frac) = stamp.split_once('.')?;
    let secs: i64 = secs.parse().ok()?;
    let frac: i64 = frac.get(..6)?.parse().ok()?;
    secs.checked_mul(1_000_000)?.checked_add(frac)
}
