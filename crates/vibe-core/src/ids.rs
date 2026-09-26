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
}
