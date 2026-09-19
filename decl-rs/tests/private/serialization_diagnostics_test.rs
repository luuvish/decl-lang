//! Rust-only census of emitted values: serializer mirror and text placement.
use super::*;
use crate::parse::parse_source;
use crate::pipeline::run_pipeline;
use crate::semantics::{ArrV, MapV, QuantityValue};
use std::cell::RefCell;

fn array(items: Vec<Value>) -> Value {
    Value::Arr(Rc::new(RefCell::new(ArrV {
        items,
        path: Default::default(),
    })))
}

fn map(entries: Vec<(&str, Value)>) -> Value {
    let entries = entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    Value::Map(Rc::new(RefCell::new(MapV {
        entries,
        path: Default::default(),
    })))
}

fn buckets(distances: &Distances) -> u64 {
    distances.up_to_64
        + distances.up_to_4_kib
        + distances.up_to_16_kib
        + distances.up_to_2_mib
        + distances.beyond_2_mib
}

#[test]
fn distances_fall_in_exactly_one_bucket_and_saturate() {
    let mut distances = Distances::default();
    let edges = [
        (0, CACHE_LINE_BYTES),
        (CACHE_LINE_BYTES + 1, 0),
        (0, SMALL_PAGE_BYTES),
        (SMALL_PAGE_BYTES + 1, 0),
        (0, LARGE_PAGE_BYTES),
        (LARGE_PAGE_BYTES + 1, 0),
        (0, SEGMENT_BYTES),
        (SEGMENT_BYTES + 1, 0),
        (7, 7),
    ];
    for (a, b) in edges {
        distances.record(a, b);
    }
    assert_eq!(
        (
            distances.up_to_64,
            distances.up_to_4_kib,
            distances.up_to_16_kib,
            distances.up_to_2_mib,
            distances.beyond_2_mib
        ),
        (2, 2, 2, 2, 1)
    );
    assert_eq!(distances.pairs, edges.len() as u64);
    assert_eq!(buckets(&distances), distances.pairs);
    let expected: usize = edges.iter().map(|(a, b)| a.abs_diff(*b)).sum();
    assert_eq!(distances.total_bytes, expected as u64);

    distances.record(0, usize::MAX);
    distances.record(usize::MAX, 0);
    assert_eq!(distances.total_bytes, u64::MAX);
    assert_eq!(distances.beyond_2_mib, 3);
}

#[test]
fn text_census_separates_descriptors_from_character_allocations() {
    let shared = Value::Str("shared".into());
    let characters: Rc<str> = Rc::from("imported");
    let value = array(vec![
        shared.clone(),
        shared.clone(),
        Value::Str(SharedText::from_rc(characters.clone())),
        Value::Str(SharedText::from_rc(characters.clone())),
        Value::Str("".into()),
        Value::Int(1_i64.into()),
    ]);
    let census = census(&value, false);
    assert_eq!(census.values.text, 5);
    assert_eq!(census.values.integers, 1);
    assert_eq!(census.values.arrays, 1);
    assert_eq!(census.max_depth, 1);
    let text = &census.text;
    assert_eq!(text.values, 5);
    assert_eq!(text.bytes, 6 + 6 + 8 + 8);
    // One shared descriptor, two imports of one character allocation, one empty text.
    assert_eq!(text.distinct_descriptors, 4);
    assert_eq!(text.distinct_characters, 3);
    assert_eq!(text.descriptor_to_characters.pairs, 5);
    assert_eq!(text.consecutive_descriptors.pairs, 4);
    assert_eq!(text.consecutive_characters.pairs, 4);
    for distances in [
        &text.descriptor_to_characters,
        &text.consecutive_descriptors,
        &text.consecutive_characters,
    ] {
        assert_eq!(buckets(distances), distances.pairs);
    }
    // The repeated shared Value is at distance zero in both records; the two
    // imports share characters, so only their character distance is zero.
    assert!(text.consecutive_descriptors.up_to_64 >= 1);
    assert!(text.consecutive_characters.up_to_64 >= 2);
    // Counting neither retains nor releases an owner.
    assert_eq!(Rc::strong_count(&characters), 3);
    let Value::Str(owner) = &shared else {
        unreachable!()
    };
    assert_eq!(Rc::strong_count(owner.as_rc()), 1);
}

#[test]
fn omitted_children_write_no_key_and_raw_positions_become_null() {
    let quantity = Value::Q(Box::new(QuantityValue {
        dim: Rc::new("Time".to_string()),
        value: 0.25,
    }));
    let typed = map(vec![
        ("kept", Value::Int(1_i64.into())),
        ("gone", Value::Absent),
        ("text", Value::Str("x".into())),
        ("wait", quantity),
        ("none", Value::Null),
        ("flag", Value::Bool(true)),
        ("real", Value::Float(1.5)),
    ]);
    let census_typed = census(&typed, false);
    assert_eq!(census_typed.values.maps, 1);
    assert_eq!(census_typed.values.omitted, 1);
    assert_eq!(census_typed.values.raw_null_substitutions, 0);
    assert_eq!(census_typed.keys.count, 6);
    assert_eq!(census_typed.keys.bytes, 4 * 6);
    assert_eq!(
        (
            census_typed.values.integers,
            census_typed.values.text,
            census_typed.values.quantities,
            census_typed.values.nulls,
            census_typed.values.booleans,
            census_typed.values.floats
        ),
        (1, 1, 1, 1, 1, 1)
    );

    let raw = Value::JObj(Rc::new(vec![
        ("a".to_string(), Value::Str("x".into())),
        ("b".to_string(), Value::Absent),
        (
            "c".to_string(),
            Value::JArr(Rc::new(vec![Value::Null, Value::Undef])),
        ),
    ]));
    let census_raw = census(&raw, false);
    assert_eq!(census_raw.values.raw_objects, 1);
    assert_eq!(census_raw.values.raw_arrays, 1);
    // Raw documents keep every position: three keys, two substituted nulls.
    assert_eq!(census_raw.keys.count, 3);
    assert_eq!(census_raw.values.omitted, 2);
    assert_eq!(census_raw.values.raw_null_substitutions, 2);
    assert_eq!(census_raw.values.nulls, 1);
    assert_eq!(census_raw.text.values, 1);
    assert_eq!(census_raw.max_depth, 2);

    assert_eq!(census(&Value::Absent, false).values.omitted, 1);
    assert_eq!(census(&Value::Int(3_i64.into()), false).max_depth, 0);
}

#[derive(Debug, Default, PartialEq)]
struct Emitted {
    objects: u64,
    arrays: u64,
    keys: u64,
    key_bytes: u64,
    strings: u64,
    string_bytes: u64,
    numbers: u64,
    booleans: u64,
    nulls: u64,
}

fn emitted(value: &serde_json::Value, counts: &mut Emitted) {
    match value {
        serde_json::Value::Null => counts.nulls += 1,
        serde_json::Value::Bool(_) => counts.booleans += 1,
        serde_json::Value::Number(_) => counts.numbers += 1,
        serde_json::Value::String(text) => {
            counts.strings += 1;
            counts.string_bytes += text.len() as u64;
        }
        serde_json::Value::Array(items) => {
            counts.arrays += 1;
            for item in items {
                emitted(item, counts);
            }
        }
        serde_json::Value::Object(entries) => {
            counts.objects += 1;
            for (key, item) in entries {
                counts.keys += 1;
                counts.key_bytes += key.len() as u64;
                emitted(item, counts);
            }
        }
    }
}

const MODULE: &str = r#"
type Svc = {
    name: string
    port?: int = 8080
    note?: string
    secret$ = `s-${name}`
    endpoint = `${name}:${port}`
    ratio?: float = 0.5
    live?: bool = true
}
type Link = { source: ref<Svc>, target: ref<Svc> }
type Plain = {
    services: Svc[]
    tags: { [string]: string }
}
type Graph = {
    services: Svc[]
    links: Link[]
}
export output plain: Plain = {
    services: [{ name: "a" }, { name: "b", port: 1, note: "n" }]
    tags: { "x": "1", "y": "22" }
}
export output graph: Graph = {
    services: [{ name: "a" }, { name: "b" }]
    links: [{ source: services[0], target: services[1] }]
}
"#;

#[test]
fn census_mirrors_what_the_serializer_emits() {
    let pipeline = run_pipeline(&parse_source(MODULE).decls);
    assert_eq!(
        pipeline.diags.len(),
        0,
        "the fixture module must evaluate cleanly"
    );
    for settable_only in [false, true] {
        for root in ["plain", "graph"] {
            let value = pipeline.env.root(root).expect("evaluated root");
            let text = pipeline.eng.serialize(&value, root, settable_only);
            let mut json = Emitted::default();
            emitted(
                &serde_json::from_str(&text).expect("canonical JSON"),
                &mut json,
            );
            let census = census(&value, settable_only);
            let counts = &census.values;
            assert_eq!(
                json.objects,
                counts.maps + counts.records + counts.raw_objects + counts.quantities
            );
            assert_eq!(json.arrays, counts.arrays + counts.raw_arrays);
            assert_eq!(json.keys, census.keys.count + 2 * counts.quantities);
            assert_eq!(
                json.strings,
                counts.text + counts.references + counts.quantities
            );
            assert_eq!(
                json.numbers,
                counts.integers + counts.floats + counts.quantities
            );
            assert_eq!(json.booleans, counts.booleans);
            assert_eq!(json.nulls, counts.nulls + counts.raw_null_substitutions);
            assert_eq!(counts.text, census.text.values);
            assert_eq!(counts.unevaluated_raw_values, 0);
            if root == "plain" {
                // No reference or quantity text: every emitted byte of text and key is counted.
                assert_eq!(counts.references + counts.quantities, 0);
                assert_eq!(json.string_bytes, census.text.bytes);
                assert_eq!(json.key_bytes, census.keys.bytes);
                assert_eq!(counts.records, 3);
                assert_eq!(counts.maps, 1);
            } else {
                assert_eq!(counts.references, 2);
            }
            // Hidden members never appear; derived ones only outside the settable projection.
            assert!(!text.contains("secret"));
            assert_eq!(text.contains("endpoint"), !settable_only);
        }
    }
}
