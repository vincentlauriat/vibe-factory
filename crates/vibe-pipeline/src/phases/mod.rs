//! One module per pipeline phase. Each exposes
//! `run_<phase>(&mut RunContext) -> Result<PhaseResult>`.

pub mod assess;
pub mod build;
pub mod fix;
pub mod merge;
pub mod plan;
pub mod qa;
pub mod spec;

pub use assess::run_assess;
pub use build::{CoderReport, run_build};
pub use fix::{FixerOutput, repeated_issue, run_fix};
pub use merge::run_merge;
pub use plan::{PlannerOutput, plan_from_output, run_plan};
pub use qa::{QaOutput, run_qa};
pub use spec::{CriticOutput, SpecDraft, run_spec};

/// Deserialize a list of strings, also accepting a single string, numbers
/// and `null`.
pub(crate) fn lenient_strings<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(items) => items.into_iter().filter_map(scalar).collect(),
        other => scalar(other).into_iter().collect(),
    })
}

fn scalar(v: serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Deserialize a string, accepting `null` and non-string scalars.
pub(crate) fn lenient_string<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    })
}

/// Pretty JSON of a value, for prior context.
pub(crate) fn pretty<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}
