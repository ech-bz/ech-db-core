use bcs::{from_bytes, to_bytes};
use wasmi::errors::ErrorKind;
use wasmi::{
    Caller, Engine, Linker, Memory, Module, Store, TrapCode, TypedResumableCall,
    TypedResumableCallHostTrap, Val,
};

use ech_db_protocol::errors::{DbError, DbErrorCode};
use ech_db_protocol::ids::StreamId;
use ech_db_protocol::keys::{LogKey, Range, ReadKey, StateKey, WindowKey};
use ech_db_protocol::values::{ArchiveRecord, AttestedIntent, CallOutput};
use ech_db_protocol::wire::{AppendResult, StateWrite};

use crate::fdb::Trx;
use crate::wasm::{ModuleCache, Profile};

const SUSPEND: i32 = 0x0ECDB;

pub enum ExecMode {
    Live { head_number: u64 },
    Recovery {
        progress_key: Vec<u8>,
        progress_value: Vec<u8>,
    },
}

pub struct Executor {
    modules: ModuleCache,
}

impl Executor {
    pub fn new(engine: Engine) -> Self {
        Self {
            modules: ModuleCache::new(engine),
        }
    }

    pub fn modules(&self) -> &ModuleCache {
        &self.modules
    }

    pub async fn execute_append(
        &self,
        trx: &Trx,
        root: &[u8; 32],
        stream_id: &StreamId,
        attested: &AttestedIntent,
        module: &Module,
        mode: &ExecMode,
    ) -> Result<AppendResult, DbError> {
        let counter_key = LogKey::seq(root, stream_id);
        let current: u64 = trx.get_bcs(&counter_key).await?.unwrap_or(0u64);
        let next = current
            .checked_add(1)
            .ok_or_else(|| DbError::never(DbErrorCode::Integrity))?;
        if next != attested.intent.body.expected_seq {
            return Err(DbError::sequence_mismatch(
                current,
                attested.intent.body.expected_seq,
            ));
        }
        trx.set_bcs(&counter_key, &next)?;
        trx.set_bcs(&LogKey::data(root, stream_id, next), attested)?;
        let result = Run::execute(trx, self.modules.engine(), module, attested.clone()).await?;
        match mode {
            ExecMode::Live { head_number } => {
                let archived = ArchiveRecord::Intent(attested.clone());
                let value = to_bytes(&archived).map_err(|_| DbError::never(DbErrorCode::Integrity))?;
                trx.set_versionstamped_key(&WindowKey::record(root, *head_number, 0), &value);
            }
            ExecMode::Recovery {
                progress_key,
                progress_value,
            } => {
                trx.set_raw(progress_key, progress_value);
            }
        }
        Ok(result)
    }
}

struct ExecState {
    root: [u8; 32],
    memory: Option<Memory>,
    in_alloc: bool,
    response: Option<Vec<u8>>,
    pending: Option<Pending>,
    fatal: Option<DbError>,
    writes: Vec<StateWrite>,
    limits: crate::wasm::ProfileLimiter,
}

impl ExecState {
    fn new(root: [u8; 32]) -> Self {
        Self {
            root,
            memory: None,
            in_alloc: false,
            response: None,
            pending: None,
            fatal: None,
            writes: Vec::new(),
            limits: Profile::limits(),
        }
    }
}

enum Pending {
    Get(Vec<ReadKey>),
    GetRange(Range),
    Set(Vec<u8>, Vec<u8>),
    Del(Vec<u8>),
    Abort(Vec<u8>),
}

struct Run;

impl Run {
    pub async fn execute(
        trx: &Trx,
        engine: &Engine,
        module: &Module,
        attested: AttestedIntent,
    ) -> Result<AppendResult, DbError> {
        let mut linker = <Linker<ExecState>>::new(engine);
        linker
            .func_wrap("ech", "get", Host::get)
            .map_err(|_| DbError::abi())?;
        linker
            .func_wrap("ech", "get_range", Host::get_range)
            .map_err(|_| DbError::abi())?;
        linker
            .func_wrap("ech", "response_copy", Host::response_copy)
            .map_err(|_| DbError::abi())?;
        linker
            .func_wrap("ech", "set", Host::set)
            .map_err(|_| DbError::abi())?;
        linker
            .func_wrap("ech", "del", Host::del)
            .map_err(|_| DbError::abi())?;
        linker
            .func_wrap("ech", "abort", Host::abort)
            .map_err(|_| DbError::abi())?;
        let mut store = Store::new(engine, ExecState::new(attested.intent.body.root));
        store.limiter(|state| &mut state.limits);
        store.set_fuel(Profile::FUEL).map_err(|_| DbError::vm_trap())?;
        let instance = linker
            .instantiate_and_start(&mut store, module)
            .map_err(|_| DbError::invalid_wasm())?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(DbError::abi)?;
        store.data_mut().memory = Some(memory);
        let alloc = instance
            .get_typed_func::<i32, i32>(&store, "ech_alloc")
            .map_err(|_| DbError::abi())?;
        let call = instance
            .get_typed_func::<(i32, i32), i64>(&store, "ech_call")
            .map_err(|_| DbError::abi())?;
        let input_bytes = to_bytes(&attested).map_err(|_| DbError::abi())?;
        let input_len = i32::try_from(input_bytes.len()).map_err(|_| DbError::resource_limit())?;
        store.data_mut().in_alloc = true;
        let ptr = match alloc.call(&mut store, input_len) {
            Ok(ptr) => ptr,
            Err(error) => return Err(Self::trap_or(&mut store, &error)),
        };
        store.data_mut().in_alloc = false;
        Self::write_input(&mut store, ptr, &input_bytes)?;
        let mut invocation = call
            .call_resumable(&mut store, (ptr, input_len))
            .map_err(|error| Self::trap_or(&mut store, &error))?;
        let output = loop {
            match invocation {
                TypedResumableCall::Finished(value) => break value,
                TypedResumableCall::OutOfFuel(_) => return Err(DbError::out_of_fuel()),
                TypedResumableCall::HostTrap(trap) => {
                    if let Some(fatal) = store.data_mut().fatal.take() {
                        return Err(fatal);
                    }
                    let pending = store.data_mut().pending.take().ok_or_else(DbError::abi)?;
                    invocation = Self::resume(trx, &mut store, trap, pending).await?;
                }
            }
        };
        let output = Self::read_output(&store, output)?;
        let writes = std::mem::take(&mut store.data_mut().writes);
        Ok(AppendResult { output, writes })
    }

    async fn resume(
        trx: &Trx,
        store: &mut Store<ExecState>,
        typed: TypedResumableCallHostTrap<i64>,
        pending: Pending,
    ) -> Result<TypedResumableCall<i64>, DbError> {
        let root = store.data().root;
        match pending {
            Pending::Get(keys) => {
                let response = Self::read_keys(trx, &root, &keys).await?;
                let bytes = to_bytes(&response).map_err(|_| DbError::never(DbErrorCode::Integrity))?;
                Self::charge_store(store, bytes.len() as u64)?;
                let len = i32::try_from(bytes.len()).map_err(|_| DbError::resource_limit())?;
                store.data_mut().response = Some(bytes);
                typed
                    .resume(&mut *store, &[Val::I32(len)])
                    .map_err(|error| Self::trap_or(store, &error))
            }
            Pending::GetRange(range) => {
                let response = Self::read_range(trx, &root, &range).await?;
                let bytes = to_bytes(&response).map_err(|_| DbError::never(DbErrorCode::Integrity))?;
                Self::charge_store(store, bytes.len() as u64)?;
                let len = i32::try_from(bytes.len()).map_err(|_| DbError::resource_limit())?;
                store.data_mut().response = Some(bytes);
                typed
                    .resume(&mut *store, &[Val::I32(len)])
                    .map_err(|error| Self::trap_or(store, &error))
            }
            Pending::Set(user_key, value) => {
                trx.set_raw(&StateKey::physical(&root, &user_key), &value);
                store.data_mut().writes.push(StateWrite {
                    key: user_key,
                    value,
                });
                typed
                    .resume(&mut *store, &[])
                    .map_err(|error| Self::trap_or(store, &error))
            }
            Pending::Del(user_key) => {
                trx.clear(&StateKey::physical(&root, &user_key));
                typed
                    .resume(&mut *store, &[])
                    .map_err(|error| Self::trap_or(store, &error))
            }
            Pending::Abort(bytes) => Err(DbError::application(bytes)),
        }
    }

    fn trap_or(store: &mut Store<ExecState>, error: &wasmi::Error) -> DbError {
        if let Some(fatal) = store.data_mut().fatal.take() {
            return fatal;
        }
        match error.kind() {
            ErrorKind::TrapCode(TrapCode::OutOfFuel) => DbError::out_of_fuel(),
            ErrorKind::TrapCode(TrapCode::GrowthOperationLimited) => DbError::resource_limit(),
            _ => DbError::vm_trap(),
        }
    }

    fn charge_store(store: &mut Store<ExecState>, fuel: u64) -> Result<(), DbError> {
        let remaining = store.get_fuel().map_err(|_| DbError::vm_trap())?;
        if remaining < fuel {
            return Err(DbError::out_of_fuel());
        }
        store
            .set_fuel(remaining - fuel)
            .map_err(|_| DbError::vm_trap())
    }

    fn write_input(store: &mut Store<ExecState>, ptr: i32, bytes: &[u8]) -> Result<(), DbError> {
        let memory = store.data().memory.ok_or_else(DbError::abi)?;
        let offset = ptr as u32 as usize;
        let end = offset.checked_add(bytes.len()).ok_or_else(DbError::abi)?;
        if end > memory.data_size(&*store) {
            return Err(DbError::abi());
        }
        memory.write(store, offset, bytes).map_err(|_| DbError::abi())
    }

    fn read_output(store: &Store<ExecState>, value: i64) -> Result<Vec<u8>, DbError> {
        let ptr = ((value as u64) >> 32) as u32 as usize;
        let len = ((value as u64) & 0xFFFF_FFFF) as u32 as usize;
        let memory = store.data().memory.ok_or_else(DbError::abi)?;
        let end = ptr.checked_add(len).ok_or_else(DbError::abi)?;
        if end > memory.data_size(store) {
            return Err(DbError::abi());
        }
        let mut buffer = vec![0u8; len];
        memory
            .read(store, ptr, &mut buffer)
            .map_err(|_| DbError::abi())?;
        match from_bytes::<CallOutput>(&buffer) {
            Ok(CallOutput::Ok(bytes)) => Ok(bytes),
            Ok(CallOutput::Err(bytes)) => Err(DbError::application(bytes)),
            Err(_) => Err(DbError::abi()),
        }
    }

    async fn read_keys(
        trx: &Trx,
        root: &[u8; 32],
        keys: &[ReadKey],
    ) -> Result<Vec<Option<Vec<u8>>>, DbError> {
        let mut values = Vec::with_capacity(keys.len());
        for key in keys {
            values.push(trx.get_raw(&key.physical(root)).await?);
        }
        Ok(values)
    }

    async fn read_range(
        trx: &Trx,
        root: &[u8; 32],
        range: &Range,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, DbError> {
        let (begin, end) = range.physical(root);
        let prefix_len = begin.len() - range.start.len();
        let rows = trx.range_raw(&begin, &end).await?;
        Ok(rows
            .into_iter()
            .map(|(key, value)| (key[prefix_len..].to_vec(), value))
            .collect())
    }
}

struct Host;

impl Host {
    fn get(mut caller: Caller<'_, ExecState>, ptr: i32, len: i32) -> Result<i32, wasmi::Error> {
        Self::request(&mut caller, ptr, len, |bytes| {
            from_bytes::<Vec<ReadKey>>(bytes)
                .map(Pending::Get)
                .map_err(|_| DbError::abi())
        })
    }

    fn get_range(mut caller: Caller<'_, ExecState>, ptr: i32, len: i32) -> Result<i32, wasmi::Error> {
        Self::request(&mut caller, ptr, len, |bytes| {
            from_bytes::<Range>(bytes)
                .map(Pending::GetRange)
                .map_err(|_| DbError::abi())
        })
    }

    fn set(mut caller: Caller<'_, ExecState>, ptr: i32, len: i32) -> Result<(), wasmi::Error> {
        Self::request(&mut caller, ptr, len, |bytes| {
            from_bytes::<(Vec<u8>, Vec<u8>)>(bytes)
                .map(|(key, value)| Pending::Set(key, value))
                .map_err(|_| DbError::abi())
        })
        .map(|_| ())
    }

    fn del(mut caller: Caller<'_, ExecState>, ptr: i32, len: i32) -> Result<(), wasmi::Error> {
        Self::request(&mut caller, ptr, len, |bytes| {
            from_bytes::<Vec<u8>>(bytes)
                .map(Pending::Del)
                .map_err(|_| DbError::abi())
        })
        .map(|_| ())
    }

    fn abort(mut caller: Caller<'_, ExecState>, ptr: i32, len: i32) -> Result<(), wasmi::Error> {
        Self::request(&mut caller, ptr, len, |bytes| Ok(Pending::Abort(bytes.to_vec()))).map(|_| ())
    }

    fn response_copy(
        mut caller: Caller<'_, ExecState>,
        ptr: i32,
        capacity: i32,
    ) -> Result<i32, wasmi::Error> {
        if caller.data().in_alloc {
            return Err(Self::fail(&mut caller, DbError::abi()));
        }
        let capacity = capacity as u32 as usize;
        let Some(response) = caller.data().response.clone() else {
            return Err(Self::fail(&mut caller, DbError::abi()));
        };
        if capacity < response.len() {
            return Err(Self::fail(&mut caller, DbError::abi()));
        }
        let cost = Profile::HOST_CALL_BASE_FUEL + 2 * response.len() as u64;
        if let Err(error) = Self::charge(&mut caller, cost) {
            return Err(Self::fail(&mut caller, error));
        }
        let memory = match caller.data().memory {
            Some(memory) => memory,
            None => return Err(Self::fail(&mut caller, DbError::abi())),
        };
        let offset = ptr as u32 as usize;
        let end = match offset.checked_add(response.len()) {
            Some(end) => end,
            None => return Err(Self::fail(&mut caller, DbError::abi())),
        };
        if end > memory.data_size(&caller) {
            return Err(Self::fail(&mut caller, DbError::abi()));
        }
        if memory.write(&mut caller, offset, &response).is_err() {
            return Err(Self::fail(&mut caller, DbError::abi()));
        }
        i32::try_from(response.len()).map_err(|_| Self::fail(&mut caller, DbError::abi()))
    }

    fn request(
        caller: &mut Caller<'_, ExecState>,
        ptr: i32,
        len: i32,
        parse: impl FnOnce(&[u8]) -> Result<Pending, DbError>,
    ) -> Result<i32, wasmi::Error> {
        if caller.data().in_alloc {
            return Err(Self::fail(caller, DbError::abi()));
        }
        let input_len = len as u32 as usize;
        let input = match Self::read(caller, ptr, len) {
            Ok(bytes) => bytes,
            Err(error) => return Err(Self::fail(caller, error)),
        };
        let cost = Profile::HOST_CALL_BASE_FUEL + input_len as u64;
        if let Err(error) = Self::charge(caller, cost) {
            return Err(Self::fail(caller, error));
        }
        let pending = match parse(&input) {
            Ok(pending) => pending,
            Err(error) => return Err(Self::fail(caller, error)),
        };
        caller.data_mut().pending = Some(pending);
        Err(wasmi::Error::i32_exit(SUSPEND))
    }

    fn read(caller: &Caller<'_, ExecState>, ptr: i32, len: i32) -> Result<Vec<u8>, DbError> {
        let memory = caller.data().memory.ok_or_else(DbError::abi)?;
        let offset = ptr as u32 as usize;
        let length = len as u32 as usize;
        let end = offset.checked_add(length).ok_or_else(DbError::abi)?;
        if end > memory.data_size(caller) {
            return Err(DbError::abi());
        }
        let mut buffer = vec![0u8; length];
        memory
            .read(caller, offset, &mut buffer)
            .map_err(|_| DbError::abi())?;
        Ok(buffer)
    }

    fn charge(caller: &mut Caller<'_, ExecState>, fuel: u64) -> Result<(), DbError> {
        let remaining = caller.get_fuel().map_err(|_| DbError::vm_trap())?;
        if remaining < fuel {
            return Err(DbError::out_of_fuel());
        }
        caller
            .set_fuel(remaining - fuel)
            .map_err(|_| DbError::vm_trap())
    }

    fn fail(caller: &mut Caller<'_, ExecState>, error: DbError) -> wasmi::Error {
        caller.data_mut().fatal = Some(error);
        wasmi::Error::i32_exit(SUSPEND)
    }
}
