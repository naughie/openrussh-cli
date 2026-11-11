use serde::{Deserialize, Deserializer};

use std::borrow::{Borrow, Cow};

#[derive(Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub(super) struct Text<'src>(#[serde(borrow)] Cow<'src, str>);

impl Text<'_> {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Text<'_> {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

pub(super) enum Strings<'src> {
    One(Text<'src>),
    Many(Vec<Text<'src>>),
}

impl<'src> Strings<'src> {
    pub(super) fn as_slice(&self) -> &[Text<'src>] {
        match self {
            Self::One(text) => std::slice::from_ref(text),
            Self::Many(texts) => texts,
        }
    }
}

impl<'de: 'src, 'src> Deserialize<'de> for Strings<'src> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{SeqAccess, Visitor};

        use std::fmt;
        use std::marker::PhantomData;

        struct StringsVisitor<'src>(PhantomData<&'src str>);

        impl<'de: 'src, 'src> Visitor<'de> for StringsVisitor<'src> {
            type Value = Strings<'src>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or an array of strings")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(Strings::One(Text(Cow::Owned(value.to_owned()))))
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E> {
                Ok(Strings::One(Text(Cow::Borrowed(value))))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(Strings::One(Text(Cow::Owned(value))))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::with_capacity(seq.size_hint().unwrap_or(0));

                while let Some(value) = seq.next_element()? {
                    values.push(value);
                }

                Ok(Strings::Many(values))
            }
        }

        deserializer.deserialize_any(StringsVisitor(PhantomData))
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

    use std::collections::BTreeMap;

    #[derive(Deserialize)]
    struct Input<'src> {
        #[serde(borrow)]
        text: Text<'src>,
        #[serde(borrow)]
        strings: Strings<'src>,
        #[serde(borrow)]
        hosts: BTreeMap<Text<'src>, Text<'src>>,
    }

    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct Flags {
        #[serde(deserialize_with = "deserialize_bool")]
        enabled: Option<bool>,
        #[serde(deserialize_with = "deserialize_tty")]
        tty: Option<openrussh_client::config::RequestTty>,
    }

    #[test]
    fn booleans_accept_native_and_text_values() {
        for (value, expected) in [
            ("true", true),
            ("false", false),
            ("'true'", true),
            ("'false'", false),
            ("'yes'", true),
            ("'no'", false),
            ("'TRUE'", true),
            ("'No'", false),
        ] {
            let flags: Flags = ::toml::from_str(&format!("enabled = {value}")).unwrap();

            assert_eq!(flags.enabled, Some(expected), "{value}");
            assert_eq!(flags.tty, None);
        }
    }

    #[test]
    fn tty_accepts_booleans_and_named_modes() {
        use openrussh_client::config::RequestTty::{Always, ForShell, IfTty, Never};

        for (value, expected) in [
            ("true", IfTty),
            ("false", Never),
            ("'true'", IfTty),
            ("'false'", Never),
            ("'yes'", IfTty),
            ("'no'", Never),
            ("'auto'", ForShell),
            ("'force'", Always),
            ("'YES'", IfTty),
            ("'Force'", Always),
        ] {
            let flags: Flags = ::toml::from_str(&format!("tty = {value}")).unwrap();

            assert_eq!(flags.tty, Some(expected), "{value}");
            assert_eq!(flags.enabled, None);
        }
    }

    #[test]
    fn omitted_flags_stay_unset_and_invalid_values_are_rejected() {
        let flags: Flags = ::toml::from_str("").unwrap();

        assert_eq!(flags.enabled, None);
        assert_eq!(flags.tty, None);

        for input in [
            "enabled = 1",
            "enabled = 0",
            "enabled = []",
            "enabled = {}",
            "enabled = 'auto'",
            "enabled = 'force'",
            "enabled = 'enabled'",
            "tty = 1",
            "tty = 0.0",
            "tty = []",
            "tty = {}",
            "tty = 'always'",
            "tty = ''",
        ] {
            assert!(::toml::from_str::<Flags>(input).is_err(), "{input}");
        }
    }

    #[test]
    fn plain_values_array_entries_and_keys_borrow_the_input() {
        let input: Input<'_> = ::toml::from_str(
            "text = 'plain'\nstrings = ['first', 'second']\n[hosts]\nname = 'host'",
        )
        .unwrap();

        assert!(matches!(input.text.0, Cow::Borrowed("plain")));
        assert!(
            input
                .strings
                .as_slice()
                .iter()
                .all(|text| matches!(text.0, Cow::Borrowed(_)))
        );

        let (name, host) = input.hosts.first_key_value().unwrap();

        assert!(matches!(name.0, Cow::Borrowed("name")));
        assert!(matches!(host.0, Cow::Borrowed("host")));
        assert_eq!(input.hosts.get("name").unwrap().as_str(), "host");
    }

    #[test]
    fn escaped_values_and_keys_are_owned_and_decoded() {
        let input: Input<'_> = ::toml::from_str(
            "text = \"line\\nbreak\"\nstrings = \"tab\\tvalue\"\n[hosts]\n\"na\\u006de\" = \"ho\\u0073t\"",
        )
        .unwrap();

        assert!(matches!(input.text.0, Cow::Owned(_)));
        assert_eq!(input.text.as_str(), "line\nbreak");
        assert!(matches!(input.strings.as_slice()[0].0, Cow::Owned(_)));
        assert_eq!(input.strings.as_slice()[0].as_str(), "tab\tvalue");

        let (name, host) = input.hosts.first_key_value().unwrap();

        assert!(matches!(name.0, Cow::Owned(_)));
        assert_eq!(name.as_str(), "name");
        assert_eq!(host.as_str(), "host");
    }
}
