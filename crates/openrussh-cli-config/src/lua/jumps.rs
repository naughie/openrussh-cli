use super::profiles::HostFields;

use serde::de::{Error, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use std::fmt;

pub(super) enum Jumps {
    One(Jump),
    Many(Vec<Jump>),
}

pub(super) enum Jump {
    Name(String),
    Host(Box<HostFields>),
}

impl Jumps {
    pub(super) fn as_slice(&self) -> &[Jump] {
        match self {
            Self::One(jump) => std::slice::from_ref(jump),
            Self::Many(jumps) => jumps,
        }
    }
}

struct JumpVisitor;

impl<'de> Visitor<'de> for JumpVisitor {
    type Value = Jump;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a jump-host string or host record")
    }

    fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Jump::Name(value.to_owned()))
    }

    fn visit_string<E: Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(Jump::Name(value))
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

impl<'de> Deserialize<'de> for Jump {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JumpVisitor)
    }
}

impl<'de> Deserialize<'de> for Jumps {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JumpsVisitor;

        impl<'de> Visitor<'de> for JumpsVisitor {
            type Value = Jumps;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a jump-host string, host record, or array of jumps")
            }

            fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
                JumpVisitor.visit_str(value).map(Jumps::One)
            }

            fn visit_string<E: Error>(self, value: String) -> Result<Self::Value, E> {
                JumpVisitor.visit_string(value).map(Jumps::One)
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                JumpVisitor.visit_map(map).map(Jumps::One)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut jumps = Vec::with_capacity(seq.size_hint().unwrap_or(0));

                while let Some(jump) = seq.next_element()? {
                    jumps.push(jump);
                }

                Ok(Jumps::Many(jumps))
            }
        }

        deserializer.deserialize_any(JumpsVisitor)
    }
}
