use std::fmt;

use serde::de::{Deserialize, Deserializer, Visitor};
use serde::Serializer;

pub const CALLBACK_SLOT: &str = "amux.callback";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallbackId(pub usize);

pub mod slot {
    use super::*;

    pub fn serialize<S: Serializer>(
        id: &Option<CallbackId>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(CALLBACK_SLOT, &id.map(|id| id.0))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<CallbackId>, D::Error> {
        deserializer.deserialize_newtype_struct(CALLBACK_SLOT, SlotVisitor)
    }
}

struct SlotVisitor;

impl<'de> Visitor<'de> for SlotVisitor {
    type Value = Option<CallbackId>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a function")
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        Option::<usize>::deserialize(deserializer).map(|id| id.map(CallbackId))
    }
}
