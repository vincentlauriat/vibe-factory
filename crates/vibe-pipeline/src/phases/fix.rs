//! `fix`: address a pending validation failure or the latest QA report, then rerun QA.

use std::collections::HashSet;

use vibe_core::{AgentRole, Error, Phase, QaReport, Result, TaskStatus};

use super::{lenient_string, lenient_strings};
use crate::context::{PhaseResult, RunContext, Transition};
use crate::kickoff::{KickoffData, kickoff_for, truncate_middle};
use crate::store::MemoryFile;

/// Number of consecutive QA rounds reporting the same issue title after
/// which the pipeline escalates to a human.
pub const ESCALATION_ROUNDS: usize = 3;

/// Final JSON document of the QA fixer.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FixerOutput {
    /// `done` or `failed`.
    #[serde(default, deserialize_with = "lenient_string")]
    pub status: String,
    /// What was changed.
    #[serde(default, deserialize_with = "lenient_string")]
    pub summary: String,
    /// Titles of the fixed issues.
    #[serde(default, deserialize_with = "lenient_strings")]
    pub fixed: Vec<String>,
}

fn norm(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// An issue title (normalised: lowercase, collapsed whitespace) present in
/// each of the last `rounds` reports, if any.
#[must_use]
pub fn repeated_issue(reports: &[QaReport], rounds: usize) -> Option<String> {
    if rounds == 0 || reports.len() < rounds {
        return None;
    }
    let last = &reports[reports.len() - rounds..];
    let sets: Vec<HashSet<String>> = last
        .iter()
        .map(|r| r.issues.iter().map(|i| norm(&i.title)).collect())
        .collect();
    let mut common: Vec<&String> = sets[0]
        .iter()
        .filter(|t| !t.is_empty() && sets[1..].iter().all(|s| s.contains(*t)))
        .collect();
    common.sort();
    common.first().map(|s| (*s).clone())
}

/// Run the QA fixer on the latest report, commit, and go back to QA.
///
/// Escalates instead (pause with status [`TaskStatus::Review`]) when the
/// same issue title was reported in [`ESCALATION_ROUNDS`] consecutive
/// rounds.
pub async fn run_fix(ctx: &mut RunContext) -> Result<PhaseResult> {
    if let Some(index) = ctx.state.pending_validation_fix {
        return run_validation_fix(ctx, index).await;
    }
    let reports = ctx.store.load_qa_reports(ctx.task.id).await?;
    let Some(last) = reports.last().cloned() else {
        return Err(Error::other("no QA report to fix"));
    };
    if let Some(title) = repeated_issue(&reports, ESCALATION_ROUNDS) {
        let reason = format!(
            "issue `{title}` was reported in {ESCALATION_ROUNDS} consecutive QA rounds; escalating to a human"
        );
        ctx.note(&reason).await?;
        ctx.remember(
            MemoryFile::Gotchas,
            &format!("Recurring QA issue that automated fixes did not solve: {title}"),
        )
        .await?;
        return Ok(PhaseResult::ok(Phase::Fix, reason.clone())
            .with_success(false)
            .then(Transition::Stop {
                status: TaskStatus::Review,
                reason,
                pause: true,
            }));
    }

    let qa_report = last.to_markdown();
    let prior: String = reports[..reports.len() - 1]
        .iter()
        .map(|r| {
            let titles: Vec<&str> = r.issues.iter().map(|i| i.title.as_str()).collect();
            format!("- Round {} reported: {}\n", r.round, titles.join("; "))
        })
        .collect();
    run_fixer(
        ctx,
        qa_report,
        prior,
        format!("round {}", last.round),
        format!("vibe: address QA round {}", last.round),
    )
    .await
}

/// Reserve the attempt before calling a model so a crash cannot reset the budget.
async fn run_validation_fix(ctx: &mut RunContext, index: usize) -> Result<PhaseResult> {
    let failure = ctx
        .state
        .validations
        .get(index)
        .filter(|v| !v.passed)
        .ok_or_else(|| Error::other("invalid pending validation failure"))?;
    let max = ctx.config.pipeline.max_validation_fix_attempts;
    if ctx.state.validation_fix_attempts >= max {
        ctx.state.pending_validation_fix = None;
        return Ok(
            PhaseResult::ok(Phase::Fix, "validation fix budget exhausted")
                .with_success(false)
                .then(Transition::Stop {
                    status: TaskStatus::Review,
                    reason: format!(
                        "validation fix budget exhausted ({max} attempts); human review needed"
                    ),
                    pause: true,
                }),
        );
    }
    let report = format!(
        "# Required validation failure\n\nCommand: `{}`\n\nExit code: {}\nTimed out: {}\n\nOutput:\n{}\n\nFix the implementation. Do not change the validation command, configuration or weaken tests. The pipeline will independently rerun every required check after QA.",
        failure.command,
        failure.metadata["exit_code"],
        failure.metadata["timed_out"],
        truncate_middle(&failure.output, 12_000),
    );
    let prior = format!(
        "{} automatic validation fix attempt(s) already started in this run.",
        ctx.state.validation_fix_attempts
    );
    ctx.state.validation_fix_attempts += 1;
    let attempt = ctx.state.validation_fix_attempts;
    ctx.state.touch();
    ctx.store.save_run_state(&ctx.state).await?;
    let result = run_fixer(
        ctx,
        report,
        prior,
        format!("validation attempt {attempt}/{max}"),
        format!("vibe: address required validation attempt {attempt}"),
    )
    .await?;
    // The pipeline persists this together with the next phase (QA).
    ctx.state.pending_validation_fix = None;
    Ok(result)
}

async fn run_fixer(
    ctx: &mut RunContext,
    qa_report: String,
    prior: String,
    label: String,
    commit_message: String,
) -> Result<PhaseResult> {
    let spec_text = ctx.spec_text();
    let memory = ctx.memory_text().await?;
    let agent = ctx.agent_spec(&AgentRole::QaFixer)?;
    let runner = ctx
        .runner(&agent)?
        .var("qa_report", qa_report.clone())
        .var("spec", spec_text.clone())
        .var("memory", memory.clone())
        .var("prior_context", prior.clone());
    let mut data = KickoffData::new(&ctx.task);
    data.qa_report = &qa_report;
    data.spec = &spec_text;
    data.memory = &memory;
    data.prior_context = &prior;
    let msg = kickoff_for(&agent.role, &data, &agent.system_prompt);
    let outcome = runner.run(&agent, msg).await?;
    ctx.record(&outcome);
    RunContext::check_cancelled(&outcome)?;
    let out = vibe_agents::parse_structured::<FixerOutput>(&outcome.final_text).unwrap_or_default();
    let ok = outcome.is_success() && !out.status.trim().eq_ignore_ascii_case("failed");
    let summary = format!(
        "{label}: fixer {} ({} issue(s) reported fixed)",
        if out.status.is_empty() {
            "finished"
        } else {
            out.status.trim()
        },
        out.fixed.len()
    );
    ctx.note(&format!("QA fix {summary}.\n\n{}", out.summary.trim()))
        .await?;
    ctx.commit(&commit_message, None).await;
    Ok(PhaseResult::ok(Phase::Fix, summary)
        .with_success(ok)
        .then(Transition::Goto { phase: Phase::Qa }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibe_core::{QaIssue, QaVerdict, Severity, TaskId};

    fn report(round: u32, titles: &[&str]) -> QaReport {
        QaReport {
            task_id: TaskId::new(),
            round,
            verdict: QaVerdict::ChangesRequested,
            summary: String::new(),
            issues: titles
                .iter()
                .map(|t| QaIssue {
                    severity: Severity::High,
                    title: (*t).to_string(),
                    detail: String::new(),
                    requirement: None,
                    file: None,
                    line: None,
                    suggested_fix: None,
                })
                .collect(),
        }
    }

    #[test]
    fn detects_repeated_titles() {
        let r = vec![
            report(1, &["Tests fail", "Typo"]),
            report(2, &["tests  FAIL"]),
            report(3, &["Tests fail", "Other"]),
        ];
        assert_eq!(repeated_issue(&r, 3).as_deref(), Some("tests fail"));
        assert_eq!(repeated_issue(&r[..2], 3), None);
        let r2 = vec![report(1, &["a"]), report(2, &["b"]), report(3, &["a"])];
        assert_eq!(repeated_issue(&r2, 3), None);
    }
}
