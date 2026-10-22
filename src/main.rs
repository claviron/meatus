use std::{path::PathBuf, sync::Arc};

use clap::Parser;

#[derive(Parser)]
#[command(about = "Run the Meatus VPN service")]
struct Args {
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = toml::from_str(&std::fs::read_to_string(args.config)?)?;
    let service = Arc::new(meatus::MeatusService::new(config)?);
    claviron::Server::new()
        .name("meatus")
        .workers(0)
        .register(move |state| {
            state.registry_ref().register(service).map(|_| ())
        })
        .serve()
        .await?;
    Ok(())
}
