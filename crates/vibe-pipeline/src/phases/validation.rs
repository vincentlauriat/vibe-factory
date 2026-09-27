//! Deterministic, fail-closed checks at the merge boundary.

use chrono::Utc;
use serde_json::json;
use vibe_core::{
    Error, ErrorKind, Phase, Result, TaskStatus, ToolContext, ToolOutput, ToolSelection,
};

use crate::context::{PhaseResult, RunContext, Transition, permissions_for};
use crate::state::ValidationResult;

/// Run every configured command afresh, including on resume. Stop at the first failure.
pub(super) async fn run_validations(ctx: &mut RunContext) -> Result<Option<PhaseResult>> {
    let root = ctx.workspace.root.clone();
    run_validations_in(ctx, &root, false).await
}

pub(super) async fn run_validations_in(
    ctx: &mut RunContext,
    root: &std::path::Path,
    integration: bool,
) -> Result<Option<PhaseResult>> {
    ctx.state.pending_validation_fix = None;
    for command in ctx.config.pipeline.validation_commands.clone() {
        if ctx.is_cancelled() {
            return Err(Error::new(ErrorKind::Cancelled, "validation cancelled"));
        }
        ctx.note(&format!(
            "Required validation (integration={integration}, {}): `{command}`",
            root.display()
        ))
        .await?;
        let mut tool_ctx = ToolContext::new(root)
            .with_permissions(permissions_for(&ToolSelection::All, &ctx.config.security));
        tool_ctx.task_id = Some(ctx.task.id);
        tool_ctx.agent = "pipeline_validation".into();
        let input = json!({"command": command});
        let output = if command.trim().is_empty() {
            ToolOutput::error("Required validation command is empty")
        } else if let vibe_core::HookDecision::Abort(reason) =
            ctx.registry.before_tool(&tool_ctx, "bash", &input).await
        {
            ToolOutput::error(format!("Required validation vetoed: {reason}"))
        } else if let Some(tool) = ctx.tools.get("bash") {
            match tool.call(&tool_ctx, input).await {
                Ok(output) => output,
                Err(error) => ToolOutput::error(error.to_string()),
            }
        } else {
            ToolOutput::error("Required validation needs a registered bash tool")
        };
        ctx.registry.after_tool(&tool_ctx, "bash", &output).await;
        let passed = !output.is_error
            && output.metadata["exit_code"].as_i64() == Some(0)
            && output.metadata["timed_out"] != true;
        // Policy/configuration failures need a human, not attempts to bypass the policy.
        let repairable = output.metadata["denied"] != true
            && (output.metadata["exit_code"]
                .as_i64()
                .is_some_and(|code| code != 0)
                || output.metadata["timed_out"] == true);
        ctx.state.validations.push(ValidationResult {
            integration,
            workspace_root: root.to_path_buf(),
            command: command.clone(),
            finished_at: Utc::now(),
            passed,
            output: output.content.clone(),
            metadata: output.metadata,
        });
        ctx.state.touch();
        ctx.store.save_run_state(&ctx.state).await?;
        ctx.note(&format!(
            "Required validation {}: `{command}`\n\n{}",
            if passed { "passed" } else { "failed" },
            output.content,
        ))
        .await?;
        if !passed {
            if !integration
                && repairable
                && ctx.state.validation_fix_attempts
                    < ctx.config.pipeline.max_validation_fix_attempts
            {
                ctx.state.pending_validation_fix = Some(ctx.state.validations.len() - 1);
                ctx.store.save_run_state(&ctx.state).await?;
                let reason = format!(
                    "required validation failed: {command}; requesting automatic correction"
                );
                ctx.note(&reason).await?;
                return Ok(Some(
                    PhaseResult::ok(Phase::Merge, reason)
                        .with_success(false)
                        .then(Transition::Goto { phase: Phase::Fix }),
                ));
            }
            let budget = if integration {
                "; integration candidate rejected; target branch was not updated".into()
            } else if repairable {
                format!(
                    "; automatic fix budget exhausted ({}/{})",
                    ctx.state.validation_fix_attempts,
                    ctx.config.pipeline.max_validation_fix_attempts
                )
            } else {
                "; execution or policy error requires human review".into()
            };
            let reason = format!(
                "required validation failed: {command}{budget}; inspect run.json and progress.md, fix the workspace, then resume"
            );
            return Ok(Some(
                PhaseResult::ok(Phase::Merge, &reason)
                    .with_success(false)
                    .then(Transition::Stop {
                        status: TaskStatus::Review,
                        reason,
                        pause: true,
                    }),
            ));
        }
    }
    if ctx.is_cancelled() {
        return Err(Error::new(ErrorKind::Cancelled, "validation cancelled"));
    }
    Ok(None)
}
