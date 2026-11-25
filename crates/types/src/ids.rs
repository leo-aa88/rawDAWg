//! Stable identifiers.
//!
//! Every persistent object has a UUID generated once and preserved across
//! edits, saves and migrations. UI indices are never persistent identity.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a fresh random (v4) identifier.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
            /// Build from raw 128-bit value (fixtures/tests).
            pub const fn from_u128(v: u128) -> Self {
                Self(Uuid::from_u128(v))
            }
            /// The nil identifier (never valid for a persisted object).
            pub const fn nil() -> Self {
                Self(Uuid::nil())
            }
            pub fn is_nil(&self) -> bool {
                self.0.is_nil()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

uuid_id!(
    #[doc = "Identifies a project."]
    ProjectId
);
uuid_id!(
    #[doc = "Identifies a channel-rack channel (instrument or sample slot)."]
    ChannelId
);
uuid_id!(
    #[doc = "Identifies a reusable pattern."]
    PatternId
);
uuid_id!(
    #[doc = "Identifies a playlist track."]
    TrackId
);
uuid_id!(
    #[doc = "Identifies a playlist clip (pattern/audio/automation)."]
    ClipId
);
uuid_id!(
    #[doc = "Identifies a note inside a pattern lane."]
    NoteId
);
uuid_id!(
    #[doc = "Identifies a mixer channel, bus or the master."]
    MixerChannelId
);
uuid_id!(
    #[doc = "Identifies a processor instance (native or hosted plugin)."]
    InstanceId
);
uuid_id!(
    #[doc = "Identifies an automation curve."]
    CurveId
);
uuid_id!(
    #[doc = "Identifies an automation point."]
    PointId
);
uuid_id!(
    #[doc = "Identifies a graph node in the advanced modular editor."]
    NodeId
);
uuid_id!(
    #[doc = "Identifies a MIDI mapping entry."]
    MappingId
);
uuid_id!(
    #[doc = "Identifies a controller/expression event inside a lane."]
    ControllerEventId
);
uuid_id!(
    #[doc = "Identifies a native preset."]
    PresetId
);

/// Content-addressed asset identifier: lowercase hex SHA-256 digest.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetId(String);

impl AssetId {
    /// Construct from a digest. Returns `None` if the string is not 64 lowercase hex chars.
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) { Some(Self(hex.to_owned())) } else { None }
    }
    /// Construct from raw digest bytes.
    pub fn from_digest(bytes: &[u8; 32]) -> Self {
        let mut s = String::with_capacity(64);
        for b in bytes {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        Self(s)
    }
    pub fn as_hex(&self) -> &str {
        &self.0
    }
    /// Validate a deserialized value.
    pub fn is_valid(&self) -> bool {
        Self::from_hex(&self.0).is_some()
    }
}

impl fmt::Debug for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetId({})", &self.0[..12.min(self.0.len())])
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable string identifier of a processor type, e.g. `native.synth`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TypeId(pub String);

impl TypeId {
    pub fn new(s: &str) -> Self {
        Self(s.to_owned())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TypeId({})", self.0)
    }
}

impl fmt::Display for TypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Native processor type identifiers (stable internal contracts).
pub mod native_types {
    pub const SYNTH: &str = "native.synth";
    pub const SAMPLER: &str = "native.sampler";
    pub const SF2: &str = "native.sf2";
    pub const FX_PREFIX: &str = "native.fx.";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_json_as_plain_strings() {
        let id = ChannelId::from_u128(0x1234);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"00000000-0000-0000-0000-000000001234\"");
        let back: ChannelId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn asset_id_requires_lowercase_hex_sha256() {
        let good = "a".repeat(64);
        assert!(AssetId::from_hex(&good).is_some());
        assert!(AssetId::from_hex(&"A".repeat(64)).is_none());
        assert!(AssetId::from_hex(&"a".repeat(63)).is_none());
        let bytes = [0xabu8; 32];
        assert_eq!(AssetId::from_digest(&bytes).as_hex(), "ab".repeat(32));
    }

    #[test]
    fn fresh_ids_are_unique() {
        let a = NoteId::new();
        let b = NoteId::new();
        assert_ne!(a, b);
        assert!(!a.is_nil());
    }
}
