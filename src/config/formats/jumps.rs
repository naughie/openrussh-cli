use super::HostFields;
use super::values::Text;

use serde::de::{Error, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use std::borrow::Cow;
use std::fmt;
use std::marker::PhantomData;

pub(super) enum Jumps<'src> {
    One(Jump<'src>),
    Many(Vec<Jump<'src>>),
}

pub(super) enum Jump<'src> {
    Name(Text<'src>),
    Host(Box<HostFields<'src>>),
}

impl Jumps<'_> {
    pub(super) fn as_slice(&self) -> &[Jump<'_>] {
        match self {
            Self::One(jump) => std::slice::from_ref(jump),
            Self::Many(jumps) => jumps,
        }
    }
}

struct JumpVisitor<'src>(PhantomData<&'src str>);

impl<'de: 'src, 'src> Visitor<'de> for JumpVisitor<'src> {
    type Value = Jump<'src>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a jump-host string or host record")
    }

    fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Jump::Name(Text::from(Cow::Owned(value.to_owned()))))
    }

    fn visit_borrowed_str<E: Error>(self, value: &'de str) -> Result<Self::Value, E> {
        Ok(Jump::Name(Text::from(Cow::Borrowed(value))))
    }

    fn visit_string<E: Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(Jump::Name(Text::from(Cow::Owned(value))))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        use serde::de::value::MapAccessDeserializer;

        HostFields::deserialize(MapAccessDeserializer::new(map))
            .map(Box::new)
            .map(Jump::Host)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::IgnoredAny;

        if seq.next_element::<IgnoredAny>()?.is_some() {
            return Err(A::Error::custom("a jump must be a string or host record"));
        }

        Ok(Jump::Host(Box::default()))
    }
}

impl<'de: 'src, 'src> Deserialize<'de> for Jump<'src> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JumpVisitor(PhantomData))
    }
}

impl<'de: 'src, 'src> Deserialize<'de> for Jumps<'src> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JumpsVisitor<'src>(PhantomData<&'src str>);

        impl<'de: 'src, 'src> Visitor<'de> for JumpsVisitor<'src> {
            type Value = Jumps<'src>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a jump-host string, host record, or array of jumps")
            }

            fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
                JumpVisitor(PhantomData).visit_str(value).map(Jumps::One)
            }

            fn visit_borrowed_str<E: Error>(self, value: &'de str) -> Result<Self::Value, E> {
                JumpVisitor(PhantomData)
                    .visit_borrowed_str(value)
                    .map(Jumps::One)
            }

            fn visit_string<E: Error>(self, value: String) -> Result<Self::Value, E> {
                JumpVisitor(PhantomData).visit_string(value).map(Jumps::One)
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                JumpVisitor(PhantomData).visit_map(map).map(Jumps::One)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut jumps = Vec::with_capacity(seq.size_hint().unwrap_or(0));

                while let Some(jump) = seq.next_element()? {
                    jumps.push(jump);
                }

                Ok(Jumps::Many(jumps))
            }
        }

        deserializer.deserialize_any(JumpsVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::formats::Profiles;

    #[test]
    fn named_jumps_preserve_borrowing_and_decode_escapes() {
        for value in ["'plain'", r#"['plain', "esc\u0061ped"]"#] {
            let source = format!("[hosts.target]\nproxy-jump = {value}");
            let profiles: Profiles<'_> = ::toml::from_str(&source).unwrap();
            profiles.require_named_jumps().unwrap();
            let jumps = profiles
                .hosts
                .get("target")
                .unwrap()
                .proxy_jump
                .as_ref()
                .unwrap();
            let names = jumps.as_slice();
            let Jump::Name(plain) = &names[0] else {
                panic!("expected a named jump")
            };
            let source_ptrs = source.as_bytes().as_ptr_range();

            assert_eq!(plain.as_str(), "plain");
            assert!(source_ptrs.contains(&plain.as_str().as_ptr()));

            if names.len() == 2 {
                let Jump::Name(escaped) = &names[1] else {
                    panic!("expected a named jump")
                };

                assert_eq!(escaped.as_str(), "escaped");
                assert!(!source_ptrs.contains(&escaped.as_str().as_ptr()));
            }
        }
    }
}
