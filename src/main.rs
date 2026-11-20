use anyhow::{anyhow, Result as AnyResult};
use dotenvy::dotenv;
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use teloxide::prelude::*;
use tokio::{
    sync::Mutex,
    time::{sleep, Duration as TokioDuration},
};

// Модульная структура
mod converter;
mod handlers;
mod limits;
mod media_workspace;
mod serverless;
mod telegram;

use handlers::process_video;
use limits::{utc_day_index, RateLimiter};

const DEFAULT_USER_DAILY_LIMIT: u32 = 10;
const DEFAULT_GLOBAL_DAILY_LIMIT: u32 = 50;
const DEFAULT_MAX_INPUT_BYTES: u32 = 100 * 1024 * 1024;
const DEFAULT_MAX_OUTPUT_BYTES: u32 = 49_000_000;
const DEFAULT_MAX_CONVERSIONS_PER_RUN: u32 = 1;
const DEFAULT_MAX_UPDATES_PER_RUN: u32 = 100;

struct LocalApiProcess {
    child: Child,
}

impl Drop for LocalApiProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Optionally run the official Local Bot API binary inside the same BotKeep
/// container. Leave LOCAL_BOT_API_BIN unset for ordinary cloud/Compose setups.
fn start_local_api() -> AnyResult<Option<LocalApiProcess>> {
    let Some(binary) = std::env::var_os("LOCAL_BOT_API_BIN") else {
        return Ok(None);
    };
    let api_id = std::env::var("TELEGRAM_API_ID")
        .map_err(|_| anyhow!("TELEGRAM_API_ID is required when LOCAL_BOT_API_BIN is set"))?;
    let api_hash = std::env::var("TELEGRAM_API_HASH")
        .map_err(|_| anyhow!("TELEGRAM_API_HASH is required when LOCAL_BOT_API_BIN is set"))?;
    let data_dir = std::env::var_os("TELEGRAM_LOCAL_FILES_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| {
            anyhow!("TELEGRAM_LOCAL_FILES_DIR is required when LOCAL_BOT_API_BIN is set")
        })?;
    std::fs::create_dir_all(&data_dir)
        .map_err(|_| anyhow!("Cannot create Local Bot API data directory"))?;
    let data_dir = std::fs::canonicalize(data_dir)
        .map_err(|_| anyhow!("Cannot resolve Local Bot API data directory"))?;

    let port = std::env::var("LOCAL_BOT_API_PORT")
        .unwrap_or_else(|_| "8081".to_owned());
    let port_number = port
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| anyhow!("LOCAL_BOT_API_PORT must be a valid TCP port"))?;
    let port = port_number.to_string();
    let temp_dir = std::env::var_os("LOCAL_BOT_API_TEMP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("tmp"));
    std::fs::create_dir_all(&temp_dir)
        .map_err(|_| anyhow!("Cannot create Local Bot API temporary directory"))?;
    let temp_dir = std::fs::canonicalize(temp_dir)
        .map_err(|_| anyhow!("Cannot resolve Local Bot API temporary directory"))?;

    let work_dir = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/jobs"));
    std::fs::create_dir_all(&work_dir)
        .map_err(|_| anyhow!("Cannot create conversion workspace directory"))?;
    let work_dir = std::fs::canonicalize(work_dir)
        .map_err(|_| anyhow!("Cannot resolve conversion workspace directory"))?;

    // A BotKeep deployment uses loopback because both processes share a container.
    std::env::set_var("TELOXIDE_API_URL", format!("http://127.0.0.1:{port}"));
    std::env::set_var("TELEGRAM_LOCAL_FILES_DIR", &data_dir);
    std::env::set_var("TMPDIR", &work_dir);

    let child = Command::new(binary)
        .arg(format!("--api-id={api_id}"))
        .arg(format!("--api-hash={api_hash}"))
        .arg("--local")
        .arg(format!("--http-port={port}"))
        .arg(format!("--dir={}", data_dir.display()))
        .arg(format!("--temp-dir={}", temp_dir.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|_| {
            anyhow!(
                "Could not start Local Bot API Server; check LOCAL_BOT_API_BIN and executable permission"
            )
        })?;

    log::info!("Local Bot API Server process started on port {}", port);
    Ok(Some(LocalApiProcess { child }))
}

async fn ensure_bot_credentials(bot: &Bot, local_api_started: bool) -> AnyResult<()> {
    let attempts = if local_api_started { 30 } else { 1 };
    for attempt in 1..=attempts {
        match bot.get_me().send().await {
            Ok(_) => return Ok(()),
            Err(_) if attempt < attempts => {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(_) => {
                return Err(anyhow!("Failed to authenticate bot with Telegram API"));
            }
        }
    }
    Err(anyhow!("Failed to authenticate bot with Telegram API"))
}

fn next_midnight_utc_seconds() -> u64 {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs();
    let secs_into_day = now_secs % 86_400;
    if secs_into_day == 0 {
        86_400
    } else {
        86_400 - secs_into_day
    }
}

fn parse_env_limit(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn parse_telegram_user_id(value: &str) -> AnyResult<u64> {
    let user_id = value
        .parse::<u64>()
        .map_err(|_| anyhow!("ALLOWED_TELEGRAM_USER_ID must be a positive integer"))?;
    if user_id == 0 {
        return Err(anyhow!(
            "ALLOWED_TELEGRAM_USER_ID must be a positive integer"
        ));
    }
    Ok(user_id)
}

async fn start_quota_monitor(limiter: Arc<Mutex<RateLimiter>>) {
    loop {
        sleep(TokioDuration::from_secs(60)).await;
        let now_day_index = utc_day_index(SystemTime::now());
        let mut limiter = limiter.lock().await;
        if limiter.reset_if_new_day(now_day_index) {
            log::info!(
                "Daily quotas reset at UTC midnight: day_index={}, next_reset_in_seconds={}",
                now_day_index,
                next_midnight_utc_seconds(),
            );
        }
    }
}

#[tokio::main]
async fn main() -> AnyResult<()> {
    let argument = std::env::args_os().nth(1);
    match argument.as_deref() {
        Some(argument) if argument == std::ffi::OsStr::new("--version") => {
            println!("converter-bot {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(argument) if argument == std::ffi::OsStr::new("--drain-queue") => {}
        Some(_) => return Err(anyhow!("usage: converter-bot [--version|--drain-queue]")),
        None => {}
    }

    dotenv().ok();
    pretty_env_logger::formatted_timed_builder()
        .parse_filters(&std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()))
        .init();
    log::info!("Starting bot");
    let local_api_process = start_local_api()?;
    std::fs::create_dir_all(std::env::temp_dir())
        .map_err(|_| anyhow!("Cannot create the configured temporary work directory"))?;
    let stale_workspaces = media_workspace::MediaWorkspace::cleanup_stale()?;
    if stale_workspaces > 0 {
        log::info!(
            "Removed abandoned conversion workspaces at startup: count={}",
            stale_workspaces
        );
    }

    let user_daily_limit = parse_env_limit("USER_DAILY_LIMIT", DEFAULT_USER_DAILY_LIMIT);
    let global_daily_limit = parse_env_limit("GLOBAL_DAILY_LIMIT", DEFAULT_GLOBAL_DAILY_LIMIT);
    let max_input_bytes = u64::from(parse_env_limit("MAX_INPUT_BYTES", DEFAULT_MAX_INPUT_BYTES));
    let max_output_bytes = u64::from(parse_env_limit(
        "MAX_OUTPUT_BYTES",
        DEFAULT_MAX_OUTPUT_BYTES,
    ));
    // The BotKeep instance has a strict 512 MiB / 50% CPU budget.
    // Keep exactly one conversion active regardless of environment overrides.
    let max_concurrent_conversions = 1_usize;

    let (bot, downloads) = telegram::configured_bot().await?;

    ensure_bot_credentials(&bot, local_api_process.is_some()).await?;
    let stale_cache_files = downloads.cleanup_stale_cache().await?;
    if stale_cache_files > 0 {
        log::info!(
            "Removed stale Local Bot API media cache files at startup: count={}",
            stale_cache_files
        );
    }

    if argument.as_deref() == Some(std::ffi::OsStr::new("--drain-queue")) {
        let allowed_user_id = parse_telegram_user_id(
            &std::env::var("ALLOWED_TELEGRAM_USER_ID")
                .map_err(|_| anyhow!("ALLOWED_TELEGRAM_USER_ID is required in queue mode"))?,
        )?;
        let max_conversions_per_run =
            parse_env_limit("MAX_CONVERSIONS_PER_RUN", DEFAULT_MAX_CONVERSIONS_PER_RUN)
                .clamp(1, 20);
        let max_updates_per_run =
            parse_env_limit("MAX_UPDATES_PER_RUN", DEFAULT_MAX_UPDATES_PER_RUN).clamp(1, 100);
        return serverless::drain(
            &bot,
            max_input_bytes,
            max_output_bytes,
            max_conversions_per_run,
            max_updates_per_run,
            allowed_user_id,
            &downloads,
        )
        .await;
    }

    let limiter = Arc::new(Mutex::new(RateLimiter::new(
        user_daily_limit,
        global_daily_limit,
    )));
    let monitor_limiter = Arc::clone(&limiter);
    let conversion_slots = Arc::new(tokio::sync::Semaphore::new(max_concurrent_conversions));

    {
        let limiter = limiter.lock().await;
        log::info!(
            "Limits initialized: day_index={}, user_daily_limit={}, global_daily_limit={}, max_input_bytes={}, max_output_bytes={}, max_concurrent_conversions={}, next_reset_in_seconds={}",
            limiter.current_day_index(),
            user_daily_limit,
            global_daily_limit,
            max_input_bytes,
            max_output_bytes,
            max_concurrent_conversions,
            next_midnight_utc_seconds(),
        );
    }

    tokio::spawn(async move {
        start_quota_monitor(monitor_limiter).await;
    });

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let limiter = Arc::clone(&limiter);
        let conversion_slots = Arc::clone(&conversion_slots);
        let downloads = downloads.clone();
        async move {
            if process_video(
                &bot,
                &msg,
                &limiter,
                &conversion_slots,
                max_input_bytes,
                max_output_bytes,
                &downloads,
            )
            .await
            .is_err()
            {
                log::error!("Video processing failed");
            }
            respond(())
        }
    })
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_telegram_user_id;

    #[test]
    fn accepts_positive_telegram_user_id() {
        assert_eq!(parse_telegram_user_id("123456789").unwrap(), 123456789);
    }

    #[test]
    fn rejects_invalid_telegram_user_id() {
        assert!(parse_telegram_user_id("").is_err());
        assert!(parse_telegram_user_id("0").is_err());
        assert!(parse_telegram_user_id("-1").is_err());
        assert!(parse_telegram_user_id("not-an-id").is_err());
    }
}
