//! `vibe trace`: the tool calls of a task's run — arguments, results and
//! the traced outputs.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;
use vibe_core::{RunId, SubtaskId};
use vibe_pipeline::trace::files_written;
use vibe_pipeline::{Call, PairedBy, PipelineStore, RunTrace, read_output, run_trace};

use super::{resolve_task, subtask_labels};
use crate::app::open_store;
use crate::cli::TraceArgs;
use crate::util::{Ui, human_duration, print_out, style};

/// Longest string argument shown without `--full`, in characters.
const ARG_MAX_CHARS: usize = 2000;

/// Run `vibe trace`.
pub async fn run(root: &Path, args: TraceArgs, ui: Ui) -> Result<u8> {
    let store = open_store(root)?;
    let task = resolve_task(&store, &args.reference).await?;
    let events = store.load_events(task.id).await?;
    let mut runs: Vec<RunId> = Vec::new();
    for run in events.iter().filter_map(|e| e.event.run_id()) {
        if !runs.contains(&run) {
            runs.push(run);
        }
    }
    if runs.is_empty() {
        if ui.json {
            ui.print_json(&if args.all {
                Value::Array(vec![])
            } else {
                Value::Null
            });
        } else {
            println!("Task has not been run yet.");
        }
        return Ok(0);
    }
    let wanted: Vec<Option<RunId>> = match (&args.run, args.all) {
        (_, true) => runs.iter().copied().map(Some).collect(),
        (Some(prefix), false) => vec![Some(match_prefix(prefix, &runs, "run")?)],
        (None, false) => vec![None],
    };

    let subtask = match &args.subtask {
        Some(prefix) => {
            let mut ids: Vec<SubtaskId> = Vec::new();
            for call in vibe_pipeline::pair_all_calls(&events, root) {
                if let Some(id) = call.subtask
                    && !ids.contains(&id)
                {
                    ids.push(id);
                }
            }
            Some(match_prefix(prefix, &ids, "subtask")?)
        }
        None => None,
    };

    let mut traces = Vec::new();
    for run in wanted {
        let Some(mut trace) = run_trace(&*store, root, task.id, run).await? else {
            continue;
        };
        // Numbers stay those of the whole run, whatever the filters keep.
        let numbered: Vec<(usize, Call)> = std::mem::take(&mut trace.calls)
            .into_iter()
            .enumerate()
            .filter(|(_, c)| args.tool.as_ref().is_none_or(|t| &c.tool == t))
            .filter(|(_, c)| subtask.is_none_or(|s| c.subtask == Some(s)))
            .map(|(i, c)| (i + 1, c))
            .collect();
        trace.calls = numbered.iter().map(|(_, c)| c.clone()).collect();
        trace.files_written = files_written(&trace.calls);
        traces.push((
            numbered.into_iter().map(|(i, _)| i).collect::<Vec<_>>(),
            trace,
        ));
    }

    if ui.json {
        let value = if args.all {
            serde_json::to_value(traces.iter().map(|(_, t)| t).collect::<Vec<_>>())?
        } else {
            serde_json::to_value(traces.first().map(|(_, t)| t))?
        };
        ui.print_json(&value);
        return Ok(0);
    }

    let labels = subtask_labels(&store, &task).await;
    for (numbers, trace) in &traces {
        let text = render_trace(root, trace, numbers, &labels, args.full).await;
        if !print_out(&text)? {
            return Ok(0);
        }
    }
    Ok(0)
}

/// The only one of `ids` whose text starts with `prefix`.
fn match_prefix<T: Copy + std::fmt::Display>(prefix: &str, ids: &[T], what: &str) -> Result<T> {
    let prefix = prefix.trim().to_ascii_lowercase();
    let found: Vec<T> = ids
        .iter()
        .copied()
        .filter(|id| !prefix.is_empty() && id.to_string().starts_with(&prefix))
        .collect();
    match found.as_slice() {
        [one] => Ok(*one),
        [] => bail!("no {what} of this task matches `{prefix}`"),
        _ => bail!("`{prefix}` matches several {what}s of this task: give more characters"),
    }
}

/// One run: a header, a block per call, a footer.
async fn render_trace(
    root: &Path,
    trace: &RunTrace,
    numbers: &[usize],
    labels: &HashMap<SubtaskId, String>,
    full: bool,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}",
        style::accent().apply_to(format!("── run {} ──────────", trace.run.short()))
    );
    for (number, call) in numbers.iter().zip(&trace.calls) {
        out.push('\n');
        out.push_str(&render_call(root, *number, call, labels, full).await);
    }
    let errors = trace
        .calls
        .iter()
        .filter(|c| c.is_error == Some(true))
        .count();
    let errors = if errors > 0 {
        style::err()
            .apply_to(format!("{errors} error(s)"))
            .to_string()
    } else {
        "no error".to_string()
    };
    let _ = writeln!(out, "\n{} call(s), {errors}", trace.calls.len());
    if !trace.files_written.is_empty() {
        let _ = writeln!(out, "files written: {}", trace.files_written.join(", "));
    }
    out
}

/// `#3 coder · subtask 1/2 Write · write_file  12 ms  ok  [call id]`,
/// then the arguments and the output.
async fn render_call(
    root: &Path,
    number: usize,
    call: &Call,
    labels: &HashMap<SubtaskId, String>,
    full: bool,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let subtask = call
        .subtask
        .map(|id| {
            let label = labels.get(&id).cloned().unwrap_or_else(|| id.short());
            format!(" · subtask {label}")
        })
        .unwrap_or_default();
    let duration = call
        .duration_ms
        .map(|ms| format!("  {}", human_duration(Duration::from_millis(ms))))
        .unwrap_or_default();
    let result = match (call.is_error, call.exit_code, call.timed_out) {
        (_, _, true) => style::err().apply_to("timed out".to_string()),
        (Some(true), Some(code), _) => style::err().apply_to(format!("error, exit {code}")),
        (Some(true), None, _) => style::err().apply_to("error".to_string()),
        (Some(false), Some(code), _) => style::ok().apply_to(format!("exit {code}")),
        (Some(false), None, _) => style::ok().apply_to("ok".to_string()),
        (None, _, _) => style::warn().apply_to("no result".to_string()),
    };
    let id = if call.call.is_nil() {
        "-".to_string()
    } else {
        call.call.to_string()
    };
    let paired = match call.paired {
        PairedBy::Order => ", paired: by order",
        PairedBy::Id | PairedBy::Unmatched => "",
    };
    let _ = writeln!(
        out,
        "{} {}{subtask} · {}{duration}  {result}  {}",
        style::bold().apply_to(format!("#{number}")),
        call.role,
        style::bold().apply_to(&call.tool),
        style::dim().apply_to(format!("[call {id}{paired}]"))
    );

    if !call.input.is_null() {
        let input = if full {
            call.input.clone()
        } else {
            shorten_strings(&call.input, ARG_MAX_CHARS)
        };
        let text = serde_json::to_string_pretty(&input).unwrap_or_default();
        let _ = writeln!(out, "{}", indent(&text, "    "));
    }

    let output = if full {
        match read_output(root, call).await {
            Ok(Some(text)) => Ok(text),
            Ok(None) if call.is_error.is_none() => {
                Err("no result: the call never returned (run interrupted)".to_string())
            }
            Ok(None) if call.output_file.is_none() => {
                Err("output not traced (tracing off, or logged before 0.5)".to_string())
            }
            Ok(None) => Err("output file missing (discarded or cleaned up)".to_string()),
            Err(e) => Err(format!("cannot read the output: {e}")),
        }
    } else {
        Err(String::new())
    };
    match output {
        Ok(text) => {
            let _ = writeln!(out, "  {}", style::dim().apply_to("⟵ output"));
            let styled = if call.is_error == Some(true) {
                style::err().apply_to(text.trim_end()).to_string()
            } else {
                text.trim_end().to_string()
            };
            let _ = writeln!(out, "{}", indent(&styled, "    "));
        }
        Err(note) => {
            if !note.is_empty() {
                let _ = writeln!(out, "  {}", style::warn().apply_to(note));
            }
            if call.is_error.is_some() {
                let shown = call.preview.chars().count() as u64;
                let more = if call.output_chars > shown {
                    format!(
                        " (preview, {} chars in total{})",
                        call.output_chars,
                        if full || call.output_file.is_none() {
                            ""
                        } else {
                            ", --full to read them"
                        }
                    )
                } else {
                    String::new()
                };
                let _ = writeln!(
                    out,
                    "  {}",
                    style::dim().apply_to(format!("⟵ output{more}"))
                );
                let preview = call.preview.trim_end();
                let styled = if call.is_error == Some(true) {
                    style::err().apply_to(preview).to_string()
                } else {
                    preview.to_string()
                };
                if !preview.is_empty() {
                    let _ = writeln!(out, "{}", indent(&styled, "    "));
                }
            }
        }
    }
    out
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|l| format!("{prefix}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `value` with every string longer than `max` characters cut, noting how
/// many characters were left out. Line breaks are kept.
fn shorten_strings(value: &Value, max: usize) -> Value {
    match value {
        Value::String(s) => {
            let len = s.chars().count();
            if len <= max {
                value.clone()
            } else {
                let kept: String = s.chars().take(max).collect();
                Value::String(format!("{kept}… ({} more chars, --full)", len - max))
            }
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| shorten_strings(v, max)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), shorten_strings(v, max)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn long_strings_are_cut_by_characters() {
        let input = json!({"path": "a.rs", "content": "é".repeat(10), "lines": ["xxxxxx", 3]});
        let short = shorten_strings(&input, 4);
        assert_eq!(short["path"], "a.rs");
        assert_eq!(short["content"], "éééé… (6 more chars, --full)");
        assert_eq!(short["lines"], json!(["xxxx… (2 more chars, --full)", 3]));
    }

    #[test]
    fn prefixes_must_match_one_id() {
        let a = RunId::parse("aaaa1111-0000-4000-8000-000000000000").unwrap();
        let b = RunId::parse("aaaa2222-0000-4000-8000-000000000000").unwrap();
        assert_eq!(match_prefix("AAAA1", &[a, b], "run").unwrap(), a);
        assert!(match_prefix("aaaa", &[a, b], "run").is_err());
        assert!(match_prefix("bbbb", &[a, b], "run").is_err());
        assert!(match_prefix("", &[a], "run").is_err());
    }
}
