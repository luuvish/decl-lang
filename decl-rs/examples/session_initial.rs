//! One initial public Session request. Supporting diagnostics, never primary A/B timing.
use decl_lang::allocation_diagnostics::{snapshot, CountingAllocator, Snapshot};
use decl_lang::session::{BindSource, Mode, Op, Session};
use serde_json::json;
use std::io::Write;
use std::path::Path;

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy, Default)]
struct Stamp {
    wall_ns: u64,
    cpu_ns: u64,
    sampled_until_ns: u64,
    allocation: Snapshot,
}
fn clock_ns(id: libc::clockid_t) -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid clock and initialized writable timespec; nothing escapes.
    assert_eq!(unsafe { libc::clock_gettime(id, &mut time) }, 0);
    assert!(time.tv_sec >= 0 && time.tv_nsec >= 0);
    (time.tv_sec as u64)
        .checked_mul(1_000_000_000)
        .and_then(|s| s.checked_add(time.tv_nsec as u64))
        .unwrap()
}
fn stamp() -> Stamp {
    let wall_ns = clock_ns(libc::CLOCK_MONOTONIC);
    let cpu_ns = clock_ns(libc::CLOCK_PROCESS_CPUTIME_ID);
    let sampled_until_ns = clock_ns(libc::CLOCK_MONOTONIC);
    Stamp {
        wall_ns,
        cpu_ns,
        sampled_until_ns,
        allocation: snapshot(),
    }
}
fn seconds(a: Stamp, b: Stamp) -> f64 {
    b.wall_ns.checked_sub(a.wall_ns).unwrap() as f64 / 1e9
}
fn write_new(path: &Path, data: &[u8]) {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap()
        .write_all(data)
        .unwrap();
}
const PHASES: [&str; 8] = [
    "setup",
    "full_run",
    "engine_lookup",
    "serialize",
    "scalar_capture",
    "output_write",
    "request_owner_release",
    "session_owner_release",
];

fn main() {
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}
fn run() {
    assert_eq!(std::env::var("DECL_QENGINE").as_deref(), Ok("1"));
    assert_eq!(std::env::var("DECL_QENGINE_STRICT").as_deref(), Ok("1"));
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(
        args.len(),
        5,
        "usage: session-initial MODEL INPUT_ROOT=INPUT_FILE OUTPUT_ROOT OUTPUT_PREFIX"
    );
    let model = Path::new(&args[1]).canonicalize().unwrap();
    // The command line's own spelling of a bound input: `--input name=file`.
    let (input_root, input_path) = args[2]
        .split_once('=')
        .expect("the input is written INPUT_ROOT=INPUT_FILE");
    let input = Path::new(input_path).canonicalize().unwrap();
    let output_root = args[3].as_str();
    let output_file = format!("{}-{output_root}.json", args[4]);
    let metrics_file = format!("{}-initial.json", args[4]);
    assert!(!Path::new(&output_file).exists() && !Path::new(&metrics_file).exists());
    let mut stamps = [Stamp::default(); 9];
    stamps[0] = stamp();
    let mut session = Session::new(Some(model.to_str().unwrap()));
    session
        .apply(Op::Bind {
            name: input_root.into(),
            src: BindSource::Inline {
                text: std::fs::read_to_string(&input).unwrap(),
            },
        })
        .unwrap();
    stamps[1] = stamp();
    let run = session.run(Mode::Full);
    stamps[2] = stamp();
    let eng = run.eng.as_ref().expect("Session did not return an engine");
    stamps[3] = stamp();
    let output = eng.serialize(
        &eng.env
            .root(output_root)
            .expect("the model declares no such output root"),
        output_root,
        false,
    );
    stamps[4] = stamp();

    // Plain copies only. The Edits owner survives the write as in the old probe.
    let edits = eng.edits.borrow().as_ref().unwrap().clone();
    let rev = &edits.revisions;
    let counters = [
        rev.revision.get(),
        rev.verified_cutoffs.get(),
        rev.value_cutoffs.get(),
        rev.recomputed.get(),
        edits.slot_computes.get(),
        edits.prepared_queries.get(),
        edits.retained_records.get(),
    ];
    let diagnostics = [
        run.load_diags.len(),
        run.checks.len(),
        run.session_checks.len(),
        run.diags.len(),
    ];
    let post_serialize_diags = eng.env.diag_len();
    let timing = run.timing;
    let output_bytes = output.len();
    let module_count = run.modules.len();
    stamps[5] = stamp();
    write_new(Path::new(&output_file), output.as_bytes());
    stamps[6] = stamp();
    // These are the ordinary end-of-request owners, followed by Session itself.
    // No manual cycle collection, early owner release, or graph census is added.
    drop(edits);
    drop(output);
    drop(run);
    stamps[7] = stamp();
    drop(session);
    stamps[8] = stamp();

    // Only scalar diagnostic metadata escapes the ordinary runtime lifetime.
    let gc = decl_lang::semantics::take_gc_diagnostics();
    let evaluation = decl_lang::evaluation_diagnostics::take_evaluation_diagnostics();
    assert_eq!(diagnostics, [0; 4], "initial Session diagnostics");
    assert_eq!(post_serialize_diags, 0, "serialization diagnostics");
    let setup_s = seconds(stamps[0], stamps[1]);
    let full_s = seconds(stamps[1], stamps[2]);
    let serialize_s = seconds(stamps[3], stamps[4]);
    let request = json!({
        "operation": "initial", "index": 0, "value": null,
        "setup_s": setup_s, "apply_s": 0.0,
        "evaluate_validate_s": full_s, "serialize_s": serialize_s,
        "total_s": full_s + serialize_s,
        "measurement_boundary": "apply + Session.run(Full) + serialize the output root; setup, counters, diagnostics inspection, output writes, and teardown excluded",
        "internal_timing_s": timing.total / 1000.0,
        "internal_recomputed": timing.recomputed, "internal_slots": timing.slots,
        "revision_total": counters[0],
        "revision_delta": {"revision": counters[0], "verified_cutoffs": counters[1],
            "value_cutoffs": counters[2], "recomputed": counters[3]},
        "revision_counters_total": {"verified_cutoffs": counters[1],
            "value_cutoffs": counters[2], "recomputed": counters[3]},
        "prepared_queries_latest_edit": counters[5], "prepared_queries_this_request": 0,
        "prepared_counter_semantics": "latest-edit gauge; this_request is zero when the revision did not advance",
        "slot_computes_total": counters[4], "slot_computes_delta": counters[4],
        "retained_records_latest_edit": counters[6],
        "diagnostics": {"load": diagnostics[0], "check": diagnostics[1],
            "session_check": diagnostics[2], "evaluation": diagnostics[3]},
        "output_bytes": output_bytes,
    });
    let boundaries: Vec<_> = stamps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "after": if i == 0 { "start" } else { PHASES[i - 1] },
                "wall_ns": s.wall_ns, "cpu_ns": s.cpu_ns,
                "sampled_until_ns": s.sampled_until_ns, "allocation": s.allocation,
            })
        })
        .collect();
    let row = json!({
        "schema": 1, "role": "initial-session-support", "primary_timing": false,
        "build_manifest_dir": env!("CARGO_MANIFEST_DIR"), "model": model, "input": input,
        "input_root": input_root, "output_root": output_root,
        "output_file": output_file, "output_bytes": output_bytes,
        "module_count": module_count, "request": request,
        "diagnostic_counts": diagnostics, "post_serialize_diags": post_serialize_diags,
        "run_timing_ms": {"load": timing.load, "check": timing.check,
            "bind": timing.bind, "evaluate": timing.evaluate, "total": timing.total},
        "phase_order": PHASES, "boundaries": boundaries,
        "gc": gc, "evaluation": evaluation,
        "wall_clock": "CLOCK_MONOTONIC; first sample defines boundary",
        "cpu_clock": "CLOCK_PROCESS_CPUTIME_ID; process user+system across threads",
        "ownership": "Normal request Edits/output/Run release, then Session release; no explicit collection; scalar metrics emitted afterwards",
        "limits": "Instrumented initial request only; Run.timing.bind is closure/setup, actual binding is inside evaluation round spans; not native paired timing",
    });
    let text = format!("{}\n", serde_json::to_string(&row).unwrap());
    write_new(Path::new(&metrics_file), text.as_bytes());
    print!("{text}");
}
