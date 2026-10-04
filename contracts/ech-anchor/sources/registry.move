module ech_anchor::registry;

use sui::object;
use sui::table::{Self, Table};
use sui::transfer;
use sui::tx_context::TxContext;

const E_INVALID_LENGTH: u64 = 0;
const E_ANCHOR_CONFLICT: u64 = 1;

public struct PublisherCap has key {
    id: UID,
}

public struct Anchor has store, copy, drop {
    prev_hash: vector<u8>,
    cur_hash: vector<u8>,
}

public struct Registry has key {
    id: UID,
    entries: Table<vector<u8>, Anchor>,
}

fun init(ctx: &mut TxContext) {
    let registry = Registry {
        id: object::new(ctx),
        entries: table::new(ctx),
    };
    transfer::share_object(registry);
    transfer::transfer(PublisherCap { id: object::new(ctx) }, ctx.sender());
}

public fun anchor(
    registry: &mut Registry,
    _cap: &PublisherCap,
    root: vector<u8>,
    prev_hash: vector<u8>,
    cur_hash: vector<u8>,
) {
    assert!(
        root.length() == 32 && prev_hash.length() == 32 && cur_hash.length() == 32,
        E_INVALID_LENGTH,
    );
    if (!registry.entries.contains(root)) {
        assert!(is_zero(&prev_hash), E_ANCHOR_CONFLICT);
        registry.entries.add(root, Anchor { prev_hash, cur_hash });
        return
    };
    let (stored_prev, stored_cur) = {
        let stored = registry.entries.borrow(root);
        (stored.prev_hash, stored.cur_hash)
    };
    if (stored_prev == prev_hash && stored_cur == cur_hash) {
        return
    };
    if (stored_cur == prev_hash) {
        let stored = registry.entries.borrow_mut(root);
        stored.prev_hash = prev_hash;
        stored.cur_hash = cur_hash;
        return
    };
    abort E_ANCHOR_CONFLICT
}

fun is_zero(value: &vector<u8>): bool {
    let mut i = 0;
    while (i < value.length()) {
        if (value[i] != 0) {
            return false
        };
        i = i + 1
    };
    true
}

#[test_only]
public fun init_for_testing(ctx: &mut TxContext) {
    init(ctx)
}

#[test_only]
public fun entry(registry: &Registry, root: vector<u8>): (vector<u8>, vector<u8>) {
    let stored = registry.entries.borrow(root);
    (stored.prev_hash, stored.cur_hash)
}
