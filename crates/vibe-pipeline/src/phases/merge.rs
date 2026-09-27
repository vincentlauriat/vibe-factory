//! `merge`: integrate the workspace (when `auto_merge` is on).

use vibe_core::{MergeOutcome, Phase, Result, TaskStatus};

use crate::context::{PhaseResult, RunContext, Transition};

/// Merge the workspace when `pipeline.auto_merge` is set.
///
/// | Situation | Final status |
/// |-----------|--------------|
/// | `auto_merge = false` | Ready (a human reviews and merges) |
/// | merged / nothing to merge | Done |
/// | conflicts left ([`MergeOutcome::NeedsHumanReview`]) | Ready, conflicting files in the progress notes |
/// | merge error | Ready, error in the progress notes |
///
/// Conflict handling (`pipeline.merge_strategy`: manual or AI-assisted) is
/// implemented by the workspace provider, which the host builds with that
/// strategy; the strategy in effect is recorded in the progress notes.
pub async fn run_merge(ctx: &mut RunContext) -> Result<PhaseResult> {
    let stop = |status: TaskStatus, reason: String| Transition::Stop {
        status,
        reason,
        pause: false,
    };
    if !ctx.config.pipeline.auto_merge {
        let reason = "ready for human review and merge (auto_merge is off)".to_string();
        ctx.note(&format!("Merge: {reason}.")).await?;
        return Ok(
            PhaseResult::ok(Phase::Merge, reason.clone()).then(stop(TaskStatus::Ready, reason))
        );
    }
    // The workspace provider is built with the strategy by the host; the
    // pipeline only records which one is in effect.
    let strategy = format!("{:?}", ctx.config.pipeline.merge_strategy).to_lowercase();
    ctx.note(&format!(
        "Merge: automatic merge with the `{strategy}` conflict strategy."
    ))
    .await?;
    match ctx.workspace_provider.merge(&ctx.workspace).await {
        Ok(MergeOutcome::Merged { commit }) => {
            let reason = match commit {
                Some(c) => format!("merged ({c})"),
                None => "merged".to_string(),
            };
            ctx.note(&format!("Merge: {reason}.")).await?;
            Ok(PhaseResult::ok(Phase::Merge, reason.clone()).then(stop(TaskStatus::Done, reason)))
        }
        Ok(MergeOutcome::NoChanges) => {
            let reason = "nothing to merge".to_string();
            ctx.note("Merge: nothing to merge.").await?;
            Ok(PhaseResult::ok(Phase::Merge, reason.clone()).then(stop(TaskStatus::Done, reason)))
        }
        Ok(MergeOutcome::NeedsHumanReview { files }) => {
            let list: String = files.iter().map(|f| format!("- `{f}`\n")).collect();
            ctx.note(&format!(
                "Merge needs a human: conflicts remain in {} file(s):\n\n{list}",
                files.len()
            ))
            .await?;
            let reason = format!("merge conflicts in {} file(s)", files.len());
            Ok(PhaseResult::ok(Phase::Merge, reason.clone())
                .with_success(false)
                .then(stop(TaskStatus::Ready, reason)))
        }
        Err(e) => {
            ctx.note(&format!("Automatic merge failed: {e}")).await?;
            let reason = format!("automatic merge failed: {}", e.message);
            Ok(PhaseResult::ok(Phase::Merge, reason.clone())
                .with_success(false)
                .then(stop(TaskStatus::Ready, reason)))
        }
    }
}
