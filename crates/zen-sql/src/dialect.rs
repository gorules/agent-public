use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    DuckDb,
    Postgres,
    BigQuery,
    Snowflake,
    Databricks,
    Redshift,
}

impl Dialect {
    pub const ALL: [Dialect; 6] = [
        Dialect::DuckDb,
        Dialect::Postgres,
        Dialect::BigQuery,
        Dialect::Snowflake,
        Dialect::Databricks,
        Dialect::Redshift,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Dialect::DuckDb => "DuckDB",
            Dialect::Postgres => "Postgres",
            Dialect::BigQuery => "BigQuery",
            Dialect::Snowflake => "Snowflake",
            Dialect::Databricks => "Databricks",
            Dialect::Redshift => "Redshift",
        }
    }

    pub(crate) fn ident(self, name: &str) -> String {
        match self {
            Dialect::BigQuery => format!(
                "`{}`",
                name.replace('\\', "\\\\")
                    .replace('`', "\\`")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
            ),
            Dialect::Databricks => format!("`{}`", name.replace('`', "``")),
            _ => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    pub(crate) fn string(self, text: &str) -> String {
        if self == Dialect::Postgres && text.contains('\\') {
            return format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"));
        }
        let backslash = matches!(
            self,
            Dialect::BigQuery | Dialect::Databricks | Dialect::Snowflake | Dialect::Redshift
        );
        let mut out = String::with_capacity(text.len() + 2);
        out.push('\'');
        for c in text.chars() {
            match c {
                '\'' if matches!(self, Dialect::BigQuery | Dialect::Databricks) => {
                    out.push_str("\\'")
                }
                '\'' => out.push_str("''"),
                '\\' if backslash => out.push_str("\\\\"),
                '\n' if backslash => out.push_str("\\n"),
                '\r' if backslash => out.push_str("\\r"),
                '\t' if backslash => out.push_str("\\t"),
                '\0' => {}
                c => out.push(c),
            }
        }
        out.push('\'');
        out
    }

    pub fn marker(self, index: usize) -> String {
        match self {
            Dialect::DuckDb | Dialect::Snowflake => "?".into(),
            Dialect::Postgres => format!("${}", index + 1),
            Dialect::BigQuery => format!("@p{index}"),
            Dialect::Databricks | Dialect::Redshift => format!(":p{index}"),
        }
    }

    pub fn text_type(self) -> &'static str {
        match self {
            Dialect::BigQuery | Dialect::Databricks => "STRING",
            Dialect::Postgres => "TEXT",
            _ => "VARCHAR",
        }
    }

    pub fn double_type(self) -> &'static str {
        match self {
            Dialect::BigQuery => "FLOAT64",
            Dialect::DuckDb | Dialect::Databricks => "DOUBLE",
            _ => "DOUBLE PRECISION",
        }
    }
}
