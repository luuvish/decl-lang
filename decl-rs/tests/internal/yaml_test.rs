//! yaml: the YAML reader's core schema and refusals, the writer's
//! plain-string rule, and the round trip.
use decl_lang::semantics::{read_json, Value};
use decl_lang::yaml::{plain_safe, read_yaml, to_json, to_yaml};

fn refusal(src: &str) -> String {
    match read_yaml(src) {
        Ok(_) => String::new(),
        Err(e) => e.message(),
    }
}

#[test]
fn core_schema() {
    let v =
        read_yaml("a: 1\nb: 2.5\nc: yes\nd: 0x1F\ne: ~\nf: \"12\"\ng: [x, {h: true}]\n").unwrap();
    assert_eq!(
        to_json(&v, 0),
        "{\"a\":1,\"b\":2.5,\"c\":\"yes\",\"d\":31,\"e\":null,\"f\":\"12\",\"g\":[\"x\",{\"h\":true}]}"
    );
    let Value::JObj(entries) = &v else {
        panic!("an object")
    };
    assert!(matches!(entries[0].1, Value::Int(_)));
    assert!(matches!(entries[1].1, Value::Float(_)));
}

#[test]
fn refused() {
    assert_eq!(refusal("a: !!str 1\n"), "uses a tag at line 1");
    assert_eq!(refusal("1: x\n"), "mapping key is not a string at line 1");
    assert_eq!(
        refusal("a: 1\na: 2\n"),
        "mapping repeats the key \"a\" at line 2"
    );
    assert_eq!(
        refusal("a: 1\n---\nb: 2\n"),
        "stream holds more than one document at line 2"
    );
}

#[test]
fn plain_strings() {
    for s in ["my-service", "with space", "a_b"] {
        assert!(plain_safe(s), "{s} is plain");
    }
    for s in ["yes", "n", "true", "12", "1e3", "a: b", "-x", "", "x #y"] {
        assert!(!plain_safe(s), "{s} is quoted");
    }
}

#[test]
fn round_trip() {
    let doc = "{\"name\":\"s\",\"xs\":[{\"a\":1,\"b\":[]},2.0],\"m\":{},\"q\":{\"value\":3000.0,\"unit\":\"m\"}}";
    let raw = read_json(doc).ok().expect("JSON");
    let y = to_yaml(&raw, 2);
    assert_eq!(
        y,
        "name: s\nxs:\n  - a: 1\n    b: []\n  - 2.0\nm: {}\nq:\n  value: 3000.0\n  unit: m"
    );
    assert_eq!(to_json(&read_yaml(&y).unwrap(), 0), doc);
    assert_eq!(
        to_json(&read_json(&to_json(&raw, 2)).ok().expect("JSON"), 0),
        doc
    );
}
