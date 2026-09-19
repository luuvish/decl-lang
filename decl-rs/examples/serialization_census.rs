//! Census of what serializing one Session root emits. Supporting diagnostics, never timing.
//!
//! The first row follows the initial full run, the second a repeated run with
//! no change, and each further row follows one `Update` edit of `EDIT_PATH` to
//! the next `VALUE`. Placement is read under the allocator the CLI uses.
use decl_lang::serialization_diagnostics::census;
use decl_lang::session::{BindSource, EditKind, Mode, Op, Session};
use serde_json::json;
use std::io::Write;
use std::path::Path;

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str =
    "usage: serialization-census MODEL INPUT_NAME INPUT_FILE ROOT OUTPUT_FILE [EDIT_PATH VALUE...]";

fn main() {
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    let args: Vec<String> = std::env::args().collect();
    assert!(args.len() == 6 || args.len() >= 8, "{USAGE}");
    let model = Path::new(&args[1]).canonicalize().unwrap();
    let input = Path::new(&args[3]).canonicalize().unwrap();
    let (input_name, root, output_file) = (&args[2], &args[4], Path::new(&args[5]));
    assert!(!output_file.exists(), "refusing to overwrite the output");
    let edit_path = args.get(6);
    let mut steps: Vec<(&str, Option<&String>)> = vec![("initial", None), ("repeated", None)];
    steps.extend(args.iter().skip(7).map(|value| ("update", Some(value))));

    let mut session = Session::new(Some(model.to_str().unwrap()));
    session
        .apply(Op::Bind {
            name: input_name.clone(),
            src: BindSource::Inline {
                text: std::fs::read_to_string(&input).unwrap(),
            },
        })
        .unwrap();
    let mut rows = Vec::with_capacity(steps.len());
    for (index, (operation, value)) in steps.into_iter().enumerate() {
        if let Some(value) = value {
            session
                .apply(Op::Edit {
                    kind: EditKind::Update,
                    path: edit_path.expect("an edit value without a path").clone(),
                    expr: Some(value.clone()),
                })
                .unwrap();
        }
        let run = session.run(Mode::Full);
        let eng = run.eng.as_ref().expect("Session did not return an engine");
        let value_root = eng.env.root(root).expect("the root was not evaluated");
        // The census follows the emission it describes and shares no work with it.
        let output_bytes = eng.serialize(&value_root, root, false).len();
        let counted = census(&value_root, false);
        rows.push(json!({
            "operation": operation, "index": index, "value": value, "output_bytes": output_bytes,
            "diagnostics": {
                "load": run.load_diags.len(), "checks": run.checks.len(),
                "session_checks": run.session_checks.len(), "evaluation": run.diags.len(),
            },
            "census": counted,
        }));
    }
    let report = json!({
        "schema": 1, "kind": "serialization-census", "timing_claim": false,
        "allocator": "MiMalloc", "root": root, "input_name": input_name, "edit_path": edit_path,
        "query_engine_environment": {
            "DECL_QENGINE": std::env::var("DECL_QENGINE").ok(),
            "DECL_QENGINE_STRICT": std::env::var("DECL_QENGINE_STRICT").ok(),
        },
        "rows": rows,
    });
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_file)
        .unwrap();
    file.write_all(&serde_json::to_vec_pretty(&report).unwrap())
        .unwrap();
    file.write_all(b"\n").unwrap();
}
