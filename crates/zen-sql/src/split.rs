use bumpalo::Bump;
use zen_expression::lexer::{
    Bracket, Lexer, LogicalOperator, Operator, QuotationMark, TemplateString, TokenKind,
};
use zen_expression::parser::{Node, Parser};

use crate::{Compiled, Error, Options, Schema, filter};

#[derive(Clone, Debug, PartialEq)]
pub struct Parts {
    pub compiled: Option<Compiled>,
    pub residual: Vec<String>,
}

impl Parts {
    pub fn residual_source(&self) -> Option<String> {
        match self.residual.len() {
            0 => None,
            1 => Some(self.residual[0].clone()),
            _ => Some(
                self.residual
                    .iter()
                    .map(|p| format!("({p})"))
                    .collect::<Vec<_>>()
                    .join(" and "),
            ),
        }
    }
}

pub fn filter_parts(source: &str, schema: &Schema, options: &Options) -> Result<Parts, Error> {
    let whole = match filter(source, schema, options) {
        Ok(c) => {
            return Ok(Parts {
                compiled: Some(c),
                residual: Vec::new(),
            });
        }
        Err(e) => e,
    };
    let Some(parts) = conjuncts(source)? else {
        return parsed(source)
            .map(|()| Parts {
                compiled: None,
                residual: vec![source.trim().to_string()],
            })
            .map_err(|_| whole);
    };
    let mut sql = Vec::new();
    let mut residual = Vec::new();
    for part in parts {
        match filter(&part, schema, options) {
            Ok(_) => sql.push(part),
            Err(_) => residual.push(part),
        }
    }
    let compiled = match sql.len() {
        0 => None,
        _ => {
            let joined = sql
                .iter()
                .map(|p| format!("({p})"))
                .collect::<Vec<_>>()
                .join(" and ");
            match filter(&joined, schema, options) {
                Ok(c) => Some(c),
                Err(_) => {
                    residual.splice(0..0, sql.drain(..));
                    None
                }
            }
        }
    };
    Ok(Parts { compiled, residual })
}

fn parsed(source: &str) -> Result<(), Error> {
    let bump = Bump::new();
    let mut lexer = Lexer::new();
    let tokens = lexer.tokenize(&bump, source).map_err(|e| Error {
        message: e.to_string(),
        span: None,
    })?;
    let parser = Parser::try_new(&tokens, &bump).map_err(|e| Error {
        message: e.to_string(),
        span: None,
    })?;
    let result = parser.standard().parse();
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
    Ok(())
}

fn conjuncts(source: &str) -> Result<Option<Vec<String>>, Error> {
    let bump = Bump::new();
    let mut lexer = Lexer::new();
    let tokens = lexer.tokenize(&bump, source).map_err(|e| Error {
        message: e.to_string(),
        span: None,
    })?;
    let parser = Parser::try_new(&tokens, &bump).map_err(|e| Error {
        message: e.to_string(),
        span: None,
    })?;
    let result = parser.standard().parse();
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
    let links = chain(result.root);
    if links == 0 {
        return Ok(None);
    }
    let mut open: Vec<u8> = Vec::new();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for t in tokens.iter() {
        match t.kind {
            TokenKind::QuotationMark(QuotationMark::Backtick) => {
                if open.last() == Some(&b'`') {
                    open.pop();
                } else {
                    open.push(b'`');
                }
            }
            TokenKind::TemplateString(TemplateString::ExpressionStart) => open.push(b'{'),
            TokenKind::TemplateString(TemplateString::ExpressionEnd) => {
                open.pop();
            }
            TokenKind::Bracket(
                Bracket::LeftParenthesis | Bracket::LeftSquareBracket | Bracket::LeftCurlyBracket,
            ) => open.push(b'('),
            TokenKind::Bracket(
                Bracket::RightParenthesis
                | Bracket::RightSquareBracket
                | Bracket::RightCurlyBracket,
            ) => {
                open.pop();
            }
            TokenKind::Operator(Operator::Logical(LogicalOperator::And)) if open.is_empty() => {
                cuts.push((t.span.0 as usize, t.span.1 as usize));
            }
            _ => {}
        }
    }
    if cuts.len() != links {
        return Ok(None);
    }
    let mut parts = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0;
    for (from, to) in cuts {
        parts.push(
            source
                .get(start..from)
                .unwrap_or_default()
                .trim()
                .to_string(),
        );
        start = to;
    }
    parts.push(source.get(start..).unwrap_or_default().trim().to_string());
    if parts.iter().any(|p| p.is_empty()) {
        return Ok(None);
    }
    Ok(Some(parts))
}

fn chain(node: &Node) -> usize {
    match node {
        Node::Binary {
            operator: Operator::Logical(LogicalOperator::And),
            left,
            ..
        } => 1 + chain(left),
        _ => 0,
    }
}
