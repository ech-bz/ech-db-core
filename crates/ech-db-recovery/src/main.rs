use std::path::PathBuf;

use clap::Parser;

use ech_db_recovery::recovery::Recovery;
use ech_db_server::config::Config;
use ech_db_server::exec::Executor;
use ech_db_server::wasm::Profile;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    root: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let root_bytes = hex::decode(args.root.trim_start_matches("0x"))?;
    let root: [u8; 32] = root_bytes.as_slice().try_into()?;
    let config = Config::load(&args.config)?;
    let _network = unsafe { foundationdb::boot() };
    let executor = Executor::new(Profile::engine());
    let recovery = Recovery::new(&config, executor)?;
    recovery.run(root).await?;
    tracing::info!("recovery completed");
    Ok(())
}
