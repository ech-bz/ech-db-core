use std::path::PathBuf;

use wasmi::Module;

use ech_db_server::abi::Abi;
use ech_db_server::wasm::Profile;

const BASE: &str = r#"
(module
  (import "ech" "get" (func (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (table 1 funcref)
  (func (export "ech_alloc") (param i32) (result i32) (local.get 0))
  (func (export "ech_call") (param i32 i32) (result i64) (i64.const 0))
)
"#;

fn module(wat: &str) -> Result<Module, wasmi::Error> {
    Module::new(&Profile::engine(), wat)
}

fn counter_wasm_path() -> PathBuf {
    std::env::var("ECH_TEST_COUNTER_WASM")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/wasm32-unknown-unknown/release/counter_program.wasm")
        })
}

#[test]
fn counter_program_matches_profile() {
    let path = counter_wasm_path();
    let wasm = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "read {}: {error} (build it with: cargo build -p counter-program --release --target wasm32-unknown-unknown)",
            path.display()
        )
    });
    let module = Module::new(&Profile::engine(), &wasm).unwrap();
    Abi::check(&module).unwrap();
    Abi::check_profile_shape(&wasm).unwrap();
}

#[test]
fn base_module_is_valid() {
    let wasm = wat::parse_str(BASE).unwrap();
    let module = module(BASE).unwrap();
    Abi::check(&module).unwrap();
    Abi::check_profile_shape(&wasm).unwrap();
}

#[test]
fn foreign_import_is_rejected() {
    let wat = BASE.replace("\"ech\" \"get\"", "\"env\" \"get\"");
    let module = module(&wat).unwrap();
    assert!(Abi::check(&module).is_err());
}

#[test]
fn unknown_import_is_rejected() {
    let wat = BASE.replace("\"ech\" \"get\"", "\"ech\" \"peek\"");
    let module = module(&wat).unwrap();
    assert!(Abi::check(&module).is_err());
}

#[test]
fn wrong_import_signature_is_rejected() {
    let wat = BASE.replace(
        "(import \"ech\" \"get\" (func (param i32 i32) (result i32)))",
        "(import \"ech\" \"get\" (func (param i32) (result i32)))",
    );
    let module = module(&wat).unwrap();
    assert!(Abi::check(&module).is_err());
}

#[test]
fn missing_export_is_rejected() {
    let wat = BASE.replace(
        "(func (export \"ech_call\") (param i32 i32) (result i64) (i64.const 0))",
        "",
    );
    let module = module(&wat).unwrap();
    assert!(Abi::check(&module).is_err());
}

#[test]
fn floats_are_allowed() {
    let wat = r#"
    (module
      (func (result f32) (f32.const 1))
    )
    "#;
    assert!(module(wat).is_ok());
}

#[test]
fn float_globals_are_allowed() {
    let wat = r#"
    (module
      (global (mut f64) (f64.const 0))
    )
    "#;
    assert!(module(wat).is_ok());
}

#[test]
fn saturating_float_conversions_are_rejected() {
    let wat = r#"
    (module
      (func (param f32) (result i32) (i32.trunc_sat_f32_s (local.get 0)))
    )
    "#;
    assert!(module(wat).is_err());
}

#[test]
fn start_function_is_rejected() {
    let wat = r#"
    (module
      (func $start)
      (start $start)
    )
    "#;
    assert!(module(wat).is_err());
}

#[test]
fn tail_calls_are_rejected() {
    let wat = r#"
    (module
      (func $callee)
      (func (return_call $callee))
    )
    "#;
    assert!(module(wat).is_err());
}

#[test]
fn multi_memory_is_rejected() {
    let wat = r#"
    (module
      (memory 1)
      (memory 1)
    )
    "#;
    assert!(module(wat).is_err());
}

#[test]
fn oversized_memory_is_rejected() {
    let wat = r#"
    (module
      (memory 2000)
    )
    "#;
    let wasm = wat::parse_str(wat).unwrap();
    let module = module(wat).unwrap();
    assert!(Abi::check_profile_shape(&wasm).is_err());
    let _ = module;
}

#[test]
fn oversized_table_budget_is_rejected() {
    let wat = r#"
    (module
      (table 1 65536 funcref)
      (table 1 65536 funcref)
    )
    "#;
    let wasm = wat::parse_str(wat).unwrap();
    let module = module(wat).unwrap();
    assert!(Abi::check_profile_shape(&wasm).is_err());
    let _ = module;
}

#[test]
fn bounded_small_tables_are_accepted() {
    let wat = r#"
    (module
      (table 10 100 funcref)
      (table 10 100 funcref)
    )
    "#;
    let wasm = wat::parse_str(wat).unwrap();
    let module = module(wat).unwrap();
    Abi::check_profile_shape(&wasm).unwrap();
    let _ = module;
}

#[test]
fn memory64_is_rejected() {
    let wat = r#"
    (module
      (memory i64 1)
    )
    "#;
    assert!(module(wat).is_err());
}
