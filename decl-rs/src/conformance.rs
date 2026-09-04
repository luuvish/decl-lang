//! Conformance judging — a port of the reference implementation's
//! conformance.ts: judges fixtures by their declared phase.
//!   valid/*                          -> parse + checks clean + outputs evaluate clean
//!   invalid @expect-phase: parsing   -> must fail to parse
//!   invalid @expect-phase: checking  -> parses; static checks report @expect-error
//!   invalid @expect-phase: binding   -> parses; the pipeline reports @expect-error
use crate::checker::check_module;
use crate::parse::parse_source;
use crate::pipeline::run_pipeline;
use crate::semantics::Diag;
use std::path::{Path, PathBuf};

pub struct Verdict {
    pub file: PathBuf,
    pub ok: bool,
    pub detail: String,
}

pub fn walk_decl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk_decl(&p, out);
        } else if p.extension().map(|x| x == "decl").unwrap_or(false) {
            out.push(p);
        }
    }
}

pub fn judge_fixture(file: &Path, is_valid: bool) -> Verdict {
    let src = std::fs::read_to_string(file).unwrap_or_default();
    let meta = |key: &str| -> Option<String> {
        src.lines().find_map(|l| l.strip_prefix(&format!("// @{key}:")).map(|v| v.trim().to_string()))
    };
    let phase = meta("expect-phase");
    let want = meta("expect-error").unwrap_or_default();
    let want_msg = meta("expect-message").unwrap_or_default();
    let parsed = parse_source(&src);
    let json_of = |ds: &[Diag]| format!("[{}]", ds.iter().map(|d| d.to_json(None)).collect::<Vec<_>>().join(","));
    let hit = |ds: &[Diag]| ds.iter().any(|d| d.code.as_deref() == Some(want.as_str())) && (want_msg.is_empty() || ds.iter().any(|d| d.message.contains(&want_msg)));
    let verdict = |ok: bool, detail: String| Verdict { file: file.to_path_buf(), ok, detail };
    if is_valid {
        // a valid fixture must parse, check clean, AND evaluate its outputs
        // without error-severity diagnostics
        if !parsed.errors.is_empty() {
            return verdict(false, format!("{} parse errors", parsed.errors.len()));
        }
        let checks = check_module(&parsed.decls, None, None);
        let eval_errs: Vec<Diag> = if checks.is_empty() {
            run_pipeline(&parsed.decls).diags.into_iter().filter(|d| d.severity == "error").collect()
        } else {
            vec![]
        };
        let all: Vec<Diag> = checks.into_iter().chain(eval_errs).collect();
        return verdict(all.is_empty(), json_of(&all));
    }
    match phase.as_deref() {
        Some("parsing") => verdict(!parsed.errors.is_empty(), "expected parse errors, got none".into()),
        Some("checking") => {
            let checks = if parsed.errors.is_empty() { check_module(&parsed.decls, None, None) } else { vec![] };
            verdict(hit(&checks), json_of(&checks))
        }
        Some("binding") => {
            let diags = if parsed.errors.is_empty() { run_pipeline(&parsed.decls).diags } else { vec![] };
            verdict(hit(&diags), json_of(&diags))
        }
        other => verdict(false, format!("unknown phase {other:?}")),
    }
}

pub fn judge_corpus(dir: &Path) -> Vec<Verdict> {
    let mut files = vec![];
    walk_decl(dir, &mut files);
    files.iter().map(|f| judge_fixture(f, f.to_string_lossy().contains("/valid/"))).collect()
}
