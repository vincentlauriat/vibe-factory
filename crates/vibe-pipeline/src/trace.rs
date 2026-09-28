//! The tool calls of a run, rebuilt from its event log: each `tool_called`
//! paired with its `tool_returned`, and the complete output kept in the
//! trace store (`.vibe/tool-output/<task dir>/<run>/<call>.txt`).
//!
//! Calls logged since 0.5 pair by their `call` id, which also pairs calls
//! that ran in parallel and returned out of order. Older logs have no id:
//! their calls pair in order, per role, tool and subtask, and are marked
//! [`PairedBy::Order`].

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use vibe_core::config::{TOOL_OUTPUT_DIR, VIBE_DIR};
use vibe_core::{AgentRole, CallId, Envelope, Event, Result, RunId, SubtaskId, TaskId};

use crate::store::PipelineStore;

/// Tools whose `path` argument is a file they write.
pub const WRITING_TOOLS: &[&str] = &["write_file", "edit_file"];

/// How a call was matched with its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedBy {
    /// By call id.
    Id,
    /// By order, per role, tool and subtask (logs recorded before 0.5).
    Order,
    /// No match: a call that never returned (run interrupted), or a result
    /// whose call is not in the log.
    Unmatched,
}

/// One tool call and its result.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Call {
    /// Run the call belongs to.
    pub run: RunId,
    /// Call id (nil in logs recorded before 0.5).
    pub call: CallId,
    /// Role of the agent that called the tool.
    pub role: AgentRole,
    /// Subtask the session worked on, if any.
    pub subtask: Option<SubtaskId>,
    /// Tool name.
    pub tool: String,
    /// Complete arguments (`null` for a result without its call).
    pub input: serde_json::Value,
    /// When the tool was called (the time of the result for a result
    /// without its call).
    pub called_at: DateTime<Utc>,
    /// When the tool returned, if it did.
    pub returned_at: Option<DateTime<Utc>>,
    /// Duration measured by the runner, in milliseconds.
    pub duration_ms: Option<u64>,
    /// Whether the tool failed.
    pub is_error: Option<bool>,
    /// Exit code, for tools that run a command.
    pub exit_code: Option<i64>,
    /// Whether the command was stopped by its timeout.
    pub timed_out: bool,
    /// First characters of the output.
    pub preview: String,
    /// Length of the complete output, in characters (0 when unknown).
    pub output_chars: u64,
    /// Absolute path of the complete output in the trace store, when it was
    /// traced. Paths recorded outside `.vibe/tool-output/` are dropped.
    pub output_file: Option<PathBuf>,
    /// How the call was matched with its result.
    pub paired: PairedBy,
}

impl Call {
    /// The file this call writes (`path` of [`WRITING_TOOLS`]), if any.
    #[must_use]
    pub fn written_path(&self) -> Option<&str> {
        if !WRITING_TOOLS.contains(&self.tool.as_str()) {
            return None;
        }
        self.input.get("path").and_then(serde_json::Value::as_str)
    }
}

/// The tool calls of one run.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunTrace {
    /// Run id.
    pub run: RunId,
    /// Calls in the order they were made; results without a call come at
    /// the time they returned.
    pub calls: Vec<Call>,
    /// Paths written by [`WRITING_TOOLS`] calls that returned without an
    /// error (see [`files_written`]), relative
    /// to the workspace root, in first-write order and without duplicates.
    pub files_written: Vec<String>,
}

impl RunTrace {
    /// Calls grouped by subtask (`None` first: calls outside the build),
    /// each group in call order.
    #[must_use]
    pub fn by_subtask(&self) -> BTreeMap<Option<SubtaskId>, Vec<&Call>> {
        let mut groups: BTreeMap<Option<SubtaskId>, Vec<&Call>> = BTreeMap::new();
        for call in &self.calls {
            groups.entry(call.subtask).or_default().push(call);
        }
        groups
    }
}

/// Absolute path of a trace file from its `output_file`, which must be a
/// relative path under `.vibe/tool-output/` without `..`.
#[must_use]
pub fn trace_file_path(project_root: &Path, output_file: &str) -> Option<PathBuf> {
    let relative = Path::new(output_file);
    let mut components = relative.components();
    let inside = components.next() == Some(Component::Normal(VIBE_DIR.as_ref()))
        && components.next() == Some(Component::Normal(TOOL_OUTPUT_DIR.as_ref()))
        && components.all(|c| matches!(c, Component::Normal(_)));
    inside.then(|| project_root.join(relative))
}

type OrderKey = (RunId, AgentRole, String, Option<SubtaskId>);

/// The calls of `run` found in `events`, paired with their results.
/// `project_root` resolves the trace files.
#[must_use]
pub fn pair_calls(events: &[Envelope], run: RunId, project_root: &Path) -> Vec<Call> {
    pair(
        events.iter().filter(|e| e.event.run_id() == Some(run)),
        project_root,
    )
}

/// The calls of every run found in `events`, paired with their results, in
/// one pass over the log.
#[must_use]
pub fn pair_all_calls(events: &[Envelope], project_root: &Path) -> Vec<Call> {
    pair(events.iter(), project_root)
}

fn pair<'a>(events: impl Iterator<Item = &'a Envelope>, project_root: &Path) -> Vec<Call> {
    let mut calls: Vec<Call> = Vec::new();
    let mut by_id: HashMap<(RunId, CallId), usize> = HashMap::new();
    let mut by_order: HashMap<OrderKey, VecDeque<usize>> = HashMap::new();
    for env in events {
        match &env.event {
            Event::ToolCalled {
                run,
                role,
                tool,
                input,
                call,
                subtask,
            } => {
                let index = calls.len();
                calls.push(Call {
                    run: *run,
                    call: *call,
                    role: role.clone(),
                    subtask: *subtask,
                    tool: tool.clone(),
                    input: input.clone(),
                    called_at: env.at,
                    returned_at: None,
                    duration_ms: None,
                    is_error: None,
                    exit_code: None,
                    timed_out: false,
                    preview: String::new(),
                    output_chars: 0,
                    output_file: None,
                    paired: PairedBy::Unmatched,
                });
                if call.is_nil() {
                    by_order
                        .entry((*run, role.clone(), tool.clone(), *subtask))
                        .or_default()
                        .push_back(index);
                } else {
                    by_id.insert((*run, *call), index);
                }
            }
            Event::ToolReturned {
                run,
                role,
                tool,
                is_error,
                duration_ms,
                preview,
                call,
                subtask,
                exit_code,
                timed_out,
                output_chars,
                output_file,
            } => {
                let (matched, paired) = if call.is_nil() {
                    let key = (*run, role.clone(), tool.clone(), *subtask);
                    let found = by_order.get_mut(&key).and_then(VecDeque::pop_front);
                    (found, PairedBy::Order)
                } else {
                    (by_id.remove(&(*run, *call)), PairedBy::Id)
                };
                let index = match matched {
                    Some(i) => {
                        calls[i].paired = paired;
                        i
                    }
                    None => {
                        calls.push(Call {
                            run: *run,
                            call: *call,
                            role: role.clone(),
                            subtask: *subtask,
                            tool: tool.clone(),
                            input: serde_json::Value::Null,
                            called_at: env.at,
                            returned_at: None,
                            duration_ms: None,
                            is_error: None,
                            exit_code: None,
                            timed_out: false,
                            preview: String::new(),
                            output_chars: 0,
                            output_file: None,
                            paired: PairedBy::Unmatched,
                        });
                        calls.len() - 1
                    }
                };
                let c = &mut calls[index];
                c.returned_at = Some(env.at);
                c.duration_ms = Some(*duration_ms);
                c.is_error = Some(*is_error);
                c.exit_code = *exit_code;
                c.timed_out = *timed_out;
                c.preview.clone_from(preview);
                c.output_chars = *output_chars;
                c.output_file = output_file
                    .as_deref()
                    .and_then(|f| trace_file_path(project_root, f));
            }
            _ => {}
        }
    }
    calls
}

/// Paths written by the calls that returned without an error, without
/// duplicates.
#[must_use]
pub fn files_written(calls: &[Call]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for call in calls.iter().filter(|c| c.is_error == Some(false)) {
        if let Some(path) = call.written_path()
            && !out.iter().any(|p| p == path)
        {
            out.push(path.to_string());
        }
    }
    out
}

/// The last run of a log: the last `run_started`, else the last event with
/// a run.
#[must_use]
pub fn last_run(events: &[Envelope]) -> Option<RunId> {
    events
        .iter()
        .rev()
        .find_map(|e| match e.event {
            Event::RunStarted { run, .. } => Some(run),
            _ => None,
        })
        .or_else(|| events.iter().rev().find_map(|e| e.event.run_id()))
}

/// The tool calls of a run of `task` (its last run by default). `None`
/// when the task has no run, or no event of the requested one.
pub async fn run_trace(
    store: &dyn PipelineStore,
    project_root: &Path,
    task: TaskId,
    run: Option<RunId>,
) -> Result<Option<RunTrace>> {
    let events = store.load_events(task).await?;
    let Some(run) = run.or_else(|| last_run(&events)) else {
        return Ok(None);
    };
    if !events.iter().any(|e| e.event.run_id() == Some(run)) {
        return Ok(None);
    }
    let calls = pair_calls(&events, run, project_root);
    let files_written = files_written(&calls);
    Ok(Some(RunTrace {
        run,
        calls,
        files_written,
    }))
}

/// The complete output of a call from the trace store of the project at
/// `project_root`. `None` when it was not traced or the file is gone
/// (`vibe task discard`, manual cleanup).
///
/// The path is resolved (symbolic links included) and must stay inside
/// `.vibe/tool-output/`: a link pointing out of the trace store is refused
/// with an error.
pub async fn read_output(project_root: &Path, call: &Call) -> Result<Option<String>> {
    let Some(path) = &call.output_file else {
        return Ok(None);
    };
    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let store = tokio::fs::canonicalize(project_root.join(VIBE_DIR).join(TOOL_OUTPUT_DIR)).await?;
    if !resolved.starts_with(&store) {
        return Err(vibe_core::Error::storage(format!(
            "trace file {} resolves outside {}",
            path.display(),
            store.display()
        )));
    }
    match tokio::fs::read(&resolved).await {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use vibe_core::{Task, TaskStore};

    use crate::store::{EVENTS_FILE, FileTaskStore};

    fn env(event: Event, second: i64) -> Envelope {
        let mut e = Envelope::now(event);
        e.at = DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(1_000_000 + second);
        e
    }

    fn called(run: RunId, tool: &str, path: &str, call: CallId) -> Event {
        Event::ToolCalled {
            run,
            role: AgentRole::Coder,
            tool: tool.into(),
            input: json!({ "path": path }),
            call,
            subtask: None,
        }
    }

    fn returned(run: RunId, tool: &str, call: CallId, is_error: bool) -> Event {
        Event::ToolReturned {
            run,
            role: AgentRole::Coder,
            tool: tool.into(),
            is_error,
            duration_ms: 5,
            preview: format!("{tool} done"),
            call,
            subtask: None,
            exit_code: None,
            timed_out: false,
            output_chars: 9,
            output_file: (!call.is_nil())
                .then(|| format!(".vibe/tool-output/001-t/{run}/{call}.txt")),
        }
    }

    /// Serialise as a log line recorded before 0.5: without the fields 0.5
    /// added to tool events.
    fn old_line(e: &Envelope) -> String {
        let mut value = serde_json::to_value(e).unwrap();
        let event = value["event"].as_object_mut().unwrap();
        for field in [
            "call",
            "subtask",
            "exit_code",
            "timed_out",
            "output_chars",
            "output_file",
        ] {
            event.remove(field);
        }
        format!("{value}\n")
    }

    #[test]
    fn new_logs_pair_by_id_when_results_come_back_out_of_order() {
        let run = RunId::new();
        let (a, b, c) = (CallId::new(), CallId::new(), CallId::new());
        let events = vec![
            env(called(run, "read_file", "slow.rs", a), 0),
            env(called(run, "read_file", "fast.rs", b), 0),
            env(called(run, "write_file", "out.rs", c), 1),
            env(returned(run, "read_file", b, false), 2),
            env(returned(run, "read_file", a, false), 3),
            // Another run's events are ignored.
            env(called(RunId::new(), "bash", "x", CallId::new()), 3),
        ];
        let root = Path::new("/project");
        let calls = pair_calls(&events, run, root);
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].input["path"], "slow.rs");
        assert_eq!(calls[0].paired, PairedBy::Id);
        assert_eq!(calls[0].returned_at, Some(events[4].at));
        assert_eq!(calls[1].returned_at, Some(events[3].at));
        assert_eq!(
            calls[1].output_file,
            Some(root.join(format!(".vibe/tool-output/001-t/{run}/{b}.txt")))
        );
        // The write never returned: unmatched, and not counted as written.
        assert_eq!(calls[2].paired, PairedBy::Unmatched);
        assert_eq!(calls[2].returned_at, None);
        assert!(files_written(&calls).is_empty());
    }

    #[test]
    fn old_logs_pair_by_order_per_tool() {
        let run = RunId::new();
        let nil = CallId::nil();
        let lines = [
            env(called(run, "read_file", "a.rs", nil), 0),
            env(called(run, "write_file", "b.rs", nil), 1),
            env(returned(run, "write_file", nil, false), 2),
            env(called(run, "read_file", "c.rs", nil), 3),
            env(returned(run, "read_file", nil, false), 4),
            env(returned(run, "read_file", nil, true), 5),
            env(returned(run, "bash", nil, false), 6),
        ]
        .iter()
        .map(old_line)
        .collect::<String>();
        let events = crate::store::parse_event_log(&lines);
        assert_eq!(events.len(), 7);
        let calls = pair_calls(&events, run, Path::new("/p"));
        let summary: Vec<(&str, Option<bool>, PairedBy)> = calls
            .iter()
            .map(|c| {
                (
                    c.input["path"].as_str().unwrap_or("-"),
                    c.is_error,
                    c.paired,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("a.rs", Some(false), PairedBy::Order),
                ("b.rs", Some(false), PairedBy::Order),
                ("c.rs", Some(true), PairedBy::Order),
                ("-", Some(false), PairedBy::Unmatched),
            ]
        );
        assert!(calls.iter().all(|c| c.output_file.is_none()));
        assert_eq!(files_written(&calls), vec!["b.rs"]);
    }

    #[test]
    fn trace_paths_stay_in_the_trace_store() {
        let root = Path::new("/p");
        assert_eq!(
            trace_file_path(root, ".vibe/tool-output/001-a/r/c.txt"),
            Some(root.join(".vibe/tool-output/001-a/r/c.txt"))
        );
        for bad in [
            "/etc/passwd",
            ".vibe/tool-output/../config.toml",
            ".vibe/tasks/x",
            "src/main.rs",
            "",
        ] {
            assert_eq!(trace_file_path(root, bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn run_trace_defaults_to_the_last_run_and_reads_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTaskStore::open(dir.path()).unwrap();
        let task = Task::new("T", "");
        store.save_task(&task).await.unwrap();
        let (first, second) = (RunId::new(), RunId::new());
        let call = CallId::new();
        let mut log = String::new();
        for (i, e) in [
            Event::RunStarted {
                run: first,
                task: task.id,
            },
            called(first, "bash", "-", CallId::new()),
            Event::RunStarted {
                run: second,
                task: task.id,
            },
            called(second, "write_file", "src/a.rs", call),
            returned(second, "write_file", call, false),
        ]
        .into_iter()
        .enumerate()
        {
            log.push_str(&serde_json::to_string(&env(e, i as i64)).unwrap());
            log.push('\n');
        }
        let task_dir = store.task_dir(task.id).await.unwrap();
        std::fs::write(task_dir.join(EVENTS_FILE), log).unwrap();
        let output = dir
            .path()
            .join(format!(".vibe/tool-output/001-t/{second}/{call}.txt"));
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::write(&output, "complete output").unwrap();

        let trace = run_trace(&store, dir.path(), task.id, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(trace.run, second);
        assert_eq!(trace.files_written, vec!["src/a.rs"]);
        assert_eq!(
            read_output(dir.path(), &trace.calls[0])
                .await
                .unwrap()
                .as_deref(),
            Some("complete output")
        );
        let earlier = run_trace(&store, dir.path(), task.id, Some(first))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(earlier.calls.len(), 1);
        assert_eq!(
            read_output(dir.path(), &earlier.calls[0]).await.unwrap(),
            None
        );

        // A link out of the trace store is refused.
        #[cfg(unix)]
        {
            let secret = dir.path().join("secret.txt");
            std::fs::write(&secret, "secret").unwrap();
            std::fs::remove_file(&output).unwrap();
            std::os::unix::fs::symlink(&secret, &output).unwrap();
            let err = read_output(dir.path(), &trace.calls[0]).await.unwrap_err();
            assert!(err.to_string().contains("outside"), "{err}");
        }
        assert!(
            run_trace(&store, dir.path(), task.id, Some(RunId::new()))
                .await
                .unwrap()
                .is_none()
        );
    }
}
