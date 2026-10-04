pub struct Programs;

impl Programs {
    pub const OK: &'static str = r#"
    (module
      (import "ech" "get" (func $get (param i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 64) "\00\00")
      (func (export "ech_alloc") (param i32) (result i32) (i32.const 1024))
      (func (export "ech_call") (param i32 i32) (result i64) (i64.const 0x0000004000000002))
    )
    "#;

    pub const TRAP: &'static str = r#"
    (module
      (import "ech" "get" (func $get (param i32 i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "ech_alloc") (param i32) (result i32) (i32.const 1024))
      (func (export "ech_call") (param i32 i32) (result i64) (unreachable))
    )
    "#;

    pub const ABORT: &'static str = r#"
    (module
      (import "ech" "abort" (func $abort (param i32 i32)))
      (memory (export "memory") 1)
      (data (i32.const 64) "boom")
      (func (export "ech_alloc") (param i32) (result i32) (i32.const 1024))
      (func (export "ech_call") (param i32 i32) (result i64)
        (call $abort (i32.const 64) (i32.const 4))
        (unreachable)
      )
    )
    "#;

    pub const FUEL_BURNER: &'static str = r#"
    (module
      (import "ech" "get" (func $get (param i32 i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "ech_alloc") (param i32) (result i32) (i32.const 1024))
      (func (export "ech_call") (param i32 i32) (result i64)
        (loop $spin (br $spin))
        (i64.const 0)
      )
    )
    "#;

    pub const MEMORY_GROWER: &'static str = r#"
    (module
      (import "ech" "get" (func $get (param i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 64) "\00\00")
      (func (export "ech_alloc") (param i32) (result i32) (i32.const 1024))
      (func (export "ech_call") (param i32 i32) (result i64)
        (drop (memory.grow (i32.const 2000)))
        (i64.const 0x0000004000000002)
      )
    )
    "#;

    pub fn write_then_trap(set_payload: &[u8]) -> String {
        let data = escape(set_payload);
        let len = set_payload.len();
        format!(
            r#"
    (module
      (import "ech" "set" (func $set (param i32 i32)))
      (memory (export "memory") 1)
      (data (i32.const 64) "{data}")
      (func (export "ech_alloc") (param i32) (result i32) (i32.const 2048))
      (func (export "ech_call") (param i32 i32) (result i64)
        (call $set (i32.const 64) (i32.const {len}))
        (unreachable)
      )
    )
    "#
        )
    }

    pub fn write_then_abort(set_payload: &[u8]) -> String {
        let data = escape(set_payload);
        let len = set_payload.len();
        format!(
            r#"
    (module
      (import "ech" "set" (func $set (param i32 i32)))
      (import "ech" "abort" (func $abort (param i32 i32)))
      (memory (export "memory") 1)
      (data (i32.const 64) "{data}")
      (func (export "ech_alloc") (param i32) (result i32) (i32.const 2048))
      (func (export "ech_call") (param i32 i32) (result i64)
        (call $set (i32.const 64) (i32.const {len}))
        (call $abort (i32.const 64) (i32.const 1))
        (unreachable)
      )
    )
    "#
        )
    }

    pub fn write_then_ok(set_payload: &[u8]) -> String {
        let data = escape(set_payload);
        let len = set_payload.len();
        format!(
            r#"
    (module
      (import "ech" "set" (func $set (param i32 i32)))
      (memory (export "memory") 1)
      (data (i32.const 64) "{data}")
      (data (i32.const 96) "\00\00")
      (func (export "ech_alloc") (param i32) (result i32) (i32.const 2048))
      (func (export "ech_call") (param i32 i32) (result i64)
        (call $set (i32.const 64) (i32.const {len}))
        (i64.const 0x0000006000000002)
      )
    )
    "#
        )
    }
}

fn escape(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("\\{byte:02x}"))
        .collect()
}
