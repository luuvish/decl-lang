//! A performance comparison of the two evaluators — the tree walker
//! (`run_pipeline`) and the query engine (`qevaluate`) — over generated
//! workloads at a few scales, timed in process (the query engine is meant to be
//! fastest in Rust). Run: `cargo run --release --example qbench`.
//!
//! This quantifies the query engine's per-evaluation overhead for one-shot
//! evaluation and is the tool that drives the incremental work: the win is on
//! re-evaluation after an edit, which a later mode of this bench will measure.
use decl_lang::parse::parse_source;
use decl_lang::pipeline::run_pipeline;
use decl_lang::qengine::qeval::qevaluate;
use decl_lang::semantics::Env;
use std::time::Instant;

/// a flat bag of records with derived members — non-`$referrers`, one round
fn flat(n: usize) -> String {
    let items: Vec<String> = (0..n)
        .map(|i| format!("{{ x: {i}, y: {} }}", i * 2))
        .collect();
    format!(
        "type Item = {{ x: int, y: int, sum = x + y, prod = x * y, \
         label = `item-${{x}}-${{y}}`, big = sum * prod + x - y }}\n\
         type Bag = {{ items: Item[] }}\n\
         export output bag: Bag = {{ items: [{}] }}",
        items.join(", ")
    )
}

/// a ring graph whose nodes count their `$referrers` — settles in a couple of
/// rounds (the fixpoint the query engine drives)
fn graph(n: usize) -> String {
    let nodes: Vec<String> = (0..n)
        .map(|i| format!("n{i}: {{ name: \"n{i}\" }}"))
        .collect();
    let edges: Vec<String> = (0..n)
        .map(|i| {
            format!(
                "{{ source: nodes[\"n{i}\"], target: nodes[\"n{}\"] }}",
                (i + 1) % n
            )
        })
        .collect();
    format!(
        "type Node = {{ name: string, incoming = $referrers(Edge, \"target\"), \
         fan_in = std.array.count(incoming) }}\n\
         type Edge = {{ source: ref<Node>, target: ref<Node> }}\n\
         type Graph = {{ nodes: {{ [/[a-z0-9]+/]: Node }}, edges: Edge[], \
         total = std.array.sum([m.fan_in for m in std.map.values(nodes)]) }}\n\
         export output g: Graph = {{ nodes: {{ {} }}, edges: [{}] }}",
        nodes.join(", "),
        edges.join(", ")
    )
}

/// best of `k` runs of `f`, in milliseconds
fn best_ms(k: usize, mut f: impl FnMut()) -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..k {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    best
}

fn bench(label: &str, src: &str, k: usize) {
    let parsed = parse_source(src);
    assert!(parsed.errors.is_empty(), "{label}: parse errors");
    let decls = parsed.decls.clone();
    // warm
    run_pipeline(&decls);
    {
        let env = Env::new();
        env.load(&decls);
        let _ = qevaluate(env);
    }
    let tw = best_ms(k, || {
        run_pipeline(&decls);
    });
    let qe = best_ms(k, || {
        let env = Env::new();
        env.load(&decls);
        let _ = qevaluate(env);
    });
    println!(
        "{label:28} tree-walker {tw:8.2} ms   query-engine {qe:8.2} ms   ratio {:.2}x",
        qe / tw
    );
}

fn main() {
    println!("== one-shot evaluation: tree walker vs query engine (best of runs) ==");
    bench("flat records n=500", &flat(500), 20);
    bench("flat records n=2000", &flat(2000), 10);
    bench("$referrers ring n=100", &graph(100), 20);
    bench("$referrers ring n=400", &graph(400), 10);
}
