use std::path::PathBuf;

use clap::Parser;
use serde::{Deserialize, Serialize};

use ech_db_client::{Call, Client, ProgramBundle, ReadKey};

#[derive(Serialize, Deserialize)]
struct Counter {
    value: u64,
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    endpoint: String,
    #[arg(long)]
    key: String,
    #[arg(long)]
    wasm: PathBuf,
    #[arg(long)]
    events: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let key: [u8; 32] = hex::decode(args.key.trim_start_matches("0x"))?
        .as_slice()
        .try_into()?;
    let client = Client::new(key).with_endpoint(args.endpoint);
    let bundle = ProgramBundle {
        format_version: 1,
        execution_profile: 1,
        wasm: std::fs::read(&args.wasm)?,
    };
    let program_hash = client.upload_program(bundle).await?;
    println!("program: 0x{}", hex::encode(program_hash));
    for index in 0..args.events {
        let call = Call::new(program_hash, "apply").with_args(&1u64)?;
        let intent = client.sign_intent(index + 1, [0u8; 32], &call)?;
        let output: Counter = client.append(&intent, Vec::new()).await?;
        println!("event {} -> counter {}", index + 1, output.value);
    }
    let values: Vec<Option<Counter>> = client
        .read(|session| async move {
            session
                .get::<Counter>(&[ReadKey::state(b"counter".to_vec())])
                .await
        })
        .await?;
    let counter = values
        .into_iter()
        .next()
        .flatten()
        .ok_or("counter missing")?;
    println!("final counter: {}", counter.value);
    Ok(())
}
