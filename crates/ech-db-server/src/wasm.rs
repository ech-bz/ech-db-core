use std::collections::HashMap;
use std::sync::Mutex;

use wasmi::{Config, CustomFuelCosts, Engine, Module, OperatorCost};

pub struct Profile;

impl Profile {
    pub const ID: u32 = 1;
    pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
    pub const MEMORY_LIMIT_PAGES: u64 = 1024;
    pub const TABLE_LIMIT: usize = 65_536;
    pub const VALUE_STACK_LIMIT: usize = 1024 * 1024;
    pub const CALL_DEPTH: usize = 1024;
    pub const FUEL: u64 = 10_000_000;
    pub const HOST_CALL_BASE_FUEL: u64 = 100;

    pub fn engine() -> Engine {
        let mut config = Config::default();
        config.consume_fuel(true);
        config.operator_cost(OperatorCost::default());
        config.fuel_cost(CustomFuelCosts {
            bytes_copied_per_fuel: 64,
            fuel_per_bytes_translated: 7,
            fuel_per_bytes_validated: 2,
        });
        config.set_max_stack_height(Self::VALUE_STACK_LIMIT);
        config.set_max_recursion_depth(Self::CALL_DEPTH);
        config.floats(true);
        config.wasm_saturating_float_to_int(false);
        config.wasm_mutable_global(true);
        config.wasm_sign_extension(true);
        config.wasm_multi_value(true);
        config.wasm_bulk_memory(true);
        config.wasm_reference_types(true);
        config.wasm_multi_memory(false);
        config.wasm_memory64(false);
        config.wasm_tail_call(false);
        config.wasm_extended_const(false);
        config.wasm_custom_page_sizes(false);
        config.wasm_wide_arithmetic(false);
        config.allow_start_fn(false);
        Engine::new(&config)
    }

    pub fn limits() -> ProfileLimiter {
        ProfileLimiter
    }
}

pub struct ProfileLimiter;

impl wasmi::ResourceLimiter for ProfileLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, wasmi_core::LimiterError> {
        if desired > Profile::MEMORY_LIMIT {
            return Err(wasmi_core::LimiterError::ResourceLimiterDeniedAllocation);
        }
        Ok(maximum.map_or(true, |max| desired <= max))
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, wasmi_core::LimiterError> {
        if desired > Profile::TABLE_LIMIT {
            return Err(wasmi_core::LimiterError::ResourceLimiterDeniedAllocation);
        }
        Ok(maximum.map_or(true, |max| desired <= max))
    }

    fn instances(&self) -> usize {
        1
    }

    fn tables(&self) -> usize {
        10_000
    }

    fn memories(&self) -> usize {
        1
    }
}

pub struct ModuleCache {
    engine: Engine,
    modules: Mutex<HashMap<[u8; 32], Module>>,
}

impl ModuleCache {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            modules: Mutex::new(HashMap::new()),
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn insert(&self, hash: [u8; 32], module: Module) {
        let mut modules = self.modules.lock().unwrap_or_else(|poison| poison.into_inner());
        modules.insert(hash, module);
    }

    pub fn get(&self, hash: &[u8; 32]) -> Option<Module> {
        let modules = self.modules.lock().unwrap_or_else(|poison| poison.into_inner());
        modules.get(hash).cloned()
    }
}
