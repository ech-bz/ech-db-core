use wasmi::{ExternType, Module, ValType};
use wasmparser::{Parser, Payload, TypeRef};

use ech_db_protocol::errors::DbError;

use crate::wasm::Profile;

pub struct Abi;

impl Abi {
    pub const MODULE: &'static str = "ech";

    pub fn check(module: &Module) -> Result<(), DbError> {
        for import in module.imports() {
            if import.module() != Self::MODULE {
                return Err(DbError::abi());
            }
            let ExternType::Func(func) = import.ty() else {
                return Err(DbError::abi());
            };
            let results = match import.name() {
                "get" | "get_range" | "response_copy" => 1,
                "set" | "del" | "abort" => 0,
                _ => return Err(DbError::abi()),
            };
            if func.params().len() != 2 || func.results().len() != results {
                return Err(DbError::abi());
            }
            if !func.params().iter().all(|param| *param == ValType::I32) {
                return Err(DbError::abi());
            }
            if results == 1 && func.results()[0] != ValType::I32 {
                return Err(DbError::abi());
            }
        }
        match module.get_export("memory") {
            Some(ExternType::Memory(_)) => {}
            _ => return Err(DbError::abi()),
        }
        Self::check_export(module, "ech_alloc", 1, ValType::I32)?;
        Self::check_export(module, "ech_call", 2, ValType::I64)?;
        Ok(())
    }

    fn check_export(
        module: &Module,
        name: &str,
        params: usize,
        result: ValType,
    ) -> Result<(), DbError> {
        match module.get_export(name) {
            Some(ExternType::Func(func))
                if func.params().len() == params
                    && func.params().iter().all(|param| *param == ValType::I32)
                    && func.results().len() == 1
                    && func.results()[0] == result =>
            {
                Ok(())
            }
            _ => Err(DbError::abi()),
        }
    }

    pub fn check_profile_shape(wasm: &[u8]) -> Result<(), DbError> {
        let mut memories: u64 = 0;
        let mut table_potential: u64 = 0;
        for payload in Parser::new(0).parse_all(wasm) {
            let payload = payload.map_err(|_| DbError::invalid_wasm())?;
            match payload {
                Payload::MemorySection(reader) => {
                    for memory in reader {
                        let memory = memory.map_err(|_| DbError::invalid_wasm())?;
                        memories += 1;
                        if memory.initial > Profile::MEMORY_LIMIT_PAGES {
                            return Err(DbError::invalid_wasm());
                        }
                    }
                }
                Payload::TableSection(reader) => {
                    for table in reader {
                        let table = table.map_err(|_| DbError::invalid_wasm())?;
                        let potential = table
                            .ty
                            .maximum
                            .unwrap_or(Profile::TABLE_LIMIT as u64)
                            .min(Profile::TABLE_LIMIT as u64)
                            .max(table.ty.initial);
                        table_potential = table_potential.saturating_add(potential);
                    }
                }
                Payload::ImportSection(reader) => {
                    for import in reader {
                        let import = import.map_err(|_| DbError::invalid_wasm())?;
                        if !matches!(import.ty, TypeRef::Func(_)) {
                            return Err(DbError::abi());
                        }
                    }
                }
                _ => {}
            }
        }
        if memories > 1 {
            return Err(DbError::invalid_wasm());
        }
        if table_potential > Profile::TABLE_LIMIT as u64 {
            return Err(DbError::invalid_wasm());
        }
        Ok(())
    }
}
