use serde::de::{SeqAccess, Visitor};
use serde::{Deserializer, Serializer};
use std::fmt;

pub fn serialize<S>(value: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use serde::ser::SerializeTuple;
    let mut tuple = serializer.serialize_tuple(64)?;
    for byte in value {
        tuple.serialize_element(byte)?;
    }
    tuple.end()
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 64], D::Error>
where
    D: Deserializer<'de>,
{
    struct Fixed64Visitor;

    impl<'de> Visitor<'de> for Fixed64Visitor {
        type Value = [u8; 64];

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("64 bytes")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut out = [0u8; 64];
            for (index, slot) in out.iter_mut().enumerate() {
                *slot = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(index, &self))?;
            }
            Ok(out)
        }
    }

    deserializer.deserialize_tuple(64, Fixed64Visitor)
}
