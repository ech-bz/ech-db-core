#[test_only]
module ech_anchor::registry_tests;

use ech_anchor::registry::{Self, PublisherCap, Registry};
use sui::test_scenario;

fun zeros(): vector<u8> {
    let mut out = vector[];
    let mut i = 0u64;
    while (i < 32) {
        out.push_back(0u8);
        i = i + 1
    };
    out
}

fun filled(value: u8): vector<u8> {
    let mut out = vector[];
    let mut i = 0u64;
    while (i < 32) {
        out.push_back(value);
        i = i + 1
    };
    out
}

fun view(registry: &Registry, root: vector<u8>): (vector<u8>, vector<u8>) {
    registry::entry(registry, root)
}

#[test]
fun anchor_insert_noop_and_advance() {
    let mut scenario = test_scenario::begin(@0xA);
    scenario.next_tx(@0xA);
    registry::init_for_testing(scenario.ctx());
    scenario.next_tx(@0xB);
    let cap = scenario.take_from_address<PublisherCap>(@0xA);
    let mut registry = scenario.take_shared<Registry>();
    let root = filled(0x11);
    registry::anchor(&mut registry, &cap, root, zeros(), filled(0x01));
    let (prev, cur) = view(&registry, root);
    assert!(prev == zeros() && cur == filled(0x01));
    registry::anchor(&mut registry, &cap, root, zeros(), filled(0x01));
    let (prev, cur) = view(&registry, root);
    assert!(prev == zeros() && cur == filled(0x01));
    registry::anchor(&mut registry, &cap, root, filled(0x01), filled(0x02));
    let (prev, cur) = view(&registry, root);
    assert!(prev == filled(0x01) && cur == filled(0x02));
    test_scenario::return_shared(registry);
    test_scenario::return_to_address(@0xA, cap);
    scenario.end();
}

#[test]
fun anchor_roots_are_independent() {
    let mut scenario = test_scenario::begin(@0xA);
    scenario.next_tx(@0xA);
    registry::init_for_testing(scenario.ctx());
    scenario.next_tx(@0xB);
    let cap = scenario.take_from_address<PublisherCap>(@0xA);
    let mut registry = scenario.take_shared<Registry>();
    registry::anchor(&mut registry, &cap, filled(0x01), zeros(), filled(0x0A));
    registry::anchor(&mut registry, &cap, filled(0x02), zeros(), filled(0x0B));
    let (prev, cur) = view(&registry, filled(0x01));
    assert!(prev == zeros() && cur == filled(0x0A));
    let (prev, cur) = view(&registry, filled(0x02));
    assert!(prev == zeros() && cur == filled(0x0B));
    test_scenario::return_shared(registry);
    test_scenario::return_to_address(@0xA, cap);
    scenario.end();
}

#[test]
#[expected_failure(abort_code = 1)]
fun anchor_conflict_is_aborted() {
    let mut scenario = test_scenario::begin(@0xA);
    scenario.next_tx(@0xA);
    registry::init_for_testing(scenario.ctx());
    scenario.next_tx(@0xB);
    let cap = scenario.take_from_address<PublisherCap>(@0xA);
    let mut registry = scenario.take_shared<Registry>();
    let root = filled(0x11);
    registry::anchor(&mut registry, &cap, root, zeros(), filled(0x01));
    registry::anchor(&mut registry, &cap, root, filled(0x05), filled(0x06));
    test_scenario::return_shared(registry);
    test_scenario::return_to_address(@0xA, cap);
    scenario.end();
}

#[test]
#[expected_failure(abort_code = 1)]
fun nonzero_prev_on_missing_root_is_aborted() {
    let mut scenario = test_scenario::begin(@0xA);
    scenario.next_tx(@0xA);
    registry::init_for_testing(scenario.ctx());
    scenario.next_tx(@0xB);
    let cap = scenario.take_from_address<PublisherCap>(@0xA);
    let mut registry = scenario.take_shared<Registry>();
    registry::anchor(&mut registry, &cap, filled(0x11), filled(0x01), filled(0x02));
    test_scenario::return_shared(registry);
    test_scenario::return_to_address(@0xA, cap);
    scenario.end();
}

#[test]
#[expected_failure(abort_code = 0)]
fun invalid_length_is_aborted() {
    let mut scenario = test_scenario::begin(@0xA);
    scenario.next_tx(@0xA);
    registry::init_for_testing(scenario.ctx());
    scenario.next_tx(@0xB);
    let cap = scenario.take_from_address<PublisherCap>(@0xA);
    let mut registry = scenario.take_shared<Registry>();
    let mut short = vector[];
    short.push_back(1u8);
    registry::anchor(&mut registry, &cap, short, zeros(), filled(0x02));
    test_scenario::return_shared(registry);
    test_scenario::return_to_address(@0xA, cap);
    scenario.end();
}
