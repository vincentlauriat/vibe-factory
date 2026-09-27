//! `assess`: pick the complexity and the pipeline profile.

use vibe_core::{AgentRole, Complexity, ErrorKind, Phase, Result};

use crate::complexity::{AssessmentOutput, heuristic_complexity, profile_for};
use crate::context::{PhaseResult, RunContext};
use crate::kickoff::{KickoffData, kickoff_for};

/// Classify the task and set `ctx.profile`, `ctx.state.profile` and
/// `ctx.task.complexity`.
///
/// Order: override, complexity already on the task, heuristic, then the
/// `complexity_assessor` agent; any agent failure (other than cancellation)
/// falls back to [`Complexity::Standard`].
pub async fn run_assess(ctx: &mut RunContext) -> Result<PhaseResult> {
    let (profile, complexity, source) = if let Some(c) = ctx.complexity_override {
        (profile_for(c), c, "override".to_string())
    } else if let Some(c) = ctx.task.complexity {
        (profile_for(c), c, "stored on the task".to_string())
    } else if let Some(c) = heuristic_complexity(&ctx.task) {
        (profile_for(c), c, "heuristic".to_string())
    } else {
        let spec = ctx.agent_spec(&AgentRole::ComplexityAssessor)?;
        let runner = ctx.runner(&spec)?.var("prior_context", "");
        let message = kickoff_for(
            &spec.role,
            &KickoffData::new(&ctx.task),
            &spec.system_prompt,
        );
        match vibe_agents::run_structured::<AssessmentOutput>(&runner, &spec, message).await {
            Ok((out, outcome)) => {
                ctx.record(&outcome);
                match (
                    out.profile(),
                    crate::complexity::parse_complexity(&out.complexity),
                ) {
                    (Some(p), Some(c)) => (
                        p,
                        c,
                        format!(
                            "assessor (confidence {:.2}, risk {}): {}",
                            out.confidence,
                            if out.risk_level.is_empty() {
                                "?"
                            } else {
                                &out.risk_level
                            },
                            out.reasoning.trim()
                        ),
                    ),
                    _ => (
                        profile_for(Complexity::Standard),
                        Complexity::Standard,
                        format!("fallback: unknown complexity `{}`", out.complexity),
                    ),
                }
            }
            Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
            Err(e) => {
                ctx.events
                    .log(
                        Some(ctx.run_id),
                        "warn",
                        format!("complexity assessment failed, using standard: {e}"),
                    )
                    .await;
                (
                    profile_for(Complexity::Standard),
                    Complexity::Standard,
                    format!("fallback after assessor failure: {}", e.message),
                )
            }
        }
    };
    ctx.task.complexity = Some(complexity);
    ctx.task.touch();
    ctx.state.profile = Some(profile.clone());
    ctx.profile = profile;
    let phases: Vec<&str> = ctx.profile.phases.iter().map(|p| p.as_str()).collect();
    let summary = format!(
        "complexity {} ({source}); phases: {}",
        serde_json::to_value(complexity)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
        phases.join(" → ")
    );
    ctx.note(&format!("Assessment: {summary}")).await?;
    Ok(PhaseResult::ok(Phase::Assess, summary))
}
