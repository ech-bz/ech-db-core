#![cfg(target_arch = "wasm32")]

use ech_db_guest::{export_program, AttestedIntent, Host, ReadKey, Stream};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Counter {
    value: u64,
}

const COUNTER_KEY: &[u8] = b"counter";

fn handle(attested: AttestedIntent) -> Result<Vec<u8>, Vec<u8>> {
    let intent = &attested.intent;
    match intent.body.call.function.as_str() {
        "apply" => {
            let delta: u64 =
                bcs::from_bytes(&intent.body.call.args).map_err(|_| b"bad args".to_vec())?;
            let current: Option<Counter> = Host::get(&[ReadKey::state(COUNTER_KEY.to_vec())])
                .into_iter()
                .next()
                .flatten();
            let value = current.map(|counter| counter.value).unwrap_or(0) + delta;
            let counter = Counter { value };
            Host::set_value(COUNTER_KEY, &counter);
            let stream_id = Stream::intent(intent.body.author).id();
            let stored: Option<AttestedIntent> =
                Host::get(&[ReadKey::log_data(&stream_id, intent.body.expected_seq)])
                    .into_iter()
                    .next()
                    .flatten();
            if stored.as_ref() != Some(&attested) {
                return Err(b"intent not visible".to_vec());
            }
            bcs::to_bytes(&counter).map_err(|_| b"encode".to_vec())
        }
        _ => Err(b"unknown function".to_vec()),
    }
}

export_program!(handle);
