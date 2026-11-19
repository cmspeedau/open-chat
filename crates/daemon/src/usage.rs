//! Turn usage ledger + gateway quota surfacing (session panel usage view).
//!
//! One JSON line per completed turn in `.openchat/usage.jsonl`: provider-
//! reported tokens when the backend gives them, an estimate-only marker
//! when it does not (opencode gateway), plus prompt composition size.
//! Totals and the recent window are computed on read; the file stays
//! append-only so a crash mid-turn loses at most that one line.

use std::path::{Path, PathBuf};

use openchat_session::agent::Usage as TurnUsage;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub ts: u64,
    pub model: String,
    pub via: String,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub prompt_est_tokens: usize,
    pub iterations: usize,
    /// True when the backend did not report real token counts.
    pub estimated: bool,
}

#[must_use]
pub fn ledger_path(cwd: &Path) -> PathBuf {
    cwd.join(".openchat").join("usage.jsonl")
}

pub fn record(cwd: &Path, entry: &LedgerEntry) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = ledger_path(cwd);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = json!({
        "ts": entry.ts,
        "model": entry.model,
        "via": entry.via,
        "input": entry.input,
        "output": entry.output,
        "prompt_est": entry.prompt_est_tokens,
        "iterations": entry.iterations,
        "estimated": entry.estimated,
    });
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Build an entry straight from a turn's usage plus composition size.
#[must_use]
pub fn entry_from(usage: &TurnUsage, prompt_est_tokens: usize, iterations: usize) -> LedgerEntry {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    LedgerEntry {
        ts: now,
        model: if usage.model.is_empty() {
            "?".into()
        } else {
            usage.model.clone()
        },
        via: usage.via.clone(),
        input: usage.input_tokens,
        output: usage.output_tokens,
        prompt_est_tokens,
        iterations,
        estimated: !usage.reported(),
    }
}

/// Summaries for the panel: totals by backend (reported tokens kept
/// separate from estimated-only turns, never mixed), plus a recent window.
#[must_use]
pub fn summary(cwd: &Path, recent_n: usize) -> Value {
    let path = ledger_path(cwd);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!({"turns": 0, "reported_input": 0, "reported_output": 0,
            "estimated_turns": 0, "by_model": {}, "recent": []});
    };
    let mut turns = 0u64;
    let mut rep_in = 0u64;
    let mut rep_out = 0u64;
    let mut est_turns = 0u64;
    let mut by_model: std::collections::BTreeMap<String, (u64, u64, u64, u64)> =
        std::collections::BTreeMap::new();
    let mut entries: Vec<Value> = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        turns += 1;
        let estimated = v["estimated"].as_bool().unwrap_or(false);
        let input = v["input"].as_u64();
        let output = v["output"].as_u64();
        if !estimated {
            rep_in += input.unwrap_or(0);
            rep_out += output.unwrap_or(0);
        } else {
            est_turns += 1;
        }
        let model = v["model"].as_str().unwrap_or("?").to_owned();
        let e = by_model.entry(model.clone()).or_insert((0, 0, 0, 0));
        e.0 += 1;
        e.1 += input.unwrap_or(0);
        e.2 += output.unwrap_or(0);
        e.3 += v["prompt_est"].as_u64().unwrap_or(0);
        entries.push(json!({
            "ts": v["ts"], "model": model, "via": v["via"],
            "input": input, "output": output, "estimated": estimated,
        }));
    }
    let start = entries.len().saturating_sub(recent_n);
    let recent: Vec<Value> = entries[start..].iter().rev().cloned().collect();
    json!({
        "turns": turns,
        "reported_input": rep_in,
        "reported_output": rep_out,
        "estimated_turns": est_turns,
        "by_model": by_model
            .into_iter()
            .map(|(m, (n, i, o, p))| (m, json!({"turns": n, "input": i, "output": o, "prompt_est": p})))
            .collect::<serde_json::Map<_, _>>(),
        "recent": recent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> TurnUsage {
        TurnUsage {
            input_tokens: Some(120),
            output_tokens: Some(34),
            model: "mock".into(),
            via: "openai-compat".into(),
        }
    }

    #[test]
    fn record_and_summarize() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), &entry_from(&usage(), 500, 1)).unwrap();
        let mut est = usage();
        est.input_tokens = None;
        est.output_tokens = None;
        est.model = "opencode-go/qwen4-max".into();
        est.via = "opencode".into();
        record(dir.path(), &entry_from(&est, 600, 2)).unwrap();
        let s = summary(dir.path(), 10);
        assert_eq!(s["turns"], 2);
        assert_eq!(s["reported_input"], 120);
        assert_eq!(s["reported_output"], 34);
        assert_eq!(
            s["estimated_turns"], 1,
            "gateway turn counted, not summed as real"
        );
        assert_eq!(s["recent"].as_array().unwrap().len(), 2);
        assert_eq!(
            s["recent"][0]["model"], "opencode-go/qwen4-max",
            "newest first"
        );
    }

    #[test]
    fn missing_ledger_is_zero_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = summary(&dir.path().join("nope"), 5);
        assert_eq!(s["turns"], 0);
    }
}
