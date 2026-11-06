use std::borrow::Cow;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Prefix(Option<Arc<String>>);

impl From<String> for Prefix {
    fn from(value: String) -> Self {
        Self(Some(Arc::new(format_prefix(value))))
    }
}

impl From<Option<String>> for Prefix {
    fn from(value: Option<String>) -> Self {
        Self(value.map(|p| Arc::from(format_prefix(p))))
    }
}

impl Prefix {
    pub fn strip<'a>(&self, target: Cow<'a, str>) -> Cow<'a, str> {
        let Some(prefix) = &self.0 else { return target };

        match target {
            Cow::Borrowed(s) => match s.strip_prefix(prefix.as_str()) {
                Some(stripped) => Cow::Borrowed(stripped),
                None => Cow::Borrowed(s),
            },
            Cow::Owned(s) => match s.strip_prefix(prefix.as_str()) {
                Some(stripped) => Cow::Owned(stripped.to_owned()),
                None => Cow::Owned(s),
            },
        }
    }

    pub fn prepend<'a>(&self, target: Cow<'a, str>) -> Cow<'a, str> {
        let Some(prefix) = &self.0 else { return target };

        let mut owned_target = target.into_owned();
        owned_target.insert_str(0, prefix.as_str());

        Cow::Owned(owned_target)
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.0 {
            None => None,
            Some(s) => Some(s.as_str()),
        }
    }

    pub fn to_string(&self) -> Option<String> {
        match &self.0 {
            None => None,
            Some(s) => Some(s.to_string()),
        }
    }
}

fn format_prefix(mut p: String) -> String {
    if !p.ends_with('/') {
        p.push('/')
    }

    p
}
