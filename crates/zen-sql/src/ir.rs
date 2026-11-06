use rust_decimal::Decimal;

use crate::schema::{DateKind, Ty};

#[derive(Clone, Debug, PartialEq)]
pub enum Base {
    Column {
        qualifier: Option<String>,
        source: String,
        name: String,
    },
    Var(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Field(String),
    Key(String),
    Index(i64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub base: Base,
    pub base_ty: Ty,
    pub steps: Vec<(Step, Ty)>,
}

impl Path {
    pub fn is_plain_column(&self) -> Option<(&str, &str)> {
        match (&self.base, self.steps.is_empty()) {
            (Base::Column { source, name, .. }, true) => Some((source, name)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scalar {
    Str,
    Num,
    Bool,
    Any,
    Text,
    Parse,
    Container,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cmp {
    pub fn sql(self) -> &'static str {
        match self {
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Gt => ">",
            Cmp::Ge => ">=",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Second,
    Minute,
    Hour,
    Day,
    DayOfYear,
    Week,
    Weekday,
    Month,
    Quarter,
    Year,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl Unit {
    pub fn parse(unit: &str) -> Option<Unit> {
        Some(match unit {
            "seconds" | "second" | "secs" | "sec" | "s" => Unit::Second,
            "minutes" | "minute" | "min" | "mins" | "m" => Unit::Minute,
            "hours" | "hour" | "hr" | "hrs" | "h" => Unit::Hour,
            "days" | "day" | "d" => Unit::Day,
            "weeks" | "week" | "w" => Unit::Week,
            "months" | "month" | "mo" | "M" => Unit::Month,
            "quarters" | "quarter" | "qtr" | "q" => Unit::Quarter,
            "years" | "year" | "y" => Unit::Year,
            _ => return None,
        })
    }

    pub fn seconds(self) -> Option<i64> {
        Some(match self {
            Unit::Second => 1,
            Unit::Minute => 60,
            Unit::Hour => 3_600,
            Unit::Day => 86_400,
            Unit::Week => 604_800,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Unit::Second => "second",
            Unit::Minute => "minute",
            Unit::Hour => "hour",
            Unit::Day => "day",
            Unit::Week => "week",
            Unit::Month => "month",
            Unit::Quarter => "quarter",
            Unit::Year => "year",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    Upper,
    Lower,
    Trim,
    StartsWith,
    EndsWith,
    StrContains,
    Matches,
    StrLen,
    ListLen,
    Abs,
    Floor,
    Ceil,
    Round,
    Trunc,
    ToString,
    ToNumber,
    IsNumeric,
    Sum,
    Avg,
    Min,
    Max,
    Median,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Each {
    Some,
    All,
    None,
    One,
    Count,
    Filter,
    Map,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Null,
    Bool(bool),
    Num(Decimal),
    Str(String),
    Time {
        micros: i64,
        kind: DateKind,
    },
    Path(Path),
    Open {
        path: Path,
        as_: Scalar,
    },
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>, bool),
    Or(Box<Expr>, Box<Expr>),
    Eq {
        left: Box<Expr>,
        right: Box<Expr>,
        nullable: (bool, bool),
    },
    Cmp(Cmp, Box<Expr>, Box<Expr>),
    IsNull(Box<Expr>),
    In {
        item: Box<Expr>,
        list: Vec<Expr>,
        nullable: bool,
    },
    InArray {
        item: Box<Expr>,
        array: Box<Expr>,
    },
    Range {
        item: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
        low_open: bool,
        high_open: bool,
    },
    Arith(Arith, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Concat(Vec<Expr>),
    Coalesce(Box<Expr>, Box<Expr>),
    Case(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
    BoolOf(Box<Expr>),
    LocalTime {
        value: Box<Expr>,
        kind: DateKind,
    },
    Parse {
        value: Box<Expr>,
        from: Ty,
    },
    DatePart(Part, Box<Expr>),
    DateTrunc(Unit, Box<Expr>),
    DateAdd {
        value: Box<Expr>,
        amount: i64,
        unit: Unit,
        kind: DateKind,
    },
    Epoch {
        value: Box<Expr>,
        kind: DateKind,
    },
    ToInstant {
        value: Box<Expr>,
        kind: DateKind,
    },
    Raw(String),
    Each {
        kind: Each,
        list: Box<Expr>,
        var: usize,
        item: Ty,
        fallible: bool,
        body: Box<Expr>,
    },
}

impl Expr {
    pub fn boxed(self) -> Box<Expr> {
        Box::new(self)
    }

    pub fn is_literal(&self) -> bool {
        matches!(
            self,
            Expr::Null | Expr::Bool(_) | Expr::Num(_) | Expr::Str(_) | Expr::Time { .. }
        )
    }

    pub fn not(self) -> Expr {
        match self {
            Expr::Bool(b) => Expr::Bool(!b),
            Expr::Not(inner) => *inner,
            other => Expr::Not(other.boxed()),
        }
    }
}
