mod config;
mod cost;
mod formatter;
mod llm;
mod materials;
mod server;
mod wechat;
mod writer;

use clap::Parser;
use config::ModelConfig;

#[derive(Parser)]
#[command(about = "公众号写作排版 Agent")]
struct Cli {
    #[arg(long, default_value = "3000")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = ModelConfig::load("config.toml")?;
    server::run_server(cfg, cli.port).await?;
    Ok(())
}
