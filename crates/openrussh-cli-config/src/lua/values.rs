use crate::algorithms::AlgorithmList;

use serde::{Deserialize, Deserializer};

pub(super) enum Strings {
    One(String),
    Many(Vec<String>),
}

impl Strings {
    pub(super) fn as_slice(&self) -> &[String] {
        match self {
            Self::One(text) => std::slice::from_ref(text),
            Self::Many(texts) => texts,
        }
    }
}

impl AlgorithmList for Strings {
    fn algorithm_names(&self) -> impl Iterator<Item = &str> + Clone {
        let (string, array) = match self {
            Self::One(value) => (Some(value.as_str()), None),
            Self::Many(values) => (None, Some(values.as_slice())),
        };

        string
            .into_iter()
            .flat_map(|value| value.split(','))
            .chain(array.into_iter().flatten().map(String::as_str))
    }
}

impl<'de> Deserialize<'de> for Strings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{SeqAccess, Visitor};

        use std::fmt;

        struct StringsVisitor;

        impl<'de> Visitor<'de> for StringsVisitor {
            type Value = Strings;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or an array of strings")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(Strings::One(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(Strings::One(value))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::with_capacity(seq.size_hint().unwrap_or(0));

                while let Some(value) = seq.next_element()? {
                    values.push(value);
                }

                Ok(Strings::Many(values))
            }
        }

        deserializer.deserialize_any(StringsVisitor)
    }
}

pub(super) fn deserialize_port<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u16>, D::Error> {
    use serde::de::{Error, Visitor};

    use std::fmt;

    struct PortVisitor;

    impl<'de> Visitor<'de> for PortVisitor {
        type Value = u16;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a port as a string or integer from 0 to 65535")
        }

        fn visit_str<E: Error>(self, value: &str) -> Result<u16, E> {
            value.parse().map_err(E::custom)
        }

        fn visit_i64<E: Error>(self, value: i64) -> Result<u16, E> {
            u16::try_from(value).map_err(E::custom)
        }

        fn visit_u64<E: Error>(self, value: u64) -> Result<u16, E> {
            u16::try_from(value).map_err(E::custom)
        }
    }

    deserializer.deserialize_any(PortVisitor).map(Some)
}

fn bool_str(value: &str) -> Option<bool> {
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no") {
        Some(false)
    } else {
        None
    }
}

pub(super) fn deserialize_bool<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<bool>, D::Error> {
    use serde::de::{Error, Unexpected, Visitor};

    use std::fmt;

    struct BoolVisitor;

    impl<'de> Visitor<'de> for BoolVisitor {
        type Value = bool;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a boolean or one of true, false, yes, no")
        }

        fn visit_bool<E>(self, value: bool) -> Result<bool, E> {
            Ok(value)
        }

        fn visit_str<E: Error>(self, value: &str) -> Result<bool, E> {
            bool_str(value).ok_or_else(|| E::invalid_value(Unexpected::Str(value), &self))
        }
    }

    deserializer.deserialize_any(BoolVisitor).map(Some)
}

pub(super) fn deserialize_tty<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<openrussh_client::config::RequestTty>, D::Error> {
    use openrussh_client::config::RequestTty;

    use serde::de::{Error, Unexpected, Visitor};

    use std::fmt;

    struct TtyVisitor;

    impl<'de> Visitor<'de> for TtyVisitor {
        type Value = RequestTty;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a boolean or one of true, false, yes, no, auto, force")
        }

        fn visit_bool<E>(self, value: bool) -> Result<RequestTty, E> {
            Ok(if value {
                RequestTty::IfTty
            } else {
                RequestTty::Never
            })
        }

        fn visit_str<E: Error>(self, value: &str) -> Result<RequestTty, E> {
            if let Some(value) = bool_str(value) {
                self.visit_bool(value)
            } else if value.eq_ignore_ascii_case("auto") {
                Ok(RequestTty::ForShell)
            } else if value.eq_ignore_ascii_case("force") {
                Ok(RequestTty::Always)
            } else {
                Err(E::invalid_value(Unexpected::Str(value), &self))
            }
        }
    }

    deserializer.deserialize_any(TtyVisitor).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    use mlua::serde::Deserializer;
    use mlua::{DeserializeOptions, Lua, Value};
    use serde::de::DeserializeOwned;

    use std::collections::BTreeMap;

    fn decode<T: DeserializeOwned>(source: &str) -> mlua::Result<T> {
        let lua = Lua::new();
        let value = lua.load(source).eval::<Value>()?;
        let options = DeserializeOptions::new()
            .encode_empty_tables_as_array(true)
            .detect_mixed_tables(true);
        T::deserialize(Deserializer::new_with_options(value, options))
    }

    #[derive(Deserialize)]
    struct Input {
        text: String,
        strings: Strings,
        hosts: BTreeMap<String, String>,
    }

    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct Flags {
        #[serde(deserialize_with = "deserialize_bool")]
        enabled: Option<bool>,
        #[serde(deserialize_with = "deserialize_tty")]
        tty: Option<openrussh_client::config::RequestTty>,
        #[serde(deserialize_with = "deserialize_port")]
        port: Option<u16>,
    }

    #[test]
    fn decoded_strings_and_keys_outlive_the_lua_state() -> mlua::Result<()> {
        let input: Input = decode(
            r#"return {text = "line\nbreak", strings = {'first', 'second'}, hosts = {name = 'host'}}"#,
        )?;

        assert_eq!(input.text, "line\nbreak");
        assert_eq!(input.strings.as_slice(), &["first", "second"]);
        assert_eq!(input.hosts.get("name").map(String::as_str), Some("host"));

        let input: Input = decode("return {text = '', strings = 'single', hosts = {}}")?;
        assert_eq!(input.strings.as_slice(), &["single"]);
        Ok(())
    }

    #[test]
    fn flags_accept_lua_booleans_and_case_insensitive_names() -> mlua::Result<()> {
        use openrussh_client::config::RequestTty::{Always, ForShell, IfTty, Never};

        for (value, enabled, tty) in [
            ("true", true, IfTty),
            ("false", false, Never),
            ("'true'", true, IfTty),
            ("'false'", false, Never),
            ("'yes'", true, IfTty),
            ("'no'", false, Never),
            ("'TRUE'", true, IfTty),
            ("'No'", false, Never),
        ] {
            let flags: Flags = decode(&format!("return {{enabled = {value}, tty = {value}}}"))?;
            assert_eq!(flags.enabled, Some(enabled), "{value}");
            assert_eq!(flags.tty, Some(tty), "{value}");
        }
        for (value, expected) in [("auto", ForShell), ("Force", Always)] {
            let flags: Flags = decode(&format!("return {{tty = '{value}'}}"))?;
            assert_eq!(flags.tty, Some(expected));
        }
        let omitted: Flags = decode("return {}")?;
        assert_eq!(omitted.enabled, None);
        assert_eq!(omitted.tty, None);
        assert_eq!(omitted.port, None);
        Ok(())
    }

    #[test]
    fn ports_accept_decimal_strings_and_lua_integers() -> mlua::Result<()> {
        for (value, expected) in [("'2222'", 2222), ("2222", 2222), ("0", 0), ("65535", 65535)] {
            let flags: Flags = decode(&format!("return {{port = {value}}}"))?;
            assert_eq!(flags.port, Some(expected));
        }
        Ok(())
    }

    #[test]
    fn scalar_visitors_reject_invalid_lua_types_and_values() {
        for source in [
            "return {enabled = 1}",
            "return {enabled = {}}",
            "return {enabled = 'auto'}",
            "return {enabled = 'enabled'}",
            "return {tty = 1}",
            "return {tty = 0.0}",
            "return {tty = {}}",
            "return {tty = 'always'}",
            "return {tty = ''}",
            "return {port = true}",
            "return {port = 22.0}",
            "return {port = 22.5}",
            "return {port = -1}",
            "return {port = 65536}",
            "return {port = '65536'}",
        ] {
            assert!(decode::<Flags>(source).is_err(), "{source}");
        }
        for source in [
            "return 1",
            "return false",
            "return {1}",
            "return {'key', extra = 'value'}",
        ] {
            assert!(decode::<Strings>(source).is_err(), "{source}");
        }
    }

    #[test]
    fn non_utf8_lua_values_and_map_keys_are_rejected() {
        for source in [
            "return {text = string.char(255), strings = {}, hosts = {}}",
            "return {text = '', strings = string.char(255), hosts = {}}",
            "return {text = '', strings = {string.char(255)}, hosts = {}}",
            "return {text = '', strings = {}, hosts = {[string.char(255)] = 'host'}}",
            "return {text = '', strings = {}, hosts = {name = string.char(255)}}",
        ] {
            assert!(decode::<Input>(source).is_err(), "{source}");
        }
    }
}
