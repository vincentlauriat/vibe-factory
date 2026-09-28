//! Strongly typed identifiers.

use std::fmt;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
                 serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(uuid::Uuid);

        impl $name {
            /// Generate a fresh random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            /// Parse an identifier from its string representation.
            pub fn parse(s: &str) -> Result<Self, uuid::Error> {
                uuid::Uuid::parse_str(s).map(Self)
            }

            /// Short 8-character prefix, convenient for display.
            #[must_use]
            pub fn short(&self) -> String {
                self.0.to_string()[..8].to_string()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
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
                Self::parse(s)
            }
        }
    };
}

id_type!(
    /// Identifier of a [`crate::Task`].
    TaskId
);
id_type!(
    /// Identifier of a [`crate::Subtask`].
    SubtaskId
);
id_type!(
    /// Identifier of one agent conversation.
    SessionId
);
id_type!(
    /// Identifier of one pipeline execution.
    RunId
);

/// Identifier of one tool call, pairing [`crate::Event::ToolCalled`] with
/// its [`crate::Event::ToolReturned`]: 48 random bits, written as 12
/// lower-case hex digits.
///
/// Events recorded before call ids existed read as [`CallId::nil`]; pair
/// those by order instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct CallId(u64);

impl CallId {
    const MASK: u64 = 0xffff_ffff_ffff;

    /// Generate a fresh random identifier (never nil).
    #[must_use]
    pub fn new() -> Self {
        let bits = uuid::Uuid::new_v4().as_u64_pair().0 & Self::MASK;
        Self(bits.max(1))
    }

    /// The identifier of calls recorded without one.
    #[must_use]
    pub const fn nil() -> Self {
        Self(0)
    }

    /// Whether this is [`CallId::nil`].
    #[must_use]
    pub const fn is_nil(&self) -> bool {
        self.0 == 0
    }

    /// Parse an identifier from its representation: exactly 12 hex digits.
    pub fn parse(s: &str) -> Result<Self, InvalidCallId> {
        if s.len() != 12 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(InvalidCallId(s.to_string()));
        }
        u64::from_str_radix(s, 16)
            .map(Self)
            .map_err(|_| InvalidCallId(s.to_string()))
    }
}

/// A string that is not a [`CallId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidCallId(String);

impl fmt::Display for InvalidCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid call id `{}`: expected 12 hex digits", self.0)
    }
}

impl std::error::Error for InvalidCallId {}

impl fmt::Display for CallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:012x}", self.0)
    }
}

impl std::str::FromStr for CallId {
    type Err = InvalidCallId;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl serde::Serialize for CallId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for CallId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let id = TaskId::new();
        let parsed: TaskId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
        assert_eq!(id.short().len(), 8);
    }

    #[test]
    fn serde_transparent() {
        let id = RunId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.starts_with('"'));
        let back: RunId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn call_id_serialises_as_short_hex_string() {
        let id = CallId::new();
        assert!(!id.is_nil());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json.len(), 14, "12 hex digits between quotes: {json}");
        assert!(json[1..13].chars().all(|c| c.is_ascii_hexdigit()));
        let back: CallId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        assert_eq!(id.to_string().parse::<CallId>().unwrap(), id);
        assert_eq!(CallId::default(), CallId::nil());
        assert_eq!(CallId::nil().to_string(), "000000000000");
        assert!(serde_json::from_str::<CallId>("\"not hex\"").is_err());
        assert_eq!(
            "00000000ABCD".parse::<CallId>().unwrap().to_string(),
            "00000000abcd"
        );
        for bad in [
            "",
            "abc",
            "+0000000000a",
            "0000000000000",
            "00000000000g",
            "-00000000001",
        ] {
            let err = bad.parse::<CallId>().unwrap_err();
            assert!(err.to_string().contains("12 hex digits"), "{bad}: {err}");
        }
        assert_ne!(CallId::new(), CallId::new());
    }
}
