pub fn alloc(len: i32) -> i32 {
    let len = len as u32 as usize;
    let mut buffer = Vec::<u8>::with_capacity(len);
    let ptr = buffer.as_mut_ptr();
    core::mem::forget(buffer);
    ptr as u32 as i32
}

#[cfg(target_arch = "wasm32")]
use crate::host;
#[cfg(target_arch = "wasm32")]
use crate::AttestedIntent;

#[cfg(target_arch = "wasm32")]
pub fn call<F>(ptr: i32, len: i32, handler: F) -> i64
where
    F: FnOnce(AttestedIntent) -> Result<Vec<u8>, Vec<u8>>,
{
    let input_bytes = unsafe {
        core::slice::from_raw_parts(ptr as u32 as usize as *const u8, len as u32 as usize)
    };
    let attested = host::decode_attested(input_bytes);
    let output = match handler(attested) {
        Ok(bytes) => crate::CallOutput::Ok(bytes),
        Err(bytes) => crate::CallOutput::Err(bytes),
    };
    let encoded = host::encode_call_output(&output);
    let len = encoded.len();
    let ptr = alloc(len as i32);
    unsafe {
        core::ptr::copy_nonoverlapping(encoded.as_ptr(), ptr as u32 as usize as *mut u8, len);
    }
    (((ptr as u32 as u64) << 32) | (len as u32 as u64)) as i64
}

#[macro_export]
macro_rules! export_program {
    ($handler:path) => {
        #[no_mangle]
        pub extern "C" fn ech_alloc(len: i32) -> i32 {
            $crate::entry::alloc(len)
        }

        #[no_mangle]
        pub extern "C" fn ech_call(ptr: i32, len: i32) -> i64 {
            $crate::entry::call(ptr, len, $handler)
        }
    };
}
