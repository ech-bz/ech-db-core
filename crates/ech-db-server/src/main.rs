use std::path::PathBuf;

use clap::Parser;

use ech_db_server::config::Config;
use ech_db_server::fdb::Fdb;
use ech_db_server::server::Server;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    let _network = unsafe { foundationdb::boot() };
    let fdb = Fdb::open(&config.fdb)?;
    let server = Server::new(config, fdb)?;
    server.run().await?;
    Ok(())
}
