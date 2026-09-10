//! Same workload as the TypeScript/Python drivers, via the public Session API.
use decl_lang::semantics::{read_json, Value};
use decl_lang::session::{BindSource, EditKind, Mode, Op, Session};
use std::collections::HashMap;
use std::time::Instant;

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn get<'a>(v: &'a Value, key: &str) -> &'a Value {
    let Value::JObj(es) = v else {
        panic!("object expected")
    };
    &es.iter().find(|(k, _)| k == key).unwrap().1
}
fn number(v: &Value) -> usize {
    let Value::Int(n) = v else {
        panic!("integer expected")
    };
    n.to_string().parse().unwrap()
}
fn main() {
    let file = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tests/benchmarks/session.json".into());
    let config = read_json(&std::fs::read_to_string(file).unwrap())
        .ok()
        .unwrap();
    let Value::Str(source) = get(&config, "source") else {
        panic!("source expected")
    };
    let Value::JArr(sizes) = get(&config, "sizes") else {
        panic!("sizes expected")
    };
    let (warmups, samples) = (
        number(get(&config, "warmups")),
        number(get(&config, "samples")),
    );
    let entry = std::env::current_dir()
        .unwrap()
        .join("tests/benchmarks/session_case.decl");
    let overlay = HashMap::from([(entry.clone(), source.to_string())]);
    let mut rows = vec![];
    for size in sizes.iter().map(number) {
        for equal in [true, false] {
            let mut session = Session::with_overlay(entry.to_str(), Some(&overlay));
            session
                .apply(Op::Bind {
                    name: "batch".into(),
                    src: BindSource::Inline {
                        text: format!("{{\"items\":[{}]}}", vec!["{\"n\":1}"; size].join(",")),
                    },
                })
                .unwrap();
            let first = session.run(Mode::Full);
            assert!(first.eng.is_some() && first.diags.is_empty());
            let mut times = vec![];
            for i in 0..warmups + samples {
                let n = if equal {
                    (2 + i % 2) as i32
                } else if i % 2 == 0 {
                    -1
                } else {
                    1
                };
                let start = Instant::now();
                session
                    .apply(Op::Edit {
                        kind: EditKind::Update,
                        path: "batch.items[0].n".into(),
                        expr: Some(n.to_string()),
                    })
                    .unwrap();
                let run = session.run(Mode::Full);
                let eng = run.eng.as_ref().unwrap();
                let value = eng.serialize(&eng.env.root("total").unwrap(), "total", false);
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                assert!(run.diags.is_empty());
                assert_eq!(value, (7 * (size - usize::from(n < 0))).to_string());
                if i >= warmups {
                    times.push(elapsed);
                }
            }
            let mut sorted = times.clone();
            sorted.sort_by(f64::total_cmp);
            rows.push(format!(
                "{{\"size\":{size},\"equal\":{equal},\"samples_ms\":{times:?},\"median_ms\":{}}}",
                sorted[samples / 2]
            ));
        }
    }
    println!("[{}]", rows.join(","));
}
