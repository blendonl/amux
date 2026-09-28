use std::fmt::{self, Write as _};
use std::str::FromStr;

use anyhow::{bail, Result};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const PUBLIC_KEY_LEN: usize = 32;
const FINGERPRINT_BYTES: usize = 8;
const FINGERPRINT_GROUP: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicKey(pub [u8; PUBLIC_KEY_LEN]);

impl PublicKey {
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    pub fn fingerprint(&self) -> String {
        self.0[..FINGERPRINT_BYTES]
            .chunks(FINGERPRINT_GROUP)
            .map(hex)
            .collect::<Vec<_>>()
            .join(":")
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.fingerprint())
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_hex())
    }
}

impl FromStr for PublicKey {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        if text.len() != PUBLIC_KEY_LEN * 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!(
                "{text:?} is not a {} digit hex public key",
                PUBLIC_KEY_LEN * 2
            );
        }
        let mut key = [0; PUBLIC_KEY_LEN];
        for (byte, pair) in key.iter_mut().zip(text.as_bytes().chunks(2)) {
            let pair = std::str::from_utf8(pair)?;
            *byte = u8::from_str_radix(pair, 16)?;
        }
        Ok(Self(key))
    }
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.to_hex())
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let text = String::deserialize(deserializer)?;
            text.parse().map_err(D::Error::custom)
        } else {
            <[u8; PUBLIC_KEY_LEN]>::deserialize(deserializer).map(Self)
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> PublicKey {
        let mut bytes = [0; PUBLIC_KEY_LEN];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = index as u8 * 7;
        }
        PublicKey(bytes)
    }

    #[test]
    fn a_key_shows_a_short_fingerprint_and_round_trips_through_hex() {
        let key = key();
        assert_eq!(key.to_string(), "0007:0e15:1c23:2a31");
        assert_eq!(key.to_hex().len(), PUBLIC_KEY_LEN * 2);
        assert_eq!(key.to_hex().parse::<PublicKey>().unwrap(), key);
        assert!("abc".parse::<PublicKey>().is_err());
        assert!("zz".repeat(PUBLIC_KEY_LEN).parse::<PublicKey>().is_err());
    }

    #[test]
    fn a_key_is_raw_bytes_on_the_wire_and_hex_in_text() {
        let key = key();
        let wire = postcard::to_stdvec(&key).unwrap();
        assert_eq!(wire, key.0);
        assert_eq!(postcard::from_bytes::<PublicKey>(&wire).unwrap(), key);

        let text = serde_json::to_string(&key).unwrap();
        assert_eq!(text, format!("\"{}\"", key.to_hex()));
        assert_eq!(serde_json::from_str::<PublicKey>(&text).unwrap(), key);
    }
}
