//! `plan`: the planner turns the spec into a validated [`Plan`].

use std::collections::HashMap;

use vibe_core::{
    AgentRole, Error, ErrorKind, Phase, Plan, PlanPhase, Result, Subtask, SubtaskId, TaskId,
};

use super::{lenient_string, lenient_strings};
use crate::context::{PhaseResult, RunContext};
use crate::kickoff::{KickoffData, kickoff_for};
use crate::store::plan_to_markdown;

/// One subtask as written by the planner.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlannerSubtask {
    /// Unique title.
    #[serde(default, deserialize_with = "lenient_string")]
    pub title: String,
    /// Instructions.
    #[serde(default, deserialize_with = "lenient_string")]
    pub description: String,
    /// Files to touch.
    #[serde(default, deserialize_with = "lenient_strings")]
    pub files: Vec<String>,
    /// Titles (or 1-based positions) of earlier subtasks.
    #[serde(default, deserialize_with = "lenient_strings")]
    pub depends_on: Vec<String>,
    /// Verification steps.
    #[serde(default, deserialize_with = "lenient_strings")]
    pub verification: Vec<String>,
}

/// One phase as written by the planner.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlannerPhase {
    /// Name.
    #[serde(default, deserialize_with = "lenient_string")]
    pub name: String,
    /// Whether subtasks are independent.
    #[serde(default)]
    pub parallel: bool,
    /// Subtasks.
    #[serde(default)]
    pub subtasks: Vec<PlannerSubtask>,
}

/// Structured output of the planner.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlannerOutput {
    /// Overall approach.
    #[serde(default, deserialize_with = "lenient_string")]
    pub approach: String,
    /// Phases.
    #[serde(default)]
    pub phases: Vec<PlannerPhase>,
    /// Tolerated alternative: a flat list of subtasks without phases
    /// (becomes one sequential phase).
    #[serde(default)]
    pub subtasks: Vec<PlannerSubtask>,
}

fn key(title: &str) -> String {
    title.trim().to_lowercase()
}

/// Convert the planner output into a [`Plan`]: generate subtask ids, map
/// `depends_on` titles (case-insensitive) or 1-based positions to ids, and
/// validate the result with [`Plan::validate`]. The error message is meant to
/// be fed back to the planner.
pub fn plan_from_output(task_id: TaskId, out: PlannerOutput) -> std::result::Result<Plan, String> {
    let mut phases = out.phases;
    if !out.subtasks.is_empty() {
        phases.push(PlannerPhase {
            name: "Implementation".into(),
            parallel: false,
            subtasks: out.subtasks,
        });
    }
    let mut ids: HashMap<String, SubtaskId> = HashMap::new();
    let mut positions: Vec<SubtaskId> = Vec::new();
    let mut errors = Vec::new();
    for s in phases.iter().flat_map(|p| p.subtasks.iter()) {
        let id = SubtaskId::new();
        positions.push(id);
        if s.title.trim().is_empty() {
            errors.push(format!("subtask #{} has no title", positions.len()));
        } else if ids.insert(key(&s.title), id).is_some() {
            errors.push(format!("duplicate subtask title `{}`", s.title.trim()));
        }
    }
    if positions.is_empty() {
        return Err("the plan contains no subtask".into());
    }
    let mut plan_phases = Vec::new();
    let mut n = 0;
    for (pi, p) in phases.into_iter().enumerate() {
        let mut subtasks = Vec::new();
        for s in p.subtasks {
            let id = positions[n];
            n += 1;
            let mut sub = Subtask::new(s.title.trim(), s.description.trim());
            sub.id = id;
            sub.files = s.files;
            sub.verification = s.verification;
            for dep in &s.depends_on {
                let resolved = ids.get(&key(dep)).copied().or_else(|| {
                    dep.trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| i.checked_sub(1))
                        .and_then(|i| positions.get(i).copied())
                });
                match resolved {
                    Some(d) if d == id => {
                        errors.push(format!("subtask `{}` depends on itself", sub.title));
                    }
                    Some(d) => {
                        if !sub.depends_on.contains(&d) {
                            sub.depends_on.push(d);
                        }
                    }
                    None => errors.push(format!(
                        "subtask `{}` depends on unknown subtask `{dep}`",
                        sub.title
                    )),
                }
            }
            subtasks.push(sub);
        }
        plan_phases.push(PlanPhase {
            name: if p.name.trim().is_empty() {
                format!("Phase {}", pi + 1)
            } else {
                p.name.trim().to_string()
            },
            parallel: p.parallel,
            subtasks,
        });
    }
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    let plan = Plan {
        task_id,
        approach: out.approach.trim().to_string(),
        phases: plan_phases,
    };
    plan.validate().map_err(|e| e.message)?;
    Ok(plan)
}

/// Run the planner (retrying with the validation error as feedback up to
/// `max_phase_retries` times) and save the plan.
pub async fn run_plan(ctx: &mut RunContext) -> Result<PhaseResult> {
    let memory = ctx.memory_text().await?;
    let spec_text = ctx.spec_text();
    let spec = ctx.agent_spec(&AgentRole::Planner)?;
    let attempts = ctx.config.pipeline.max_phase_retries + 1;
    let human = ctx
        .take_rejection(vibe_core::ApprovalGate::Plan)
        .unwrap_or_default();
    let mut feedback = human.clone();
    for attempt in 1..=attempts {
        let runner = ctx
            .runner(&spec)?
            .var("spec", spec_text.clone())
            .var("memory", memory.clone())
            .var("prior_context", feedback.clone());
        let mut data = KickoffData::new(&ctx.task);
        data.spec = &spec_text;
        data.memory = &memory;
        data.prior_context = &feedback;
        let msg = kickoff_for(&spec.role, &data, &spec.system_prompt);
        let error = match vibe_agents::run_structured::<PlannerOutput>(&runner, &spec, msg).await {
            Ok((out, outcome)) => {
                ctx.record(&outcome);
                match plan_from_output(ctx.task.id, out) {
                    Ok(plan) => {
                        ctx.store.save_plan(&plan).await?;
                        ctx.artefact_written(vibe_core::Artefact::Plan).await;
                        let summary = format!(
                            "{} subtask(s) in {} phase(s)",
                            plan.len(),
                            plan.phases.len()
                        );
                        ctx.note(&format!(
                            "Plan ready: {summary}.\n\n{}",
                            plan_to_markdown(&plan)
                        ))
                        .await?;
                        ctx.plan = Some(plan);
                        return Ok(PhaseResult::ok(Phase::Plan, summary));
                    }
                    Err(e) => e,
                }
            }
            Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
            Err(e) => e.message,
        };
        if attempt < attempts {
            ctx.events
                .publish(vibe_core::Event::Retrying {
                    run: ctx.run_id,
                    what: format!("plan: {error}"),
                    attempt: attempt + 1,
                    delay_ms: 0,
                })
                .await;
        }
        feedback = format!(
            "{human}Your previous plan was rejected: {error}. Produce a corrected, complete plan."
        );
    }
    Err(Error::other(format!(
        "the planner did not produce a valid plan after {attempts} attempt(s): {feedback}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn out(json: &str) -> PlannerOutput {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn maps_titles_to_ids() {
        let plan = plan_from_output(
            TaskId::new(),
            out(r#"{"approach":"a","phases":[
                {"name":"p1","parallel":true,"subtasks":[{"title":"A"},{"title":"B"}]},
                {"name":"p2","subtasks":[{"title":"C","depends_on":["a"," B ", "1"]}]}
            ]}"#),
        )
        .unwrap();
        assert_eq!(plan.len(), 3);
        let ids: Vec<SubtaskId> = plan.subtasks().map(|s| s.id).collect();
        let c = plan.subtasks().nth(2).unwrap();
        assert_eq!(c.depends_on, vec![ids[0], ids[1]]);
        assert!(plan.phases[0].parallel);
    }

    #[test]
    fn rejects_bad_plans() {
        let t = TaskId::new();
        assert!(plan_from_output(t, out(r#"{"phases":[]}"#)).is_err());
        let e = plan_from_output(t, out(r#"{"subtasks":[{"title":"A","depends_on":"Z"}]}"#))
            .unwrap_err();
        assert!(e.contains("unknown subtask `Z`"));
        let e =
            plan_from_output(t, out(r#"{"subtasks":[{"title":"A"},{"title":"a"}]}"#)).unwrap_err();
        assert!(e.contains("duplicate"));
        let e = plan_from_output(
            t,
            out(r#"{"subtasks":[{"title":"A","depends_on":["B"]},{"title":"B","depends_on":["A"]}]}"#),
        )
        .unwrap_err();
        assert!(e.contains("cycle"));
        let e = plan_from_output(t, out(r#"{"subtasks":[{"title":"A","depends_on":["A"]}]}"#))
            .unwrap_err();
        assert!(e.contains("itself"));
    }

    #[test]
    fn flat_subtasks_become_one_phase() {
        let plan = plan_from_output(TaskId::new(), out(r#"{"subtasks":[{"title":"A"}]}"#)).unwrap();
        assert_eq!(plan.phases.len(), 1);
        assert!(!plan.phases[0].parallel);
    }
}
