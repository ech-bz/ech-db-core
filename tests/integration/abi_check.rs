fn main() {
    let path = "target/wasm32-unknown-unknown/release/board_program.wasm";
    let bytes = std::fs::read(path).unwrap();
    let engine = ech_db_server::wasm::Profile::engine();
    match wasmi::Module::new(&engine, &bytes) {
        Ok(module) => {
            println!("module ok");
            match ech_db_server::abi::Abi::check(&module) {
                Ok(()) => println!("abi ok"),
                Err(error) => println!("abi fail: {error}"),
            }
            match ech_db_server::abi::Abi::check_profile_shape(&bytes) {
                Ok(()) => println!("shape ok"),
                Err(error) => println!("shape fail: {error}"),
            }
        }
        Err(error) => println!("module fail: {error:?}"),
    }
}
