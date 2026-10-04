use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{CallOutput, Range, ReadKey};
use ech_db_protocol::values::AttestedIntent;

#[link(wasm_import_module = "ech")]
extern "C" {
    fn get(ptr: i32, len: i32) -> i32;
    fn get_range(ptr: i32, len: i32) -> i32;
    fn response_copy(ptr: i32, capacity: i32) -> i32;
    fn set(ptr: i32, len: i32);
    fn del(ptr: i32, len: i32);
    fn abort(ptr: i32, len: i32);
}

pub struct Host;

impl Host {
    pub fn get_raw(keys: &[ReadKey]) -> Vec<Option<Vec<u8>>> {
        let input = to_bcs_bytes(&keys.to_vec());
        let response_len = unsafe { get(input.as_ptr() as i32, input.len() as i32) };
        let response = copy_response(response_len as u32 as usize);
        from_bcs_bytes(&response)
    }

    pub fn get<T: DeserializeOwned>(keys: &[ReadKey]) -> Vec<Option<T>> {
        Self::get_raw(keys)
            .into_iter()
            .map(|value| value.map(|bytes| from_bcs_bytes(&bytes)))
            .collect()
    }

    pub fn get_range_raw(range: &Range) -> Vec<(Vec<u8>, Vec<u8>)> {
        let input = to_bcs_bytes(range);
        let response_len = unsafe { get_range(input.as_ptr() as i32, input.len() as i32) };
        let response = copy_response(response_len as u32 as usize);
        from_bcs_bytes(&response)
    }

    pub fn get_range<T: DeserializeOwned>(range: &Range) -> Vec<(Vec<u8>, T)> {
        Self::get_range_raw(range)
            .into_iter()
            .map(|(key, value)| (key, from_bcs_bytes(&value)))
            .collect()
    }

    pub fn set(user_key: &[u8], value: &[u8]) {
        let input = to_bcs_bytes(&(user_key.to_vec(), value.to_vec()));
        unsafe { set(input.as_ptr() as i32, input.len() as i32) };
    }

    pub fn set_value<T: Serialize>(user_key: &[u8], value: &T) {
        let value = to_bcs_bytes(value);
        Self::set(user_key, &value);
    }

    pub fn del(user_key: &[u8]) {
        let input = to_bcs_bytes(&user_key.to_vec());
        unsafe { del(input.as_ptr() as i32, input.len() as i32) };
    }

    pub fn abort(bytes: &[u8]) -> ! {
        unsafe { abort(bytes.as_ptr() as i32, bytes.len() as i32) };
        unreachable!("abort must not return")
    }
}

fn copy_response(response_len: usize) -> Vec<u8> {
    let ptr = crate::entry::alloc(response_len as i32);
    let copied = unsafe { response_copy(ptr, response_len as i32) };
    assert_eq!(copied as u32 as usize, response_len);
    let mut buffer = vec![0u8; response_len];
    unsafe {
        core::ptr::copy_nonoverlapping(
            ptr as u32 as usize as *const u8,
            buffer.as_mut_ptr(),
            response_len,
        );
    }
    buffer
}

fn to_bcs_bytes<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
    bcs::to_bytes(value).expect("bcs encode")
}

fn from_bcs_bytes<T: DeserializeOwned>(bytes: &[u8]) -> T {
    bcs::from_bytes(bytes).expect("bcs decode")
}

pub fn decode_attested(bytes: &[u8]) -> AttestedIntent {
    from_bcs_bytes(bytes)
}

pub fn encode_call_output(output: &CallOutput) -> Vec<u8> {
    to_bcs_bytes(output)
}
