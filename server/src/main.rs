#![feature(try_blocks, iter_intersperse)]
#![feature(let_chains)]

use crate::config::Config;
use anyhow::Context;
use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tracing_subscriber::fmt::writer::MakeWriterExt;

mod base64_sestring;
mod config;
mod ffxiv;
mod listing;
mod listing_container;
mod sestring_ext;
mod stats;
mod template;
mod web;
mod ws;

mod api;
mod fflogs;
mod mongo;
mod player;
#[cfg(test)]
mod test;

#[tokio::main]
async fn main() {
    // 로깅 초기화: 콘솔 + 일별 로테이션 파일
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("server")
        .filename_suffix("log")
        .build("logs")
        .expect("initializing rolling file appender failed");

    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into())
        )
        .with_writer(std::io::stderr.and(non_blocking))
        .with_ansi(true)
        .init();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = if args.is_empty() {
        Cow::from("./config.toml")
    } else {
        Cow::from(args.remove(0))
    };

    let config = match get_config(&*config_path).await {
        Ok(config) => config,
        Err(e) => {
            tracing::error!("Failed to load config: {}", e);
            return;
        }
    };

    if let Err(e) = self::web::start(Arc::new(config)).await {
        tracing::error!("Server error: {}", e);
        tracing::error!("  {:?}", e);
    }
}

async fn get_config<P: AsRef<Path>>(path: P) -> anyhow::Result<Config> {
    let mut f = File::open(path)
        .await
        .context("could not open config file")?;
    let mut toml = String::new();
    f.read_to_string(&mut toml)
        .await
        .context("could not read config file")?;
    let config = toml::from_str(&toml).context("could not parse config file")?;

    Ok(config)
}
