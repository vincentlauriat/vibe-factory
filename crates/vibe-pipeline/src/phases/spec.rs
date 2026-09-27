//! `spec`: gatherer → (researcher) → (writer) → (critic).

use vibe_core::{
    AgentRole, ErrorKind, Phase, Requirement, RequirementKind, Result, Spec, SpecContext, TaskId,
};

use super::{lenient_string, lenient_strings, pretty};
use crate::context::{PhaseResult, RunContext};
use crate::kickoff::{KickoffData, PRIOR_CONTEXT_MAX_CHARS, kickoff_for, truncate_head};
use crate::store::MemoryFile;

/// A requirement as written by an agent (lenient).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DraftRequirement {
    /// `R1`, … (generated when missing).
    #[serde(default, deserialize_with = "lenient_string")]
    pub id: String,
    /// What must be true.
    #[serde(default, deserialize_with = "lenient_string")]
    pub description: String,
    /// `functional`, `non_functional` or `constraint`.
    #[serde(default, deserialize_with = "lenient_string")]
    pub kind: String,
    /// 1 = must have.
    #[serde(default)]
    pub priority: Option<u8>,
    /// Acceptance criteria.
    #[serde(default, deserialize_with = "lenient_strings")]
    pub acceptance: Vec<String>,
}

/// The JSON document produced by the gatherer and the writer (and embedded
/// in the critic output).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpecDraft {
    /// Goal summary.
    #[serde(default, deserialize_with = "lenient_string")]
    pub summary: String,
    /// Requirements.
    #[serde(default)]
    pub requirements: Vec<DraftRequirement>,
    /// Gathered context.
    #[serde(default)]
    pub context: SpecContext,
    /// Markdown body.
    #[serde(default, deserialize_with = "lenient_string")]
    pub body: String,
}

impl SpecDraft {
    /// Whether the draft carries any content.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.summary.trim().is_empty() && self.requirements.is_empty()
    }

    /// Convert into a [`Spec`], generating missing ids and normalising kinds.
    #[must_use]
    pub fn into_spec(self, task_id: TaskId) -> Spec {
        let requirements = self
            .requirements
            .into_iter()
            .enumerate()
            .filter(|(_, r)| !r.description.trim().is_empty())
            .map(|(i, r)| Requirement {
                id: if r.id.trim().is_empty() {
                    format!("R{}", i + 1)
                } else {
                    r.id.trim().to_string()
                },
                description: r.description.trim().to_string(),
                kind: match r
                    .kind
                    .trim()
                    .to_ascii_lowercase()
                    .replace(['-', ' '], "_")
                    .as_str()
                {
                    "non_functional" | "nonfunctional" => RequirementKind::NonFunctional,
                    "constraint" => RequirementKind::Constraint,
                    _ => RequirementKind::Functional,
                },
                priority: r.priority.unwrap_or(1).max(1),
                acceptance: r.acceptance,
            })
            .collect();
        Spec {
            task_id,
            summary: self.summary.trim().to_string(),
            requirements,
            context: self.context,
            body: self.body,
        }
    }
}

/// Structured output of the spec critic.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CriticOutput {
    /// `ok` or `revised`.
    #[serde(default, deserialize_with = "lenient_string")]
    pub verdict: String,
    /// Issues found (kept verbatim).
    #[serde(default)]
    pub issues: Vec<serde_json::Value>,
    /// The (possibly corrected) specification.
    #[serde(default)]
    pub spec: Option<SpecDraft>,
}

/// Relative path (inside the workspace) where the writer puts its markdown.
fn spec_path(ctx: &RunContext) -> String {
    format!(".vibe/specs/{}-spec.md", ctx.task.id.short())
}

/// Run the spec steps enabled by the profile and save the result.
///
/// The gatherer is mandatory (its failure fails the phase); researcher,
/// writer and critic failures are logged and the best spec so far is kept.
pub async fn run_spec(ctx: &mut RunContext) -> Result<PhaseResult> {
    let memory = ctx.memory_text().await?;
    let mut steps = Vec::new();
    let human = ctx
        .take_rejection(vibe_core::ApprovalGate::Spec)
        .unwrap_or_default();

    // 1. Gatherer.
    let spec_g = ctx.agent_spec(&AgentRole::SpecGatherer)?;
    let runner = ctx
        .runner(&spec_g)?
        .var("memory", memory.clone())
        .var("prior_context", human.clone());
    let mut data = KickoffData::new(&ctx.task);
    data.memory = &memory;
    data.prior_context = &human;
    let msg = kickoff_for(&spec_g.role, &data, &spec_g.system_prompt);
    let (gathered, outcome) =
        vibe_agents::run_structured::<SpecDraft>(&runner, &spec_g, msg).await?;
    ctx.record(&outcome);
    steps.push("gatherer");
    let mut prior = format!(
        "{human}### Gathered requirements (JSON)\n\n{}\n",
        pretty(&gathered)
    );
    let mut draft = gathered;

    // 2. Researcher.
    if ctx.profile.research {
        let spec_r = ctx.agent_spec(&AgentRole::SpecResearcher)?;
        let p = truncate_head(&prior, PRIOR_CONTEXT_MAX_CHARS);
        let runner = ctx
            .runner(&spec_r)?
            .var("memory", memory.clone())
            .var("prior_context", p.clone());
        let mut data = KickoffData::new(&ctx.task);
        data.memory = &memory;
        data.prior_context = &p;
        let msg = kickoff_for(&spec_r.role, &data, &spec_r.system_prompt);
        let outcome = runner.run(&spec_r, msg).await?;
        ctx.record(&outcome);
        RunContext::check_cancelled(&outcome)?;
        if outcome.is_success() && !outcome.final_text.trim().is_empty() {
            prior.push_str(&format!(
                "\n### Research report\n\n{}\n",
                outcome.final_text.trim()
            ));
            steps.push("researcher");
        } else {
            ctx.events
                .log(Some(ctx.run_id), "warn", "spec research produced no report")
                .await;
        }
    }

    // 3. Writer.
    if ctx.profile.full_spec {
        let spec_w = ctx.agent_spec(&AgentRole::SpecWriter)?;
        let p = truncate_head(&prior, PRIOR_CONTEXT_MAX_CHARS);
        let runner = ctx
            .runner(&spec_w)?
            .var("memory", memory.clone())
            .var("prior_context", p.clone())
            .var("spec_path", spec_path(ctx));
        let mut data = KickoffData::new(&ctx.task);
        data.memory = &memory;
        data.prior_context = &p;
        let msg = kickoff_for(&spec_w.role, &data, &spec_w.system_prompt);
        match vibe_agents::run_structured::<SpecDraft>(&runner, &spec_w, msg).await {
            Ok((written, outcome)) => {
                ctx.record(&outcome);
                if !written.is_empty() {
                    draft = written;
                    steps.push("writer");
                }
            }
            Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
            Err(e) => {
                ctx.events
                    .log(
                        Some(ctx.run_id),
                        "warn",
                        format!("spec writer failed, keeping gathered requirements: {e}"),
                    )
                    .await;
            }
        }
    }

    let mut spec = draft.into_spec(ctx.task.id);

    // 4. Critic.
    let mut critique = String::new();
    if ctx.profile.critique {
        let spec_c = ctx.agent_spec(&AgentRole::SpecCritic)?;
        let p = truncate_head(&prior, PRIOR_CONTEXT_MAX_CHARS);
        let current = spec.to_markdown();
        let runner = ctx
            .runner(&spec_c)?
            .var("spec", current.clone())
            .var("prior_context", p.clone());
        let mut data = KickoffData::new(&ctx.task);
        data.spec = &current;
        data.prior_context = &p;
        let msg = kickoff_for(&spec_c.role, &data, &spec_c.system_prompt);
        match vibe_agents::run_structured::<CriticOutput>(&runner, &spec_c, msg).await {
            Ok((out, outcome)) => {
                ctx.record(&outcome);
                steps.push("critic");
                let revised = out.verdict.trim().eq_ignore_ascii_case("revised");
                critique = format!(
                    ", critic: {} ({} issues)",
                    out.verdict.trim(),
                    out.issues.len()
                );
                if revised
                    && let Some(new) = out.spec
                    && !new.is_empty()
                {
                    spec = new.into_spec(ctx.task.id);
                }
            }
            Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
            Err(e) => {
                ctx.events
                    .log(Some(ctx.run_id), "warn", format!("spec critic failed: {e}"))
                    .await;
            }
        }
    }

    if spec.summary.is_empty() && spec.requirements.is_empty() {
        return Err(vibe_core::Error::other("the specification is empty"));
    }
    ctx.store.save_spec(&spec).await?;
    ctx.artefact_written(vibe_core::Artefact::Spec).await;
    for finding in &spec.context.findings {
        ctx.remember(MemoryFile::Patterns, finding).await?;
    }
    let summary = format!(
        "{} requirement(s) via {}{critique}",
        spec.requirements.len(),
        steps.join(" → ")
    );
    ctx.note(&format!(
        "Specification written: {summary}.\n\n{}",
        spec.summary
    ))
    .await?;
    ctx.spec = Some(spec);
    Ok(PhaseResult::ok(Phase::Spec, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_normalises_requirements() {
        let d: SpecDraft = serde_json::from_str(
            r#"{"summary":" s ","requirements":[
                {"description":"a","kind":"non-functional","acceptance":"check a"},
                {"id":"X9","description":"b","kind":"constraint","priority":2,"acceptance":["x",3]},
                {"description":"  "}
            ],"body":null}"#,
        )
        .unwrap();
        let spec = d.into_spec(TaskId::new());
        assert_eq!(spec.summary, "s");
        assert_eq!(spec.requirements.len(), 2);
        assert_eq!(spec.requirements[0].id, "R1");
        assert_eq!(spec.requirements[0].kind, RequirementKind::NonFunctional);
        assert_eq!(spec.requirements[0].acceptance, vec!["check a"]);
        assert_eq!(spec.requirements[1].id, "X9");
        assert_eq!(spec.requirements[1].acceptance, vec!["x", "3"]);
    }

    #[test]
    fn critic_output_parses() {
        let c: CriticOutput = serde_json::from_str(
            r#"{"verdict":"revised","issues":[{"title":"t"}],"spec":{"summary":"new"}}"#,
        )
        .unwrap();
        assert_eq!(c.issues.len(), 1);
        assert!(!c.spec.unwrap().is_empty());
    }
}
