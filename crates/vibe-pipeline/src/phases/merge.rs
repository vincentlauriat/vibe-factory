//! `merge`: integrate the workspace (when `auto_merge` is on).

use vibe_core::{
    Error, ErrorKind, Event, MergeOutcome, MergeValidator, Phase, Result, TaskStatus, Workspace,
};

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
/// With required validation commands, integration uses `merge_validated` and any
/// conflict, validation or provider error pauses in Review instead of marking Ready.
pub async fn run_merge(ctx: &mut RunContext) -> Result<PhaseResult> {
    let stop = |status: TaskStatus, reason: String| Transition::Stop {
        status,
        reason,
        pause: false,
    };
    if let Some(blocked) = super::validation::run_validations(ctx).await? {
        return Ok(blocked);
    }
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
    let validated = !ctx.config.pipeline.validation_commands.is_empty();
    let outcome = if validated {
        let provider = ctx.workspace_provider.clone();
        let workspace = ctx.workspace.clone();
        let mut validator = IntegrationValidator { ctx, blocked: None };
        let outcome = provider.merge_validated(&workspace, &mut validator).await;
        if let Some(blocked) = validator.blocked {
            return Ok(blocked);
        }
        outcome
    } else {
        ctx.workspace_provider.merge(&ctx.workspace).await
    };
    match outcome {
        Ok(MergeOutcome::Merged { commit }) => {
            if let Some(c) = &commit {
                ctx.events
                    .publish(Event::Merged {
                        run: ctx.run_id,
                        commit: c.clone(),
                        branch: ctx.workspace.branch.clone().unwrap_or_default(),
                        base: ctx.workspace.base_branch.clone().unwrap_or_default(),
                    })
                    .await;
            }
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
            if validated {
                return Ok(integration_paused(format!(
                    "integration conflicts need human review: {}",
                    files.join(", ")
                )));
            }
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
            if e.kind == ErrorKind::Cancelled {
                return Err(e);
            }
            if validated {
                ctx.note(&format!("Validated integration stopped: {e}"))
                    .await?;
                return Ok(integration_paused(format!(
                    "validated integration stopped: {}",
                    e.message
                )));
            }
            ctx.note(&format!("Automatic merge failed: {e}")).await?;
            let reason = format!("automatic merge failed: {}", e.message);
            Ok(PhaseResult::ok(Phase::Merge, reason.clone())
                .with_success(false)
                .then(stop(TaskStatus::Ready, reason)))
        }
    }
}

fn integration_paused(reason: String) -> PhaseResult {
    PhaseResult::ok(Phase::Merge, &reason)
        .with_success(false)
        .then(Transition::Stop {
            status: TaskStatus::Review,
            reason,
            pause: true,
        })
}

struct IntegrationValidator<'a> {
    ctx: &'a mut RunContext,
    blocked: Option<PhaseResult>,
}

#[async_trait::async_trait]
impl MergeValidator for IntegrationValidator<'_> {
    async fn validate(&mut self, candidate: &Workspace) -> Result<()> {
        self.blocked =
            super::validation::run_validations_in(self.ctx, &candidate.root, true).await?;
        if self.blocked.is_some() {
            return Err(Error::workspace("integration validation failed"));
        }
        Ok(())
    }
}
