use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use tokio::sync::mpsc;
use ts_connection::ConnectSettings;

/// 命令行参数：仅配置文件路径。
#[derive(Parser, Debug)]
#[command(about = "TS3 musicbot")]
struct Args {
    /// TOML 配置文件路径
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let config = tsbot_config::load(&args.config)?;

    let identity = ts_connection::identity::load_or_create(Path::new(&config.bot.identity_path))?;
    let settings = ConnectSettings {
        address: config.server.address,
        password: config.server.password,
        channel: config.server.channel,
        name: config.bot.name,
        identity,
    };

    let (mut player, handle, snapshot) = player::Player::new()?;
    let store = playlist::Store::new(std::path::PathBuf::from(config.playlist.dir));
    let (chat_tx, chat_rx) = mpsc::channel(32);
    let (reply_tx, reply_rx) = mpsc::channel(32);

    tokio::spawn(commands::run(chat_rx, handle, snapshot, store, config.permissions, reply_tx));

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    ts_connection::run_persistent(settings, &mut player, chat_tx, reply_rx, shutdown).await
}
