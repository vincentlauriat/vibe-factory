//! QA reports.

use crate::ids::TaskId;

/// Outcome of a QA review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaVerdict {
    /// Every requirement is met.
    Approved,
    /// Issues must be fixed before approval.
    ChangesRequested,
    /// The reviewer could not complete the review.
    Inconclusive,
}

/// Severity of a QA issue.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Cosmetic.
    Low,
    /// Should be fixed.
    Medium,
    /// Breaks a requirement.
    High,
    /// Data loss, security, build broken.
    Critical,
}

/// One issue found by QA.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QaIssue {
    /// Severity.
    pub severity: Severity,
    /// Short title.
    pub title: String,
    /// Detailed explanation.
    pub detail: String,
    /// Related requirement id, if any.
    #[serde(default)]
    pub requirement: Option<String>,
    /// File path, if any.
    #[serde(default)]
    pub file: Option<String>,
    /// Line number, if any.
    #[serde(default)]
    pub line: Option<u32>,
    /// Suggested fix.
    #[serde(default)]
    pub suggested_fix: Option<String>,
}

/// The result of a QA review.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QaReport {
    /// Task reviewed.
    pub task_id: TaskId,
    /// Review round (1-based).
    pub round: u32,
    /// Verdict.
    pub verdict: QaVerdict,
    /// Summary of what was checked.
    #[serde(default)]
    pub summary: String,
    /// Issues found.
    #[serde(default)]
    pub issues: Vec<QaIssue>,
}

impl QaReport {
    /// Issues at or above the given severity.
    pub fn issues_at_least(&self, severity: Severity) -> impl Iterator<Item = &QaIssue> {
        self.issues.iter().filter(move |i| i.severity >= severity)
    }

    /// Whether the report blocks completion.
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.verdict != QaVerdict::Approved
    }

    /// Render as markdown.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut md = format!(
            "# QA report (round {})\n\n**Verdict:** {:?}\n\n",
            self.round, self.verdict
        );
        if !self.summary.is_empty() {
            md.push_str(&self.summary);
            md.push_str("\n\n");
        }
        if !self.issues.is_empty() {
            md.push_str("## Issues\n\n");
            for i in &self.issues {
                md.push_str(&format!(
                    "### [{:?}] {}\n\n{}\n",
                    i.severity, i.title, i.detail
                ));
                if let Some(f) = &i.file {
                    md.push_str(&format!("\n- File: `{}`", f));
                    if let Some(l) = i.line {
                        md.push_str(&format!(":{l}"));
                    }
                    md.push('\n');
                }
                if let Some(fix) = &i.suggested_fix {
                    md.push_str(&format!("- Suggested fix: {fix}\n"));
                }
                md.push('\n');
            }
        }
        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_filter() {
        let r = QaReport {
            task_id: TaskId::new(),
            round: 1,
            verdict: QaVerdict::ChangesRequested,
            summary: String::new(),
            issues: vec![
                QaIssue {
                    severity: Severity::Low,
                    title: "a".into(),
                    detail: String::new(),
                    requirement: None,
                    file: None,
                    line: None,
                    suggested_fix: None,
                },
                QaIssue {
                    severity: Severity::Critical,
                    title: "b".into(),
                    detail: String::new(),
                    requirement: None,
                    file: Some("x.rs".into()),
                    line: Some(3),
                    suggested_fix: None,
                },
            ],
        };
        assert_eq!(r.issues_at_least(Severity::High).count(), 1);
        assert!(r.is_blocking());
        assert!(r.to_markdown().contains("`x.rs`:3"));
    }
}
