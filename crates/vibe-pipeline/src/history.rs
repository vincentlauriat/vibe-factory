//! What happened to finished tasks, rebuilt from their event logs, their
//! `run.json` and git: runs with their phases, totals, commits, merge,
//! validations and approvals, the files the task changed and, when models
//! are priced, what it cost.
//!
//! Nothing here fails a whole history because one task cannot be read from
//! git: the problem is recorded in [`TaskHistory::errors`] and the next
//! source of changed files is tried.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;

use chrono::{DateTime, Utc};
use tokio::sync::OnceCell;
use vibe_core::{
    AgentRole, ApprovalGate, Envelope, Event, Phase, QaVerdict, Result, RunId, SubtaskId, Task,
    TaskStatus, Usage, VibeConfig,
};
use vibe_workspace::Git;
use vibe_workspace::git::EXCLUDE_VIBE;

use crate::state::{ApprovalRecord, RunState};
use crate::store::PipelineStore;
use crate::trace::{self, files_written};

/// One phase of a run.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PhaseSummary {
    /// Phase.
    pub phase: Phase,
    /// When it started.
    pub started_at: DateTime<Utc>,
    /// When it finished, if it did.
    pub finished_at: Option<DateTime<Utc>>,
    /// Whether it succeeded, once finished.
    pub success: Option<bool>,
    /// Summary published when it finished.
    pub summary: String,
}

/// A commit made by the pipeline in the task workspace.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitRecord {
    /// Commit id.
    pub commit: String,
    /// First line of the message (empty for integrations logged before 0.5).
    pub message: String,
    /// Files changed (empty for integrations logged before 0.5).
    pub files: Vec<String>,
    /// Subtask the commit belongs to, if any.
    pub subtask: Option<SubtaskId>,
    /// When it was logged.
    pub at: DateTime<Utc>,
}

/// The merge of the task branch into its base.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MergeRecord {
    /// Resulting commit on the base branch.
    pub commit: String,
    /// Merged branch.
    pub branch: String,
    /// Branch merged into.
    pub base: String,
    /// When it was logged.
    pub at: DateTime<Utc>,
}

/// A required validation command that finished.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ValidationRecord {
    /// The configured command.
    pub command: String,
    /// True when it checked the combined integration candidate.
    pub integration: bool,
    /// Whether it passed.
    pub passed: bool,
    /// Exit code, when the command ran to completion.
    pub exit_code: Option<i64>,
    /// When it finished.
    pub at: DateTime<Utc>,
}

/// Where a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSummaryState {
    /// A process runs it now (only known for the last run of a task, from
    /// its run lock).
    Running,
    /// Its last `run_finished` is its last start's end.
    Finished,
    /// Started (or resumed) without a `run_finished` since, and no process
    /// runs it: the process died.
    Interrupted,
}

/// One run of a task (a run and its resumes share one id).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunSummary {
    /// Run id.
    pub run: RunId,
    /// When the run first started.
    pub started_at: DateTime<Utc>,
    /// When it last finished; `None` while it runs again after a resume, or
    /// when it was interrupted without finishing (see `state`).
    pub finished_at: Option<DateTime<Utc>>,
    /// Running, finished or interrupted.
    pub state: RunSummaryState,
    /// Task status it finished with.
    pub status: Option<TaskStatus>,
    /// Whether it finished successfully.
    pub success: Option<bool>,
    /// Number of times it was resumed.
    pub resumes: u32,
    /// Tokens used by the whole run, resumes included (see `totals_known`).
    pub usage: Usage,
    /// Active time of the whole run in milliseconds (see `totals_known`).
    pub active_ms: u64,
    /// Whether `usage` and `active_ms` are known: false for a run logged
    /// before 0.5 that is not the one `run.json` describes. Interfaces show
    /// unknown totals as such, not as zeros.
    pub totals_known: bool,
    /// Phases in the order they started.
    pub phases: Vec<PhaseSummary>,
    /// Commits in the task workspace.
    pub commits: Vec<CommitRecord>,
    /// The merge into the base branch, if the run merged.
    pub merged: Option<MergeRecord>,
    /// Required validation commands.
    pub validations: Vec<ValidationRecord>,
    /// Human decisions.
    pub approvals: Vec<ApprovalRecord>,
    /// Gate the run waits on, if any.
    pub pending_approval: Option<ApprovalGate>,
    /// Last error, if the run did not end well.
    pub last_error: Option<String>,
    /// Time of the last event of the run.
    pub last_event_at: DateTime<Utc>,
}

impl RunSummary {
    fn new(run: RunId, at: DateTime<Utc>) -> Self {
        Self {
            run,
            started_at: at,
            finished_at: None,
            state: RunSummaryState::Interrupted,
            status: None,
            success: None,
            resumes: 0,
            usage: Usage::default(),
            active_ms: 0,
            totals_known: false,
            phases: Vec::new(),
            commits: Vec::new(),
            merged: None,
            validations: Vec::new(),
            approvals: Vec::new(),
            pending_approval: None,
            last_error: None,
            last_event_at: at,
        }
    }

    /// Use the totals and error of `run.json` when it describes this run
    /// (it is only kept for the last run of a task): when the log has no
    /// totals, or when the run was resumed without finishing again, so that
    /// `run.json` has grown past the totals of its earlier `run_finished`.
    pub fn apply_run_state(&mut self, state: &RunState) {
        if state.run_id != self.run {
            return;
        }
        let grew = self.finished_at.is_none()
            && (state.usage.total() > self.usage.total() || state.active_ms > self.active_ms);
        if !self.totals_known || grew {
            self.usage = state.usage;
            self.active_ms = state.active_ms;
            self.totals_known = true;
        }
        if state.last_error.is_some() {
            self.last_error.clone_from(&state.last_error);
        }
    }
}

/// Every run found in a task's event log, in the order they first started.
///
/// Totals come from the last `run_finished` of each run, which covers its
/// resumes; a `run_finished` logged before 0.5 has none (its `started_at` is
/// the Unix epoch) and leaves `totals_known` false.
#[must_use]
pub fn summarize_runs(events: &[Envelope]) -> Vec<RunSummary> {
    let mut runs: Vec<RunSummary> = Vec::new();
    let mut index: HashMap<RunId, usize> = HashMap::new();
    let mut starts: HashMap<RunId, u32> = HashMap::new();
    for env in events {
        let Some(run) = env.event.run_id() else {
            continue;
        };
        let i = *index.entry(run).or_insert_with(|| {
            runs.push(RunSummary::new(run, env.at));
            runs.len() - 1
        });
        let r = &mut runs[i];
        r.last_event_at = r.last_event_at.max(env.at);
        match &env.event {
            Event::RunStarted { .. } => {
                let n = starts.entry(run).or_insert(0);
                *n += 1;
                r.resumes = *n - 1;
                r.finished_at = None;
                r.status = None;
                r.success = None;
            }
            Event::PhaseStarted { phase, .. } => r.phases.push(PhaseSummary {
                phase: *phase,
                started_at: env.at,
                finished_at: None,
                success: None,
                summary: String::new(),
            }),
            Event::PhaseFinished {
                phase,
                success,
                summary,
                ..
            } => {
                if let Some(p) = r
                    .phases
                    .iter_mut()
                    .rev()
                    .find(|p| p.phase == *phase && p.finished_at.is_none())
                {
                    p.finished_at = Some(env.at);
                    p.success = Some(*success);
                    p.summary.clone_from(summary);
                }
                if !success {
                    r.last_error = Some(format!("{} failed: {summary}", phase.as_str()));
                }
            }
            Event::Committed {
                subtask,
                commit,
                message,
                files,
                ..
            } => {
                if !r.commits.iter().any(|c| c.commit == *commit) {
                    r.commits.push(CommitRecord {
                        commit: commit.clone(),
                        message: message.clone(),
                        files: files.clone(),
                        subtask: *subtask,
                        at: env.at,
                    });
                }
            }
            // Logs recorded before 0.5 announce integrations only.
            Event::SubtaskIntegrated {
                subtask,
                commit: Some(commit),
                conflicts,
                ..
            } if conflicts.is_empty() => {
                if !r.commits.iter().any(|c| c.commit == *commit) {
                    r.commits.push(CommitRecord {
                        commit: commit.clone(),
                        message: String::new(),
                        files: Vec::new(),
                        subtask: Some(*subtask),
                        at: env.at,
                    });
                }
            }
            Event::Merged {
                commit,
                branch,
                base,
                ..
            } => {
                r.merged = Some(MergeRecord {
                    commit: commit.clone(),
                    branch: branch.clone(),
                    base: base.clone(),
                    at: env.at,
                });
            }
            Event::ValidationFinished {
                command,
                integration,
                passed,
                exit_code,
                ..
            } => r.validations.push(ValidationRecord {
                command: command.clone(),
                integration: *integration,
                passed: *passed,
                exit_code: *exit_code,
                at: env.at,
            }),
            Event::ApprovalRequested { gate, .. } => r.pending_approval = Some(*gate),
            Event::ApprovalResolved {
                gate,
                approved,
                comment,
                ..
            } => {
                if r.pending_approval == Some(*gate) {
                    r.pending_approval = None;
                }
                r.approvals.push(ApprovalRecord {
                    gate: *gate,
                    approved: *approved,
                    comment: comment.clone(),
                    at: env.at,
                });
            }
            Event::Log { level, message, .. } if level == "error" => {
                r.last_error = Some(message.clone());
            }
            Event::RunFinished {
                success,
                status,
                usage,
                active_ms,
                started_at,
                ..
            } => {
                r.finished_at = Some(env.at);
                r.status = Some(*status);
                r.success = Some(*success);
                if *started_at != DateTime::<Utc>::UNIX_EPOCH {
                    r.usage = *usage;
                    r.active_ms = *active_ms;
                    r.totals_known = true;
                }
                if *success {
                    r.last_error = None;
                }
            }
            _ => {}
        }
    }
    for r in &mut runs {
        if r.finished_at.is_some() {
            r.state = RunSummaryState::Finished;
        }
    }
    runs
}

/// Status of a changed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// Created.
    Added,
    /// Modified (or its type changed).
    Modified,
    /// Deleted.
    Deleted,
    /// Renamed from `old_path`.
    Renamed,
    /// Copied from `old_path`.
    Copied,
    /// Not known (files from commit events or from the trace).
    Unknown,
}

/// One file changed by a task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileChange {
    /// Path relative to the repository (or workspace) root.
    pub path: String,
    /// What happened to it.
    pub status: FileStatus,
    /// Former path of a renamed or copied file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
}

/// Where the list of changed files comes from, in the order they are tried:
/// the merge, then the branch, then the commit events, then the trace.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChangedFilesSource {
    /// `git diff <base>...<branch>`: the task branch still exists, is not
    /// recorded as merged, and differs from its base.
    Branch {
        /// Task branch.
        branch: String,
        /// Base it is compared with.
        base: String,
    },
    /// The merge recorded by `merged`: against the merge commit's first
    /// parent, or, for a fast-forward, against the parent of the task's
    /// oldest commit that the merge contains (approximate when there is
    /// none).
    MergeCommit {
        /// Commit on the base branch.
        commit: String,
        /// Whether the merge was a fast-forward.
        fast_forward: bool,
    },
    /// The files of the `committed` events (statuses unknown; files a later
    /// commit reverted are still listed).
    Commits,
    /// The paths written by `write_file` and `edit_file` calls (statuses
    /// unknown; files written by commands are missing).
    Trace,
    /// Nothing to go on.
    None,
}

/// The files a task changed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangedFiles {
    /// Where the list comes from.
    pub source: ChangedFilesSource,
    /// Whether the list may be incomplete or too long
    /// ([`ChangedFilesSource::Commits`], [`ChangedFilesSource::Trace`], and a
    /// fast-forward none of whose recorded commits the merge contains).
    pub approximate: bool,
    /// The files, sorted by path.
    pub files: Vec<FileChange>,
}

impl ChangedFiles {
    fn none() -> Self {
        Self {
            source: ChangedFilesSource::None,
            approximate: false,
            files: Vec::new(),
        }
    }

    fn unknown_statuses(source: ChangedFilesSource, paths: BTreeSet<String>) -> Self {
        Self {
            source,
            approximate: true,
            files: paths
                .into_iter()
                .map(|path| FileChange {
                    path,
                    status: FileStatus::Unknown,
                    old_path: None,
                })
                .collect(),
        }
    }
}

/// Parse `git diff --name-status -z`.
#[must_use]
pub fn parse_name_status(output: &str) -> Vec<FileChange> {
    let mut fields = output.split('\0').filter(|f| !f.is_empty());
    let mut out = Vec::new();
    while let Some(code) = fields.next() {
        let status = match code.chars().next() {
            Some('A') => FileStatus::Added,
            Some('M' | 'T') => FileStatus::Modified,
            Some('D') => FileStatus::Deleted,
            Some('R') => FileStatus::Renamed,
            Some('C') => FileStatus::Copied,
            _ => FileStatus::Unknown,
        };
        let change = if matches!(status, FileStatus::Renamed | FileStatus::Copied) {
            let (Some(old), Some(new)) = (fields.next(), fields.next()) else {
                break;
            };
            FileChange {
                path: new.to_string(),
                status,
                old_path: Some(old.to_string()),
            }
        } else {
            let Some(path) = fields.next() else {
                break;
            };
            FileChange {
                path: path.to_string(),
                status,
                old_path: None,
            }
        };
        out.push(change);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// What is read once per project from git: local branches, the
/// `branch.<b>.vibebase` settings and, when needed, the default branch.
struct Repo {
    git: Git,
    branches: BTreeSet<String>,
    vibebases: BTreeMap<String, String>,
    config_base: Option<String>,
    /// Problems reading the repository itself, reported on every task.
    errors: Vec<String>,
    default_branch: OnceCell<std::result::Result<String, String>>,
}

impl Repo {
    /// `None` when the project is not a git repository.
    async fn open(project_root: &Path, config: &VibeConfig) -> Option<Self> {
        let git = Git::new(project_root);
        if !git.is_repo().await {
            return None;
        }
        let mut errors = Vec::new();
        let branches = match git
            .output(&["for-each-ref", "--format=%(refname)", "refs/heads"])
            .await
        {
            Ok(out) if out.success => out
                .stdout
                .lines()
                .filter_map(|l| l.strip_prefix("refs/heads/"))
                .map(str::to_string)
                .collect(),
            Ok(out) => {
                errors.push(format!(
                    "cannot list branches, task branches are ignored: {}",
                    out.stderr.trim()
                ));
                BTreeSet::new()
            }
            Err(e) => {
                errors.push(format!(
                    "cannot list branches, task branches are ignored: {e}"
                ));
                BTreeSet::new()
            }
        };
        // Exit status 1 means no such setting.
        let vibebases = match git
            .output(&[
                "config",
                "--local",
                "-z",
                "--get-regexp",
                r"^branch\..*\.vibebase$",
            ])
            .await
        {
            Ok(out) if out.success => out
                .stdout
                .split('\0')
                .filter_map(|entry| {
                    let (key, value) = entry.split_once('\n')?;
                    let branch = key.strip_prefix("branch.")?.strip_suffix(".vibebase")?;
                    Some((branch.to_string(), value.to_string()))
                })
                .collect(),
            _ => BTreeMap::new(),
        };
        Some(Self {
            git,
            branches,
            vibebases,
            config_base: config.base_branch.clone(),
            errors,
            default_branch: OnceCell::new(),
        })
    }

    /// Base of `branch`, as `vibe pr` resolves it: `branch.<b>.vibebase`,
    /// then `base_branch` from the configuration, then the default branch.
    async fn base_of(&self, branch: &str) -> std::result::Result<String, String> {
        if let Some(base) = self.vibebases.get(branch).or(self.config_base.as_ref()) {
            return Ok(base.clone());
        }
        self.default_branch
            .get_or_init(|| async { self.git.default_branch().await.map_err(|e| e.to_string()) })
            .await
            .clone()
    }

    /// `git diff --name-status` of `revs` (one range, or two commits),
    /// without the `.vibe` directory.
    async fn diff_files(&self, revs: &[&str]) -> std::result::Result<Vec<FileChange>, String> {
        // Revisions come from the log and the configuration: never let one
        // read as an option.
        let mut args = vec![
            "diff",
            "--name-status",
            "-z",
            "--no-ext-diff",
            "--end-of-options",
        ];
        args.extend_from_slice(revs);
        args.extend_from_slice(&["--", ".", EXCLUDE_VIBE]);
        let out = self.git.output(&args).await.map_err(|e| e.to_string())?;
        if out.success {
            Ok(parse_name_status(&out.stdout))
        } else {
            Err(format!(
                "git diff {}: {}",
                revs.join(" "),
                out.stderr.trim()
            ))
        }
    }

    /// Files between the base and the tip of an existing task branch.
    async fn branch_files(&self, branch: &str) -> std::result::Result<ChangedFiles, String> {
        let base = self.base_of(branch).await?;
        let files = self.diff_files(&[&format!("{base}...{branch}")]).await?;
        Ok(ChangedFiles {
            source: ChangedFilesSource::Branch {
                branch: branch.to_string(),
                base,
            },
            approximate: false,
            files,
        })
    }

    /// Files brought in by a recorded merge.
    ///
    /// The merge is a fast-forward when git says so (one parent) or when the
    /// merged commit is one the task made: the tip of a task branch is often
    /// the merge commit of a subtask integration, which the final merge
    /// fast-forwards to. A true merge commit is compared with its first
    /// parent. A fast-forward is compared from the parent of the oldest
    /// recorded commit of the task (`task_commits`, in log order) that the
    /// merged commit contains: commits thrown away by a reset are skipped.
    /// Without such a commit, only the merged commit itself is compared with
    /// its parent and the list is marked approximate.
    async fn merge_files(
        &self,
        merge: &MergeRecord,
        task_commits: &[&str],
    ) -> std::result::Result<ChangedFiles, String> {
        let out = self
            .git
            .output(&[
                "rev-list",
                "--parents",
                "-n",
                "1",
                "--end-of-options",
                &merge.commit,
            ])
            .await
            .map_err(|e| e.to_string())?;
        if !out.success {
            return Err(format!(
                "merge commit {} not found: {}",
                merge.commit,
                out.stderr.trim()
            ));
        }
        let parents = out.stdout.split_whitespace().count().saturating_sub(1);
        let fast_forward = parents < 2 || task_commits.contains(&merge.commit.as_str());
        let mut first = None;
        if fast_forward {
            for commit in task_commits {
                if self
                    .git
                    .is_ancestor(commit, &merge.commit)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    first = Some(*commit);
                    break;
                }
            }
        }
        let from = format!("{}^1", first.unwrap_or(&merge.commit));
        let files = self.diff_files(&[&from, &merge.commit]).await?;
        Ok(ChangedFiles {
            source: ChangedFilesSource::MergeCommit {
                commit: merge.commit.clone(),
                fast_forward,
            },
            approximate: fast_forward && first.is_none(),
            files,
        })
    }
}

/// Totals over every run of a task.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HistoryTotals {
    /// Tokens of the runs whose totals are known.
    pub usage: Usage,
    /// Active time of the runs whose totals are known, in milliseconds.
    pub active_ms: u64,
    /// Number of runs.
    pub runs: u32,
    /// Number of commits in the task workspace.
    pub commits: u32,
    /// Whether the totals of every run are known.
    pub complete: bool,
}

/// The last QA review of a task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QaSummary {
    /// Review round (1-based).
    pub round: u32,
    /// Verdict.
    pub verdict: QaVerdict,
    /// Summary of what was checked.
    pub summary: String,
    /// Number of issues found.
    pub issues: usize,
}

/// What the models used by a task cost, from `[pricing]`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Cost {
    /// Amount.
    pub amount: f64,
    /// Currency of the prices (`USD`).
    pub currency: String,
    /// False when the amount is a lower bound: an agent session started
    /// but never finished (a crash, its tokens are not logged), or a run
    /// used more tokens than its sessions account for (calls made outside a
    /// session, such as the repair of an invalid structured answer).
    pub complete: bool,
}

/// Cost of the agent sessions of a log, or `None` when a session used a
/// model without a price (or a kind of token its price does not cover).
///
/// Each `agent_finished` is paired with the oldest unfinished
/// `agent_started` of the same run and role, whose `model` names the price.
/// Sessions logged before 0.5 have no model and have no cost. The tokens of
/// the sessions of a run are checked against the run's totals
/// (`run_finished`) to set [`Cost::complete`].
///
/// A `run_started` drops the sessions of that run still open: they died
/// with the previous process and must not be paired with the sessions of
/// the resume. One limit remains: within one process, sessions of one role
/// run in parallel are paired in start order, which is right only because
/// they share a model (the model of their phase).
#[must_use]
pub fn cost_of(events: &[Envelope], config: &VibeConfig) -> Option<Cost> {
    if config.pricing.is_empty() {
        return None;
    }
    let mut started: HashMap<(RunId, AgentRole), VecDeque<String>> = HashMap::new();
    let mut sessions: HashMap<RunId, Usage> = HashMap::new();
    let mut totals: HashMap<RunId, Usage> = HashMap::new();
    let mut amount = 0.0;
    let mut lost = false;
    for env in events {
        match &env.event {
            Event::RunStarted { run, .. } => {
                for ((r, _), open) in &mut started {
                    if r == run && !open.is_empty() {
                        lost = true;
                        open.clear();
                    }
                }
            }
            Event::RunFinished {
                run,
                usage,
                started_at,
                ..
            } if *started_at != DateTime::<Utc>::UNIX_EPOCH => {
                totals.insert(*run, *usage);
            }
            Event::AgentStarted {
                run, role, model, ..
            } => started
                .entry((*run, role.clone()))
                .or_default()
                .push_back(model.clone()),
            Event::AgentFinished {
                run, role, usage, ..
            } => {
                let model = started
                    .get_mut(&(*run, role.clone()))
                    .and_then(VecDeque::pop_front)?;
                amount += config.price_of(&model)?.cost(*usage)?;
                let sum = sessions.entry(*run).or_default();
                *sum = sum.combined(*usage);
            }
            _ => {}
        }
    }
    let covered = totals.iter().all(|(run, total)| {
        let sum = sessions.get(run).copied().unwrap_or_default();
        sum.input_tokens >= total.input_tokens
            && sum.output_tokens >= total.output_tokens
            && sum.cache_read_tokens >= total.cache_read_tokens
            && sum.cache_write_tokens >= total.cache_write_tokens
    });
    Some(Cost {
        amount,
        currency: "USD".into(),
        complete: !lost && covered && started.values().all(VecDeque::is_empty),
    })
}

/// Everything known about one task.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaskHistory {
    /// The task.
    pub task: Task,
    /// Task sequence number.
    pub number: u32,
    /// Runs, in the order they started.
    pub runs: Vec<RunSummary>,
    /// Totals over the runs.
    pub totals: HistoryTotals,
    /// Files the task changed.
    pub changed_files: ChangedFiles,
    /// Last QA review, if any.
    pub last_qa: Option<QaSummary>,
    /// Whether the last result of every required validation command of the
    /// last run that ran any passed.
    pub validations_passed: Option<bool>,
    /// Cost, when every model used has a price (see [`cost_of`]).
    pub cost: Option<Cost>,
    /// Time of the last event, or of the last change of the task.
    pub last_activity: DateTime<Utc>,
    /// Problems met while reading git; the changed files then come from a
    /// less exact source.
    pub errors: Vec<String>,
}

/// Which tasks [`project_history`] covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryFilter {
    /// Also failed and cancelled tasks, not only ready and done ones.
    pub all: bool,
}

impl HistoryFilter {
    /// Whether a task with this status is covered.
    #[must_use]
    pub fn accepts(self, status: TaskStatus) -> bool {
        match status {
            TaskStatus::Ready | TaskStatus::Done => true,
            TaskStatus::Failed | TaskStatus::Cancelled => self.all,
            _ => false,
        }
    }
}

fn validations_passed(runs: &[RunSummary]) -> Option<bool> {
    let run = runs.iter().rev().find(|r| !r.validations.is_empty())?;
    let mut last: BTreeMap<(&str, bool), bool> = BTreeMap::new();
    for v in &run.validations {
        last.insert((v.command.as_str(), v.integration), v.passed);
    }
    Some(last.values().all(|p| *p))
}

async fn build_history(
    store: &dyn PipelineStore,
    repo: Option<&Repo>,
    project_root: &Path,
    config: &VibeConfig,
    task: Task,
    number: u32,
    branch: Option<&str>,
) -> TaskHistory {
    // Files of this task that cannot be read are reported on it, never fail
    // the whole history.
    let mut errors: Vec<String> = repo.map(|r| r.errors.clone()).unwrap_or_default();
    let events = store.load_events(task.id).await.unwrap_or_else(|e| {
        errors.push(e.to_string());
        Vec::new()
    });
    let mut runs = summarize_runs(&events);
    match store.load_run_state(task.id).await {
        Ok(Some(state)) => {
            if let Some(run) = runs.iter_mut().find(|r| r.run == state.run_id) {
                run.apply_run_state(&state);
            }
        }
        Ok(None) => {}
        Err(e) => errors.push(e.to_string()),
    }
    if let Some(last) = runs.last_mut()
        && last.state == RunSummaryState::Interrupted
    {
        match store.is_running(task.id).await {
            Ok(true) => last.state = RunSummaryState::Running,
            Ok(false) => {}
            Err(e) => errors.push(e.to_string()),
        }
    }

    let mut changed_files = None;
    if let Some(repo) = repo {
        // A merged branch that still exists diffs as empty against its base:
        // the merge goes first.
        if let Some(merge) = runs.iter().rev().find_map(|r| r.merged.as_ref()) {
            let commits: Vec<&str> = runs
                .iter()
                .flat_map(|r| &r.commits)
                .map(|c| c.commit.as_str())
                .collect();
            match repo.merge_files(merge, &commits).await {
                Ok(files) => changed_files = Some(files),
                Err(e) => errors.push(e),
            }
        }
        if changed_files.is_none()
            && let Some(branch) = branch.filter(|b| repo.branches.contains(*b))
        {
            // An empty diff says nothing (a branch merged outside vibe, for
            // instance): try the next source.
            match repo.branch_files(branch).await {
                Ok(files) if !files.files.is_empty() => changed_files = Some(files),
                Ok(_) => {}
                Err(e) => errors.push(e),
            }
        }
    }
    let changed_files = changed_files.unwrap_or_else(|| {
        let committed: BTreeSet<String> = runs
            .iter()
            .flat_map(|r| &r.commits)
            .flat_map(|c| c.files.iter().cloned())
            .collect();
        if !committed.is_empty() {
            return ChangedFiles::unknown_statuses(ChangedFilesSource::Commits, committed);
        }
        let written: BTreeSet<String> =
            files_written(&trace::pair_all_calls(&events, project_root))
                .into_iter()
                .collect();
        if written.is_empty() {
            ChangedFiles::none()
        } else {
            ChangedFiles::unknown_statuses(ChangedFilesSource::Trace, written)
        }
    });

    let totals = HistoryTotals {
        usage: runs
            .iter()
            .filter(|r| r.totals_known)
            .fold(Usage::default(), |acc, r| acc.combined(r.usage)),
        active_ms: runs
            .iter()
            .filter(|r| r.totals_known)
            .map(|r| r.active_ms)
            .sum(),
        runs: u32::try_from(runs.len()).unwrap_or(u32::MAX),
        commits: u32::try_from(runs.iter().map(|r| r.commits.len()).sum::<usize>())
            .unwrap_or(u32::MAX),
        complete: runs.iter().all(|r| r.totals_known),
    };
    let reports = store.load_qa_reports(task.id).await.unwrap_or_else(|e| {
        errors.push(e.to_string());
        Vec::new()
    });
    let last_qa = reports.into_iter().next_back().map(|r| QaSummary {
        round: r.round,
        verdict: r.verdict,
        summary: r.summary,
        issues: r.issues.len(),
    });
    let last_activity = events
        .iter()
        .map(|e| e.at)
        .max()
        .map_or(task.updated_at, |at| at.max(task.updated_at));
    TaskHistory {
        validations_passed: validations_passed(&runs),
        cost: cost_of(&events, config),
        task,
        number,
        runs,
        totals,
        changed_files,
        last_qa,
        last_activity,
        errors,
    }
}

/// History of one task. `branch` is its task branch, if it has one (the
/// caller knows the workspace provider: `task.branch`, or the branch the
/// worktree provider derives for tasks run before 0.5).
pub async fn task_history(
    store: &dyn PipelineStore,
    project_root: &Path,
    config: &VibeConfig,
    task: Task,
    branch: Option<&str>,
) -> Result<TaskHistory> {
    let number = store
        .task_numbers()
        .await?
        .get(&task.id)
        .copied()
        .unwrap_or(0);
    let repo = Repo::open(project_root, config).await;
    Ok(build_history(
        store,
        repo.as_ref(),
        project_root,
        config,
        task,
        number,
        branch,
    )
    .await)
}

/// History of the project's finished tasks (see [`HistoryFilter`]), most
/// recent activity first. `branch_of` gives each task's branch, as for
/// [`task_history`]. Git is read once for the project, then once or twice
/// per task.
pub async fn project_history(
    store: &dyn PipelineStore,
    project_root: &Path,
    config: &VibeConfig,
    filter: HistoryFilter,
    branch_of: &(dyn Fn(&Task) -> Option<String> + Sync),
) -> Result<Vec<TaskHistory>> {
    let numbers = store.task_numbers().await?;
    let repo = Repo::open(project_root, config).await;
    let mut out = Vec::new();
    for task in store.list_tasks().await? {
        if !filter.accepts(task.status) {
            continue;
        }
        let number = numbers.get(&task.id).copied().unwrap_or(0);
        let branch = branch_of(&task);
        out.push(
            build_history(
                store,
                repo.as_ref(),
                project_root,
                config,
                task,
                number,
                branch.as_deref(),
            )
            .await,
        );
    }
    out.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| b.number.cmp(&a.number))
    });
    Ok(out)
}
