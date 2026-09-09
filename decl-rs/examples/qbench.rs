//! A performance comparison of the two evaluators — the tree walker
//! (`run_pipeline`) and the query engine (`qevaluate`) — over generated
//! workloads at a few scales, timed in process. Run:
//! `cargo run --locked --release --example qbench`.
//!
//! Both paths load, evaluate, validate, and serialize identical outputs. This
//! measures batch evaluation, not incremental edits. Parsing and checking are
//! excluded. One warmup precedes nine samples; the median is reported.
use decl_lang::parse::parse_source;
use decl_lang::pipeline::run_pipeline;
use decl_lang::qengine::qeval::qevaluate;
use decl_lang::semantics::Env;
use std::hint::black_box;
use std::time::Instant;

// Match the allocator used by the command line being optimized.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

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

/// Many values share a wide, tagged schema. Union selection and record binding
/// should read that schema, not copy every member for every candidate lookup.
fn tagged(n: usize) -> String {
    let fields = (0..12)
        .map(|i| format!("field{i}: int = {i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let types = (0..8)
        .map(|i| format!("type Row{i} = {{ kind: \"tag{i}\", {fields} }}\n"))
        .collect::<String>();
    let union = (0..8)
        .map(|i| format!("Row{i}"))
        .collect::<Vec<_>>()
        .join(" | ");
    let rows = (0..n)
        .map(|i| format!("{{ kind: \"tag{}\" }}", i % 8))
        .collect::<Vec<_>>()
        .join(",\n");
    format!("{types}type Row = {union}\nexport output rows: Row[] = [{rows}]")
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

/// median of `k` runs of `f`, in milliseconds (including result destruction)
fn median_ms(k: usize, mut f: impl FnMut()) -> f64 {
    let mut times = Vec::with_capacity(k);
    for _ in 0..k {
        let t = Instant::now();
        f();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    times[k / 2]
}

fn bench(label: &str, src: &str, k: usize) {
    let parsed = parse_source(src);
    assert!(parsed.errors.is_empty(), "{label}: parse errors");
    let decls = parsed.decls.clone();
    // Both paths load, bind, evaluate, validate AND serialize every output.
    // Parsing and static checking are outside both timed regions.
    let tree = || {
        let p = run_pipeline(&decls);
        assert!(!p.diags.iter().any(|d| d.severity == "error"), "{label}");
        let outputs: Vec<(String, String)> = p
            .env
            .outputs
            .borrow()
            .iter()
            .map(|(n, _, _)| {
                let v = p.env.root(n).expect("output exists");
                (n.clone(), p.eng.serialize(&v, n, false))
            })
            .collect();
        outputs
    };
    let query = || {
        let env = Env::new();
        env.load(&decls);
        let r = qevaluate(env).unwrap_or_else(|u| panic!("{label}: unsupported {}", u.0));
        assert!(r.ok, "{label}: query evaluation failed");
        r.outputs
    };
    // Warm both paths and verify the measured workload before reporting a time.
    assert_eq!(tree(), query(), "{label}: evaluators disagree");
    let tw = median_ms(k, || {
        black_box(tree());
    });
    let qe = median_ms(k, || {
        black_box(query());
    });
    println!(
        "{label:28} tree-walker {tw:8.2} ms   query-engine {qe:8.2} ms   ratio {:.2}x",
        qe / tw
    );
}

fn main() {
    println!("== load + evaluate + validate + serialize: median of 9 runs ==");
    bench("flat records n=500", &flat(500), 9);
    bench("flat records n=2000", &flat(2000), 9);
    bench("tagged union n=500", &tagged(500), 9);
    bench("tagged union n=2000", &tagged(2000), 9);
    bench("$referrers ring n=100", &graph(100), 9);
    bench("$referrers ring n=400", &graph(400), 9);
    bench("$referrers ring n=1600", &graph(1600), 9);
}
