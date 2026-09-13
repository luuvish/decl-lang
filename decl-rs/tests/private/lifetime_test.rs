//! Rust-only collector correctness checks, included by semantics::lifetime.
//! The final observer test characterizes baseline Drop timing separately from
//! the preceding liveness/reentrancy requirements. It is not a language rule.
use super::*;
use crate::engine::Inst;

fn array() -> Rc<RefCell<ArrV>> {
    Rc::new(RefCell::new(ArrV {
        items: vec![],
        path: Rc::new(PrefixPath::default()),
    }))
}

fn collect_graph(graph: Graph) -> usize {
    #[cfg(feature = "runtime-diagnostics")]
    {
        let mut pass = diagnostics::Pass::new(0);
        let count = graph.collect(&mut pass);
        assert_eq!(
            pass.work.traced_nodes,
            pass.work.node_kinds.iter().sum::<usize>()
        );
        assert_eq!(pass.work.live_nodes + count, pass.work.traced_nodes);
        assert_eq!(pass.work.queue_pushes, pass.work.queue_pops);
        assert_eq!(
            pass.work.queue_pops,
            pass.work.live_nodes + pass.work.duplicate_queue_pops
        );
        count
    }
    #[cfg(not(feature = "runtime-diagnostics"))]
    graph.collect()
}

fn seed_arrays(values: &[Rc<RefCell<ArrV>>]) -> Graph {
    let mut graph = Graph::default();
    for value in values {
        graph.add(Node::Arr(value.clone()));
    }
    graph
}

fn check_graph(edges: &[&[usize]], roots: &[usize], expected_live: &[bool]) {
    assert_eq!(edges.len(), expected_live.len());
    let arrays: Vec<_> = edges.iter().map(|_| array()).collect();
    for (i, row) in edges.iter().enumerate() {
        arrays[i].borrow_mut().items = row.iter().map(|&j| Value::Arr(arrays[j].clone())).collect();
    }
    let weak: Vec<_> = arrays.iter().map(Rc::downgrade).collect();
    let held: Vec<_> = roots.iter().map(|&i| arrays[i].clone()).collect();
    let graph = seed_arrays(&arrays);
    drop(arrays);
    assert_eq!(
        collect_graph(graph),
        expected_live.iter().filter(|&&live| !live).count()
    );
    assert_eq!(
        weak.iter()
            .map(|w| w.upgrade().is_some())
            .collect::<Vec<_>>(),
        expected_live.to_vec()
    );
    drop(held);
    // Arrays are not tracked seeds themselves. Re-seed remaining actual owners,
    // then release those temporary seed handles before collecting their cycles.
    let surviving: Vec<_> = weak.iter().filter_map(Weak::upgrade).collect();
    let graph = seed_arrays(&surviving);
    drop(surviving);
    collect_graph(graph);
    assert!(weak.iter().all(|w| w.upgrade().is_none()));
}

#[test]
fn empty_graph_and_duplicate_node_seed() {
    assert_eq!(collect_graph(Graph::default()), 0);
    let value = array();
    value.borrow_mut().items.push(Value::Arr(value.clone()));
    let weak = Rc::downgrade(&value);
    let mut graph = Graph::default();
    assert_eq!(graph.add(Node::Arr(value.clone())), 0);
    let strong = Rc::strong_count(&value);
    assert_eq!(graph.add(Node::Arr(value.clone())), 0);
    assert_eq!(
        Rc::strong_count(&value),
        strong,
        "duplicate graph insertion retained an extra owner"
    );
    assert_eq!(graph.nodes.len(), 1);
    drop(value);
    assert_eq!(collect_graph(graph), 1);
    assert!(weak.upgrade().is_none());
}

#[test]
fn diamonds_cycles_and_duplicate_edge_multiplicity() {
    // The duplicate 1->3 edge must contribute twice to incoming counts. On the
    // second collection, every node in the rooted cycle becomes unreachable.
    check_graph(
        &[&[1, 2], &[3, 3], &[3], &[0], &[4]],
        &[0],
        &[true, true, true, true, false],
    );
    check_graph(&[&[1], &[2], &[2], &[]], &[0], &[true, true, true, false]);
    check_graph(&[&[1], &[0], &[2]], &[], &[false, false, false]);
    // Duplicate external root handles do not cause a second graph node.
    check_graph(&[&[1], &[0], &[]], &[0, 0], &[true, true, false]);
}

#[test]
fn high_fanin_keeps_all_reachable_nodes_then_reclaims_cycle() {
    check_graph(
        &[&[1, 2, 3, 4], &[5, 5], &[5, 5], &[5, 5], &[5, 5], &[0]],
        &[0],
        &[true; 6],
    );
}

#[test]
fn active_array_borrow_cannot_clear_hidden_children() {
    let parent = array();
    let child = array();
    parent.borrow_mut().items.push(Value::Arr(child.clone()));
    child.borrow_mut().items.push(Value::Arr(parent.clone()));
    let weak_parent = Rc::downgrade(&parent);
    let weak_child = Rc::downgrade(&child);
    let graph = seed_arrays(&[parent.clone(), child.clone()]);
    let borrowed = parent.borrow_mut();
    drop(child);
    assert_eq!(collect_graph(graph), 0);
    assert_eq!(borrowed.items.len(), 1);
    assert!(weak_child.upgrade().is_some());
    drop(borrowed);
    let graph = seed_arrays(std::slice::from_ref(&parent));
    drop(parent);
    assert_eq!(collect_graph(graph), 2);
    assert!(weak_parent.upgrade().is_none() && weak_child.upgrade().is_none());
}

#[test]
fn partial_environment_trace_keeps_early_and_unseen_children_alive() {
    let env = Env::new();
    let early = ty(RTk::Union(RefCell::new(vec![])));
    let RTk::Union(arms) = &early.k else {
        unreachable!()
    };
    arms.borrow_mut().push(early.clone());
    env.type_memo
        .borrow_mut()
        .insert("cycle".into(), early.clone());
    let weak_early = Rc::downgrade(&early);
    drop(early);
    let child = array();
    child.borrow_mut().items.push(Value::Arr(child.clone()));
    env.roots
        .borrow()
        .borrow_mut()
        .push(("child".into(), Value::Arr(child.clone())));
    let weak_child = Rc::downgrade(&child);
    drop(child);
    let weak_env = Rc::downgrade(&env);
    // Keep this Env discoverable until collection: arrays themselves are not
    // tracked seeds, whereas the Env <-> registry record cycle is tracked.
    let anchor = record();
    anchor.borrow_mut().menv = Some(env.clone());
    env.registry_push(anchor.clone());
    drop(anchor);
    // Env.trace has already visited type_memo when this later borrow fails;
    // the roots container and child have not yet been enumerated from this Env.
    let borrowed = env.registry.borrow_mut();
    collect_cycles();
    assert!(weak_early.upgrade().is_some() && weak_child.upgrade().is_some());
    assert_eq!(borrowed.borrow().len(), 1);
    drop(borrowed);
    drop(env);
    collect_cycles();
    assert!(weak_env.upgrade().is_none());
    assert!(weak_early.upgrade().is_none() && weak_child.upgrade().is_none());
}

fn record() -> Inst {
    record_instance(RecInst {
        type_name: None,
        rt: ty(RTk::Prim("int".into())),
        path: Rc::new(vec![]),
        ps: RefCell::new(None),
        parent: None,
        slots: vec![],
        entry_order: vec![],
        extras: vec![],
        menv: None,
    })
}

fn descriptor<T: From<Compute>>(compute: Compute) -> T {
    T::from(compute)
}

fn install_bridge(record: &Inst, callback: Rc<dyn Fn() -> R<Value>>) {
    record.borrow_mut().slots.push((
        "bridge".into(),
        Slot {
            kind: MKind::Der,
            hidden: true,
            state: SlotState::Unforced,
            value: Value::Undef,
            // The generic conversion supports both the baseline owned descriptor and the
            // separately proposed Rc<Compute> representation without another test.
            compute: Some(descriptor(Compute::Bridge(callback))),
        },
    ));
}

#[test]
fn externally_held_descriptor_keeps_its_raw_record_usable() {
    collect_cycles();
    let holder = record();
    let raw = record();
    holder.borrow_mut().parent = Some(holder.clone());
    raw.borrow_mut().parent = Some(raw.clone());
    raw.borrow_mut()
        .extras
        .push(("sentinel".into(), Value::Int(Num::Small(7))));
    let weak_holder = Rc::downgrade(&holder);
    let weak_raw = Rc::downgrade(&raw);
    holder.borrow_mut().slots.push((
        "value".into(),
        Slot {
            kind: MKind::Req,
            hidden: false,
            state: SlotState::Unforced,
            value: Value::Undef,
            compute: Some(descriptor(Compute::Check {
                raw: Value::Rec(raw.clone()),
                types: Rc::new(vec![]),
                name: "value".into(),
                root_name: "holder".into(),
                menv: None,
            })),
        },
    ));
    let preserved = holder.borrow().slots[0].1.compute.as_ref().unwrap().clone();
    drop(holder);
    drop(raw);
    collect_cycles();
    assert!(weak_holder.upgrade().is_none());
    // Borrow<Compute> supports both owned Compute and Rc<Compute>. A caller's
    // retained descriptor is a real external owner; its live raw value cannot
    // be cleared merely because its original slot was collected.
    let Compute::Check {
        raw: Value::Rec(value),
        ..
    } = std::borrow::Borrow::<Compute>::borrow(&preserved)
    else {
        panic!("preserved descriptor changed kind")
    };
    assert_eq!(value.borrow().extras.len(), 1);
    assert!(matches!(
        value.borrow().extra("sentinel"),
        Some(Value::Int(Num::Small(7)))
    ));
    assert!(weak_raw.upgrade().is_some());
    drop(preserved);
    collect_cycles();
    assert!(weak_raw.upgrade().is_none());
}

#[test]
fn opaque_owner_release_requires_followup_pass_without_callback_execution() {
    collect_cycles();
    #[cfg(feature = "runtime-diagnostics")]
    let _ = take_gc_diagnostics();
    let first = record();
    let second = record();
    first.borrow_mut().parent = Some(first.clone());
    second.borrow_mut().parent = Some(second.clone());
    let weak_first = Rc::downgrade(&first);
    let weak_second = Rc::downgrade(&second);
    let invoked = Rc::new(Cell::new(0));
    let invoked_capture = invoked.clone();
    let hidden_owner = second.clone();
    install_bridge(
        &first,
        Rc::new(move || {
            let _ = &hidden_owner;
            invoked_capture.set(invoked_capture.get() + 1);
            Ok(Value::Null)
        }),
    );
    drop(first);
    drop(second);
    assert!(collect_cycles() >= 2);
    assert_eq!(invoked.get(), 0, "collector invoked an opaque callback");
    assert!(weak_first.upgrade().is_none() && weak_second.upgrade().is_none());
    #[cfg(feature = "runtime-diagnostics")]
    {
        let report = take_gc_diagnostics();
        let call = report.calls.last().unwrap();
        assert!(
            call.passes.len() >= 3,
            "opaque second owner was lost before first root classification"
        );
        assert_eq!(call.passes.last().unwrap().work.visited_garbage_nodes, 0);
    }
}

#[derive(Debug, PartialEq)]
struct DropObservation {
    label: &'static str,
    own_alive: bool,
    own_borrow_active: bool,
    peer_alive: bool,
    collecting: bool,
    reentrant_result: usize,
}
struct Witness {
    label: &'static str,
    own: Weak<RefCell<RecInst>>,
    peer: Weak<RefCell<RecInst>>,
    events: Rc<RefCell<Vec<DropObservation>>>,
}
impl Drop for Witness {
    fn drop(&mut self) {
        let own = self.own.upgrade();
        let observation = DropObservation {
            label: self.label,
            own_alive: own.is_some(),
            own_borrow_active: own.as_ref().is_some_and(|r| r.try_borrow().is_err()),
            peer_alive: self.peer.upgrade().is_some(),
            collecting: COLLECTING.try_with(Cell::get).unwrap_or(false),
            reentrant_result: collect_cycles(),
        };
        self.events.borrow_mut().push(observation);
    }
}

#[test]
fn baseline_bridge_drop_characterization_and_reentrant_suppression() {
    collect_cycles();
    let a = record();
    let b = record();
    a.borrow_mut().parent = Some(a.clone());
    b.borrow_mut().parent = Some(b.clone());
    let wa = Rc::downgrade(&a);
    let wb = Rc::downgrade(&b);
    let events = Rc::new(RefCell::new(vec![]));
    for (label, own, peer) in [("a", &a, &b), ("b", &b, &a)] {
        let witness = Witness {
            label,
            own: Rc::downgrade(own),
            peer: Rc::downgrade(peer),
            events: events.clone(),
        };
        install_bridge(
            own,
            Rc::new(move || {
                let _ = &witness;
                panic!("opaque Bridge must never execute during collection")
            }),
        );
    }
    drop(a);
    drop(b);
    assert!(collect_cycles() >= 2);
    assert!(wa.upgrade().is_none() && wb.upgrade().is_none());
    assert!(!COLLECTING.with(Cell::get));
    // This exact ordering characterizes the baseline's externally observable
    // Rust Drop boundary, not a promise that already-unreachable nodes are live.
    // A representation proposal changing it needs an explicit compatibility
    // decision; it must not be mislabeled memory unsafety merely for differing.
    assert_eq!(
        *events.borrow(),
        vec![
            DropObservation {
                label: "a",
                own_alive: true,
                own_borrow_active: true,
                peer_alive: true,
                collecting: true,
                reentrant_result: 0
            },
            DropObservation {
                label: "b",
                own_alive: true,
                own_borrow_active: true,
                peer_alive: true,
                collecting: true,
                reentrant_result: 0
            },
        ]
    );
    assert_eq!(
        collect_cycles(),
        0,
        "reentrant collection left the thread guard stuck"
    );
}
