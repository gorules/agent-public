use zen_expression::functions::FunctionKind;
use zen_expression::parser::Node;

use crate::{Error, Limits};

fn children<'a>(node: &'a Node<'a>) -> Vec<&'a Node<'a>> {
    match node {
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::Pointer
        | Node::Root
        | Node::Identifier(_) => Vec::new(),
        Node::TemplateString(parts) | Node::Array(parts) => parts.to_vec(),
        Node::Object(pairs) => pairs.iter().flat_map(|(k, v)| [*k, *v]).collect(),
        Node::Assignments { list, output } => list
            .iter()
            .flat_map(|(k, v)| [*k, *v])
            .chain(output.iter().copied())
            .collect(),
        Node::Closure { body, .. } => vec![*body],
        Node::Parenthesized(inner) => vec![*inner],
        Node::Member { node, property } => vec![*node, *property],
        Node::Slice { node, from, to } => [Some(*node), *from, *to].into_iter().flatten().collect(),
        Node::Interval { left, right, .. } => vec![*left, *right],
        Node::Conditional {
            condition,
            on_true,
            on_false,
        } => vec![*condition, *on_true, *on_false],
        Node::Unary { node, .. } => vec![*node],
        Node::Binary { left, right, .. } => vec![*left, *right],
        Node::FunctionCall { arguments, .. } => arguments.to_vec(),
        Node::MethodCall {
            this, arguments, ..
        } => std::iter::once(*this)
            .chain(arguments.iter().copied())
            .collect(),
        Node::Error { node, .. } => node.iter().copied().collect(),
    }
}

pub fn check(root: &Node, limits: &Limits) -> Result<(), Error> {
    let mut stack = vec![(root, 1usize, 0usize)];
    while let Some((node, depth, closures)) = stack.pop() {
        if depth > limits.depth {
            return Err(Error {
                message: format!(
                    "the expression is nested deeper than {} levels",
                    limits.depth
                ),
                span: None,
            });
        }
        let closures = match node {
            Node::FunctionCall {
                kind: FunctionKind::Closure(_),
                ..
            } => closures + 1,
            _ => closures,
        };
        if closures > limits.closures {
            return Err(Error {
                message: format!("closures can be nested at most {} deep", limits.closures),
                span: None,
            });
        }
        for child in children(node) {
            stack.push((child, depth + 1, closures));
        }
    }
    Ok(())
}
