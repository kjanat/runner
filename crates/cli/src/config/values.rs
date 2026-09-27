//! Setting values config, environment variables and flags share.

/// Whether runner may download a package to run a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Download {
    /// Download without asking.
    Allow,
    /// Refuse a command that needs a download.
    Refuse,
    /// Confirm each download.
    Ask,
}

impl Download {
    /// The value `raw` spells: `ask`, or a boolean word.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let word = raw.trim();
        if word.eq_ignore_ascii_case("ask") {
            return Ok(Self::Ask);
        }
        match boolean(word) {
            Some(true) => Ok(Self::Allow),
            Some(false) => Ok(Self::Refuse),
            None => Err(format!("{raw:?} is not true, false or ask")),
        }
    }
}

/// The boolean `raw` spells: `1`/`true`/`yes`/`on` or `0`/`false`/`no`/`off`.
pub(crate) fn boolean(raw: &str) -> Option<bool> {
    let word = raw.trim();
    let any = |words: &[&str]| words.iter().any(|w| word.eq_ignore_ascii_case(w));
    if any(&["1", "true", "yes", "on"]) {
        Some(true)
    } else if any(&["0", "false", "no", "off"]) {
        Some(false)
    } else {
        None
    }
}

impl serde::Serialize for Download {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Allow => serializer.serialize_bool(true),
            Self::Refuse => serializer.serialize_bool(false),
            Self::Ask => serializer.serialize_str("ask"),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Download {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Download;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("true, false or \"ask\"")
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Download, E> {
                Ok(if value {
                    Download::Allow
                } else {
                    Download::Refuse
                })
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Download, E> {
                if value == "ask" {
                    Ok(Download::Ask)
                } else {
                    Err(E::invalid_value(serde::de::Unexpected::Str(value), &self))
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl schemars::JsonSchema for Download {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Download".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "enum": [true, false, "ask"] })
    }
}

#[cfg(test)]
mod tests {
    use super::{Download, boolean};

    #[test]
    fn download_parses_ask_and_every_boolean_word() {
        assert_eq!(Download::parse("ask"), Ok(Download::Ask));
        assert_eq!(Download::parse(" Yes "), Ok(Download::Allow));
        assert_eq!(Download::parse("0"), Ok(Download::Refuse));
        assert!(Download::parse("maybe").is_err());
    }

    #[test]
    fn booleans_are_case_insensitive_and_closed() {
        assert_eq!(boolean("ON"), Some(true));
        assert_eq!(boolean("off"), Some(false));
        assert_eq!(boolean(""), None);
        assert_eq!(boolean("flase"), None);
    }
}
