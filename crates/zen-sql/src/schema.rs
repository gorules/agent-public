use arrow::datatypes::{DataType, Field as ArrowField, Schema as ArrowSchema};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateKind {
    Date,
    Local,
    Instant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Ty {
    Null,
    Bool,
    Number,
    String,
    Date { kind: DateKind },
    List { item: Box<Ty> },
    Struct { fields: Vec<Field> },
    Map { value: Box<Ty> },
    Json,
    Any,
}

impl Ty {
    pub fn list(item: Ty) -> Ty {
        Ty::List {
            item: Box::new(item),
        }
    }

    pub fn object(fields: Vec<Field>) -> Ty {
        Ty::Struct { fields }
    }

    pub fn map(value: Ty) -> Ty {
        Ty::Map {
            value: Box::new(value),
        }
    }

    pub fn instant() -> Ty {
        Ty::Date {
            kind: DateKind::Instant,
        }
    }

    pub fn local() -> Ty {
        Ty::Date {
            kind: DateKind::Local,
        }
    }

    pub fn date() -> Ty {
        Ty::Date {
            kind: DateKind::Date,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Ty::Null => "null",
            Ty::Bool => "bool",
            Ty::Number => "number",
            Ty::String => "string",
            Ty::Date { .. } => "date",
            Ty::List { .. } => "array",
            Ty::Struct { .. } | Ty::Map { .. } => "object",
            Ty::Json => "json",
            Ty::Any => "unknown",
        }
    }

    pub fn is_scalar(&self) -> bool {
        matches!(self, Ty::Bool | Ty::Number | Ty::String | Ty::Date { .. })
    }

    pub fn is_open(&self) -> bool {
        matches!(self, Ty::Json | Ty::Any)
    }

    pub fn from_arrow(data_type: &DataType) -> Option<Ty> {
        Some(match data_type {
            DataType::Null => Ty::Null,
            DataType::Boolean => Ty::Bool,
            DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float16
            | DataType::Float32
            | DataType::Float64
            | DataType::Decimal32(..)
            | DataType::Decimal64(..)
            | DataType::Decimal128(..)
            | DataType::Decimal256(..) => Ty::Number,
            DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => Ty::String,
            DataType::Date32 | DataType::Date64 => Ty::date(),
            DataType::Timestamp(_, None) => Ty::local(),
            DataType::Timestamp(_, Some(_)) => Ty::instant(),
            DataType::List(f)
            | DataType::LargeList(f)
            | DataType::ListView(f)
            | DataType::LargeListView(f)
            | DataType::FixedSizeList(f, _) => Ty::list(Field::from_arrow(f)?.ty),
            DataType::Struct(fields) => {
                Ty::object(fields.iter().filter_map(|f| Field::from_arrow(f)).collect())
            }
            DataType::Map(entries, _) => match entries.data_type() {
                DataType::Struct(kv) if kv.len() == 2 => {
                    if !matches!(
                        kv[0].data_type(),
                        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
                    ) {
                        return None;
                    }
                    Ty::map(Field::from_arrow(&kv[1])?.ty)
                }
                _ => return None,
            },
            DataType::Dictionary(_, value) => Ty::from_arrow(value)?,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    #[serde(default = "yes")]
    pub nullable: bool,
}

fn yes() -> bool {
    true
}

impl Field {
    pub fn new(name: impl Into<String>, ty: Ty) -> Field {
        Field {
            name: name.into(),
            ty,
            nullable: true,
        }
    }

    pub fn required(name: impl Into<String>, ty: Ty) -> Field {
        Field {
            name: name.into(),
            ty,
            nullable: false,
        }
    }

    pub fn from_arrow(field: &ArrowField) -> Option<Field> {
        let json = field
            .metadata()
            .get("ARROW:extension:name")
            .is_some_and(|n| n == "arrow.json");
        let ty = if json {
            Ty::Json
        } else {
            Ty::from_arrow(field.data_type())?
        };
        Some(Field {
            name: field.name().clone(),
            ty,
            nullable: field.is_nullable(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
    pub fields: Vec<Field>,
}

impl Source {
    pub fn new(name: impl Into<String>, fields: Vec<Field>) -> Source {
        Source {
            name: name.into(),
            qualifier: None,
            fields,
        }
    }

    pub fn qualified(mut self, qualifier: impl Into<String>) -> Source {
        self.qualifier = Some(qualifier.into());
        self
    }

    pub fn from_arrow(name: impl Into<String>, schema: &ArrowSchema) -> Source {
        Source::new(
            name,
            schema
                .fields()
                .iter()
                .filter_map(|f| Field::from_arrow(f))
                .collect(),
        )
    }

    pub fn json(mut self, column: &str) -> Source {
        for f in &mut self.fields {
            if f.name == column {
                f.ty = Ty::Json;
            }
        }
        self
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl Schema {
    pub fn new() -> Schema {
        Schema::default()
    }

    pub fn single(source: Source) -> Schema {
        let default = Some(source.name.clone());
        Schema {
            sources: vec![source],
            default,
        }
    }

    pub fn with(mut self, source: Source) -> Schema {
        self.sources.push(source);
        self
    }

    pub fn default_source(mut self, name: impl Into<String>) -> Schema {
        self.default = Some(name.into());
        self
    }

    pub fn source(&self, name: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.name == name)
    }

    pub fn fallback(&self) -> Option<&Source> {
        self.default.as_deref().and_then(|n| self.source(n))
    }
}
