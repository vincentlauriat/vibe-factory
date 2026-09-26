//! Pipeline phases.

use std::fmt;

/// One stage of the task pipeline.
///
/// The default pipeline runs phases in the order they are declared here, but
/// a pipeline implementation is free to skip or repeat phases (for example a
/// trivial task may skip [`Phase::Spec`], and [`Phase::Fix`] loops back to
/// [`Phase::Qa`]).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Classify the task complexity to choose a pipeline profile.
    Assess,
    /// Gather context, research the codebase and write a specification.
    Spec,
    /// Break the specification into an ordered plan of subtasks.
    Plan,
    /// Implement the subtasks (possibly in parallel).
    Build,
    /// Review the implementation against the acceptance criteria.
    Qa,
    /// Fix the issues reported by QA.
    Fix,
    /// Integrate the isolated workspace back into the main line.
    Merge,
}

impl Phase {
    /// All phases in canonical order.
    pub const ALL: [Phase; 7] = [
        Phase::Assess,
        Phase::Spec,
        Phase::Plan,
        Phase::Build,
        Phase::Qa,
        Phase::Fix,
        Phase::Merge,
    ];

    /// Stable lowercase name, used in configuration files and CLI flags.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Assess => "assess",
            Phase::Spec => "spec",
            Phase::Plan => "plan",
            Phase::Build => "build",
            Phase::Qa => "qa",
            Phase::Fix => "fix",
            Phase::Merge => "merge",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Phase {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Phase::ALL
            .into_iter()
            .find(|p| p.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| crate::Error::config(format!("unknown phase `{s}`")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for p in Phase::ALL {
            assert_eq!(p.as_str().parse::<Phase>().unwrap(), p);
        }
        assert!("nope".parse::<Phase>().is_err());
    }
}
