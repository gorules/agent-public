use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Scalar {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Timestamp(i64),
    Date(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    IsNull,
    IsNotNull,
    Prefix,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Predicate {
    pub column: String,
    pub op: Op,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Scalar>,
}

impl Predicate {
    pub fn new(column: impl Into<String>, op: Op, value: Scalar) -> Predicate {
        Predicate {
            column: column.into(),
            op,
            values: vec![value],
        }
    }

    pub fn is_in(column: impl Into<String>, values: Vec<Scalar>) -> Predicate {
        Predicate {
            column: column.into(),
            op: Op::In,
            values,
        }
    }

    pub fn unary(column: impl Into<String>, op: Op) -> Predicate {
        Predicate {
            column: column.into(),
            op,
            values: Vec::new(),
        }
    }
}
