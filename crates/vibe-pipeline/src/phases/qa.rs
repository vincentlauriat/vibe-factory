//! `qa`: independent review of the implementation.

use vibe_core::{
    AgentRole, ErrorKind, Phase, QaIssue, QaReport, QaVerdict, Result, Severity, TaskStatus,
};

use super::{lenient_string, lenient_strings};
use crate::context::{PhaseResult, RunContext, Transition};
use crate::kickoff::{CHANGES_MAX_CHARS, KickoffData, kickoff_for, truncate_middle};

/// One issue as written by the reviewer (lenient).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DraftIssue {
    /// `low`, `medium`, `high`, `critical` (default `medium`).
    #[serde(default, deserialize_with = "lenient_string")]
    pub severity: String,
    /// Short title.
    #[serde(default, deserialize_with = "lenient_string")]
    pub title: String,
    /// Explanation.
    #[serde(default, deserialize_with = "lenient_string")]
    pub detail: String,
    /// Requirement id.
    #[serde(default)]
    pub requirement: Option<String>,
    /// File path.
    #[serde(default)]
    pub file: Option<String>,
    /// Line number.
    #[serde(default)]
    pub line: Option<u32>,
    /// Suggested fix.
    #[serde(default)]
    pub suggested_fix: Option<String>,
}

/// Structured output of the QA reviewer.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QaOutput {
    /// `approved`, `changes_requested` or `inconclusive` (aliases accepted).
    #[serde(default, deserialize_with = "lenient_string")]
    pub verdict: String,
    /// What was checked.
    #[serde(default, deserialize_with = "lenient_string")]
    pub summary: String,
    /// Issues found.
    #[serde(default)]
    pub issues: Vec<DraftIssue>,
    /// Commands run (informational; appended to the summary).
    #[serde(default, deserialize_with = "lenient_strings")]
    pub commands: Vec<String>,
}

/// Parse a verdict, tolerating aliases; unknown values are inconclusive.
#[must_use]
pub fn parse_verdict(s: &str) -> QaVerdict {
    match s
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_")
        .as_str()
    {
        "approved" | "approve" | "pass" | "passed" | "accepted" | "ok" => QaVerdict::Approved,
        "changes_requested" | "request_changes" | "changes_required" | "rejected" | "fail"
        | "failed" | "needs_changes" | "needs_work" => QaVerdict::ChangesRequested,
        _ => QaVerdict::Inconclusive,
    }
}

fn parse_severity(s: &str) -> Severity {
    match s.trim().to_ascii_lowercase().as_str() {
        "low" | "minor" | "info" | "trivial" => Severity::Low,
        "high" | "major" | "error" => Severity::High,
        "critical" | "blocker" | "blocking" => Severity::Critical,
        _ => Severity::Medium,
    }
}

impl QaOutput {
    /// Convert into a [`QaReport`] for `round`.
    #[must_use]
    pub fn into_report(self, task_id: vibe_core::TaskId, round: u32) -> QaReport {
        let mut summary = self.summary.trim().to_string();
        if !self.commands.is_empty() {
            summary.push_str("\n\nCommands: ");
            summary.push_str(&self.commands.join("; "));
        }
        QaReport {
            task_id,
            round,
            verdict: parse_verdict(&self.verdict),
            summary,
            issues: self
                .issues
                .into_iter()
                .filter(|i| !i.title.trim().is_empty() || !i.detail.trim().is_empty())
                .map(|i| QaIssue {
                    severity: parse_severity(&i.severity),
                    title: if i.title.trim().is_empty() {
                        i.detail
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .to_string()
                    } else {
                        i.title.trim().to_string()
                    },
                    detail: i.detail,
                    requirement: i.requirement,
                    file: i.file,
                    line: i.line,
                    suggested_fix: i.suggested_fix,
                })
                .collect(),
        }
    }
}

/// Summary of earlier QA rounds for the reviewer.
fn earlier_rounds(reports: &[QaReport]) -> String {
    reports
        .iter()
        .map(|r| {
            let titles: Vec<&str> = r.issues.iter().map(|i| i.title.as_str()).collect();
            format!(
                "- Round {}: {:?}{}",
                r.round,
                r.verdict,
                if titles.is_empty() {
                    String::new()
                } else {
                    format!(" — issues: {}", titles.join("; "))
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run one QA round, save its report and decide what comes next:
///
/// * approved → continue (merge);
/// * changes requested → `fix`, unless the profile has no fix phase or
///   `max_qa_rounds` reviews already ran in this invocation, in which case
///   the run pauses with status [`TaskStatus::Review`];
/// * inconclusive (or a reviewer that fails to answer) → pause with status
///   [`TaskStatus::Review`].
pub async fn run_qa(ctx: &mut RunContext) -> Result<PhaseResult> {
    ctx.state.qa_round += 1;
    ctx.qa_rounds_this_invocation += 1;
    let round = ctx.state.qa_round;
    let changes = match ctx.workspace_provider.changes(&ctx.workspace).await {
        Ok(c) => truncate_middle(&c, CHANGES_MAX_CHARS),
        Err(e) => format!("(changes unavailable: {e})"),
    };
    let previous = ctx.store.load_qa_reports(ctx.task.id).await?;
    let prior = earlier_rounds(&previous);
    let spec_text = ctx.spec_text();
    let plan_text = ctx.plan_text();
    let progress = ctx.progress_text().await?;
    let memory = ctx.memory_text().await?;
    let agent = ctx.agent_spec(&AgentRole::QaReviewer)?;
    let runner = ctx
        .runner(&agent)?
        .var("spec", spec_text.clone())
        .var("plan", plan_text.clone())
        .var("progress", progress.clone())
        .var("memory", memory.clone())
        .var("prior_context", prior.clone())
        .var("changes", changes.clone());
    let mut data = KickoffData::new(&ctx.task);
    data.spec = &spec_text;
    data.plan = &plan_text;
    data.progress = &progress;
    data.memory = &memory;
    data.prior_context = &prior;
    data.changes = &changes;
    let msg = kickoff_for(&agent.role, &data, &agent.system_prompt);
    let report = match vibe_agents::run_structured::<QaOutput>(&runner, &agent, msg).await {
        Ok((out, outcome)) => {
            ctx.record(&outcome);
            out.into_report(ctx.task.id, round)
        }
        Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
        Err(e) => QaReport {
            task_id: ctx.task.id,
            round,
            verdict: QaVerdict::Inconclusive,
            summary: format!("The reviewer did not produce a report: {}", e.message),
            issues: Vec::new(),
        },
    };
    ctx.store.save_qa_report(&report).await?;
    ctx.artefact_written(vibe_core::Artefact::QaReport {
        round: report.round,
    })
    .await;
    let summary = format!(
        "round {round}: {:?} ({} issue(s))",
        report.verdict,
        report.issues.len()
    );
    ctx.note(&format!("QA {summary}.\n\n{}", report.summary))
        .await?;

    let max = ctx.config.pipeline.max_qa_rounds.max(1);
    let result = PhaseResult::ok(Phase::Qa, summary);
    Ok(match report.verdict {
        QaVerdict::Approved => result,
        QaVerdict::ChangesRequested if !ctx.profile.has(Phase::Fix) => {
            result.with_success(false).then(Transition::Stop {
                status: TaskStatus::Review,
                reason: format!(
                    "QA requested changes in round {round} and the profile has no fix phase"
                ),
                pause: true,
            })
        }
        QaVerdict::ChangesRequested if ctx.qa_rounds_this_invocation >= max => {
            result.with_success(false).then(Transition::Stop {
                status: TaskStatus::Review,
                reason: format!("QA did not approve after {max} round(s); human review needed"),
                pause: true,
            })
        }
        QaVerdict::ChangesRequested => result
            .with_success(false)
            .then(Transition::Goto { phase: Phase::Fix }),
        QaVerdict::Inconclusive => result.with_success(false).then(Transition::Stop {
            status: TaskStatus::Review,
            reason: format!("QA round {round} was inconclusive; human review needed"),
            pause: true,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_aliases() {
        assert_eq!(parse_verdict("Approved"), QaVerdict::Approved);
        assert_eq!(
            parse_verdict("changes requested"),
            QaVerdict::ChangesRequested
        );
        assert_eq!(parse_verdict("???"), QaVerdict::Inconclusive);
    }

    #[test]
    fn output_to_report() {
        let o: QaOutput = serde_json::from_str(
            r#"{"verdict":"changes_requested","summary":"s","issues":[
                {"severity":"major","title":"Bug","detail":"d","line":null},
                {"detail":"untitled problem\nmore"},
                {}
            ]}"#,
        )
        .unwrap();
        let r = o.into_report(vibe_core::TaskId::new(), 2);
        assert_eq!(r.round, 2);
        assert_eq!(r.issues.len(), 2);
        assert_eq!(r.issues[0].severity, Severity::High);
        assert_eq!(r.issues[1].title, "untitled problem");
        assert_eq!(r.issues[1].severity, Severity::Medium);
    }
}
