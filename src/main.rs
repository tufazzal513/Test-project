use anyhow::{anyhow, bail, Context, Result};
use reqwest::{multipart, Client};
use tokio_util::io::ReaderStream;
use serde::{Deserialize, Serialize};
use std::{env, path::{Path, PathBuf}, process::Stdio, time::Duration};
use tokio::{fs, process::Command, time::sleep};
use tracing::{error, info, warn};

#[derive(Clone)]
struct Config {
    token: String,
    api_base: String,
    work_dir: PathBuf,
    timeout_secs: u64,
    min_free_mb: u64,
    max_upload_mb: u64,
    allowed_users: Vec<i64>,
}

#[derive(Deserialize)]
struct ApiResponse<T> { ok: bool, result: Option<T>, description: Option<String> }
#[derive(Deserialize)]
struct Update { update_id: i64, message: Option<Message> }
#[derive(Deserialize)]
struct Message { message_id: i64, chat: Chat, from: Option<User>, text: Option<String>, video: Option<TgFile>, document: Option<TgFile> }
#[derive(Deserialize)]
struct Chat { id: i64 }
#[derive(Deserialize)]
struct User { id: i64 }
#[derive(Deserialize)]
struct TgFile { file_id: String, file_name: Option<String>, file_size: Option<u64> }
#[derive(Deserialize)]
struct FileInfo { file_path: Option<String> }
#[derive(Deserialize)]
struct Probe { streams: Vec<Stream>, format: Option<Format> }
#[derive(Deserialize)]
struct Stream { index: usize, codec_type: Option<String>, codec_name: Option<String>, tags: Option<std::collections::HashMap<String,String>> }
#[derive(Deserialize)]
struct Format { tags: Option<std::collections::HashMap<String,String>> }
#[derive(Serialize)]
struct GetUpdates { offset: i64, timeout: u8, allowed_updates: [&'static str; 1] }

impl Config {
    fn load() -> Result<Self> {
        let token = env::var("BOT_TOKEN").context("BOT_TOKEN is required")?;
        let allowed_users = env::var("ALLOWED_USER_IDS").unwrap_or_default().split(',').filter_map(|s| s.trim().parse().ok()).collect();
        Ok(Self {
            token,
            api_base: env::var("BOT_API_BASE").unwrap_or_else(|_| "https://api.telegram.org".into()).trim_end_matches('/').to_string(),
            work_dir: PathBuf::from(env::var("WORK_DIR").unwrap_or_else(|_| "./data".into())),
            timeout_secs: env::var("CONVERSION_TIMEOUT_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(7200),
            min_free_mb: env::var("MIN_FREE_DISK_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(512),
            max_upload_mb: env::var("MAX_UPLOAD_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(1900),
        })
    }
    fn api(&self, method: &str) -> String { format!("{}/bot{}/{}", self.api_base, self.token, method) }
    fn file_url(&self, path: &str) -> String { format!("{}/file/bot{}/{}", self.api_base, self.token, path) }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".into())).init();
    let cfg = Config::load()?;
    fs::create_dir_all(&cfg.work_dir).await?;
    for d in ["jobs", "logs"] { fs::create_dir_all(cfg.work_dir.join(d)).await?; }
    let client = Client::builder().timeout(Duration::from_secs(300)).build()?;
    recover_old_jobs(&cfg.work_dir).await?;
    let mut offset = 0_i64;
    info!("Rust Video Converter Bot started; jobs are processed sequentially");
    loop {
        let req = GetUpdates { offset, timeout: 45, allowed_updates: ["message"] };
        match client.post(cfg.api("getUpdates")).json(&req).timeout(Duration::from_secs(55)).send().await {
            Ok(resp) => match resp.json::<ApiResponse<Vec<Update>>>().await {
                Ok(body) if body.ok => {
                    for update in body.result.unwrap_or_default() {
                        offset = update.update_id + 1;
                        if let Some(msg) = update.message {
                            if let Err(e) = handle_message(&client, &cfg, msg).await { error!(error=%e, "message processing failed"); }
                        }
                    }
                }
                Ok(body) => { warn!(description=?body.description, "Telegram API returned error"); sleep(Duration::from_secs(3)).await; }
                Err(e) => { warn!(error=%e, "invalid getUpdates response"); sleep(Duration::from_secs(3)).await; }
            },
            Err(e) => { warn!(error=%e, "polling failed"); sleep(Duration::from_secs(5)).await; }
        }
    }
}

async fn handle_message(client: &Client, cfg: &Config, msg: Message) -> Result<()> {
    let text = msg.text.as_deref().unwrap_or("");
    if text == "/start" || text == "/help" {
        return send_text(client, cfg, msg.chat.id, "Send me a video as a Telegram document or video. I will convert it to MP4 and return it as a document.\n\nRemux is used when compatible; otherwise video/audio are transcoded. Processing is sequential.").await;
    }
    let Some(file) = msg.document.or(msg.video) else {
        if !text.is_empty() { send_text(client, cfg, msg.chat.id, "Please send a video file. Use /help for instructions.").await?; }
        return Ok(());
    };
    if let Some(user) = msg.from.as_ref() {
        if !cfg.allowed_users.is_empty() && !cfg.allowed_users.contains(&user.id) {
            send_text(client, cfg, msg.chat.id, "Sorry, this bot is private.").await?;
            return Ok(());
        }
    }
    if let Some(size) = file.file_size {
        if size > cfg.max_upload_mb * 1024 * 1024 {
            send_text(client, cfg, msg.chat.id, &format!("File is larger than the configured {} MB limit.", cfg.max_upload_mb)).await?;
            return Ok(());
        }
    }
    let job = cfg.work_dir.join("jobs").join(format!("{}_{}", msg.chat.id, msg.message_id));
    fs::create_dir_all(&job).await?;
    let result = process_job(client, cfg, msg.chat.id, &file, &job).await;
    // Always remove this job directory. Output is retained until Telegram confirms upload.
    if let Err(e) = fs::remove_dir_all(&job).await { warn!(path=%job.display(), error=%e, "job cleanup failed"); }
    if let Err(e) = result {
        error!(error=%e, "conversion job failed");
        let _ = send_text(client, cfg, msg.chat.id, &format!("Conversion failed: {}", short_error(&e))).await;
    }
    Ok(())
}

async fn process_job(client: &Client, cfg: &Config, chat_id: i64, file: &TgFile, job: &Path) -> Result<()> {
    ensure_disk_space(job, cfg.min_free_mb)?;
    send_text(client, cfg, chat_id, "Your video is queued. Conversion will start shortly.").await?;
    let info: FileInfo = api_call(client, cfg, "getFile", serde_json::json!({"file_id":file.file_id})).await?;
    let remote = info.file_path.ok_or_else(|| anyhow!("Telegram did not return file_path"))?;
    let input = job.join("source.bin");
    // Local Bot API Server may return an absolute filesystem path. If it is
    // visible to this process (same host/shared volume), copy it locally.
    if Path::new(&remote).is_absolute() && Path::new(&remote).is_file() {
        fs::copy(&remote, &input).await.context("Could not copy Local Bot API file; mount its file directory into this container")?;
    } else {
        download_to(client, &cfg.file_url(&remote), &input).await?;
    }
    ensure_disk_space(job, cfg.min_free_mb)?;
    let probe = probe_file(&input).await?;
    if !probe.streams.iter().any(|s| s.codec_type.as_deref() == Some("video")) { bail!("The uploaded file does not contain a video stream"); }
    let output = job.join("converted.mp4");
    let compatible = is_mp4_stream_compatible(&probe);
    let mode = if compatible { "remux" } else { "transcode" };
    send_text(client, cfg, chat_id, if compatible { "Converting: compatible streams detected, trying lossless remux." } else { "Converting: incompatible codec detected, using high-quality H.264/AAC transcode. Quality may differ slightly from the source." }).await?;
    let conversion = if compatible { run_ffmpeg(cfg, &input, &output, false, &probe).await } else { run_ffmpeg(cfg, &input, &output, true, &probe).await };
    if let Err(e) = conversion {
        if compatible {
            warn!(error=%e, "remux failed; trying transcode fallback");
            run_ffmpeg(cfg, &input, &output, true, &probe).await?;
        } else { return Err(e); }
    }
    let verified = probe_file(&output).await?;
    if !verified.streams.iter().any(|s| s.codec_type.as_deref() == Some("video")) { bail!("Output validation failed: no video stream"); }
    let size = fs::metadata(&output).await?.len();
    if size > cfg.max_upload_mb * 1024 * 1024 { bail!("Converted file exceeds configured Telegram upload limit"); }
    let filename = safe_filename(file.file_name.as_deref().unwrap_or("converted"));
    let filename = format!("{}.mp4", filename);
    send_document(client, cfg, chat_id, &output, &filename).await
        .with_context(|| format!("Telegram upload failed (conversion mode: {mode}); output was not sent"))?;
    info!(chat_id, mode, output_bytes=size, "conversion uploaded successfully; temporary files will be deleted");
    Ok(())
}

fn is_mp4_stream_compatible(p: &Probe) -> bool {
    p.streams.iter().filter(|s| matches!(s.codec_type.as_deref(), Some("video"|"audio")))
        .all(|s| match s.codec_type.as_deref() {
            Some("video") => matches!(s.codec_name.as_deref(), Some("h264"|"hevc"|"mpeg4"|"av1")),
            Some("audio") => matches!(s.codec_name.as_deref(), Some("aac"|"mp3"|"alac"|"ac3"|"eac3")),
            _ => true,
        })
}

async fn probe_file(path: &Path) -> Result<Probe> {
    let out = Command::new("ffprobe").args(["-v", "error", "-show_streams", "-show_format", "-of", "json"]).arg(path).output().await?;
    if !out.status.success() { bail!("ffprobe could not read the video: {}", String::from_utf8_lossy(&out.stderr)); }
    Ok(serde_json::from_slice(&out.stdout).context("Could not parse ffprobe output")?)
}

async fn run_ffmpeg(cfg: &Config, input: &Path, output: &Path, transcode: bool, probe: &Probe) -> Result<()> {
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner").arg("-nostdin").arg("-y").arg("-threads").arg("1").arg("-i").arg(input)
        .args(["-map", "0:v:0", "-map", "0:a?"]);
    // MP4 supports text subtitles; bitmap subtitle formats (PGS/VobSub) cannot be
    // represented as mov_text and are deliberately excluded rather than failing the job.
    for stream in probe.streams.iter().filter(|s| s.codec_type.as_deref() == Some("subtitle")) {
        if matches!(stream.codec_name.as_deref(), Some("subrip"|"ass"|"ssa"|"webvtt"|"mov_text")) {
            cmd.arg("-map").arg(format!("0:{}", stream.index));
        }
    }
    cmd.args(["-map_metadata", "0", "-map_chapters", "0"]);
    if transcode {
        cmd.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p", "-threads:v", "1"])
            .args(["-c:a", "aac", "-b:a", "192k", "-threads:a", "1"])
            .args(["-c:s", "mov_text"]);
    } else {
        cmd.args(["-c:v", "copy", "-c:a", "copy", "-c:s", "mov_text"]);
    }
    cmd.args(["-max_muxing_queue_size", "2048", "-movflags", "+faststart"]).arg(output)
        .stdout(Stdio::null()).stderr(Stdio::piped());
    let child = cmd.spawn().context("Could not start FFmpeg; ensure ffmpeg is installed with libx264 and AAC support")?;
    let result = tokio::time::timeout(Duration::from_secs(cfg.timeout_secs), child.wait_with_output()).await;
    let out = result.map_err(|_| anyhow!("FFmpeg timed out after {} seconds", cfg.timeout_secs))??;
    if !out.status.success() { bail!("FFmpeg failed: {}", tail(&String::from_utf8_lossy(&out.stderr), 1800)); }
    Ok(())
}

async fn download_to(client: &Client, url: &str, path: &Path) -> Result<()> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut file = fs::File::create(path).await?;
    while let Some(chunk) = response.chunk().await? { tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?; }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    Ok(())
}

async fn send_document(client: &Client, cfg: &Config, chat: i64, path: &Path, filename: &str) -> Result<()> {
    let file = fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let stream = ReaderStream::new(file);
    let part = multipart::Part::stream_with_length(reqwest::Body::wrap_stream(stream), len).file_name(filename.to_string()).mime_str("video/mp4")?;
    let form = multipart::Form::new().text("chat_id", chat.to_string()).part("document", part);
    let response = client.post(cfg.api("sendDocument")).multipart(form).timeout(Duration::from_secs(900)).send().await?;
    let body: ApiResponse<serde_json::Value> = response.json().await?;
    if !body.ok { bail!("Telegram sendDocument failed: {}", body.description.unwrap_or_default()); }
    Ok(())
}

async fn send_text(client: &Client, cfg: &Config, chat: i64, text: &str) -> Result<()> {
    let body: ApiResponse<serde_json::Value> = client.post(cfg.api("sendMessage")).json(&serde_json::json!({"chat_id":chat,"text":text})).send().await?.json().await?;
    if !body.ok { warn!(description=?body.description, "sendMessage failed"); }
    Ok(())
}

async fn api_call<T: for<'de> Deserialize<'de>>(client: &Client, cfg: &Config, method: &str, payload: serde_json::Value) -> Result<T> {
    let body: ApiResponse<T> = client.post(cfg.api(method)).json(&payload).send().await?.error_for_status()?.json().await?;
    if !body.ok { bail!("Telegram API error: {}", body.description.unwrap_or_default()); }
    body.result.ok_or_else(|| anyhow!("Telegram API returned an empty result"))
}

fn ensure_disk_space(path: &Path, min_mb: u64) -> Result<()> {
    let available = fs2::available_space(path)?;
    if available < min_mb * 1024 * 1024 { bail!("Not enough free disk space. At least {min_mb} MB must remain available"); }
    Ok(())
}
async fn recover_old_jobs(work: &Path) -> Result<()> {
    let mut entries = fs::read_dir(work.join("jobs")).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            warn!(path=%entry.path().display(), "removing abandoned job directory from previous run");
            fs::remove_dir_all(entry.path()).await?;
        }
    }
    Ok(())
}
fn safe_filename(s: &str) -> String {
    let stem = Path::new(s).file_stem().and_then(|v| v.to_str()).unwrap_or("converted");
    let clean: String = stem.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(80).collect();
    if clean.is_empty() { "converted".into() } else { clean }
}
fn tail(s: &str, n: usize) -> String { s.chars().rev().take(n).collect::<String>().chars().rev().collect() }
fn short_error(e: &anyhow::Error) -> String { tail(&format!("{e:#}"), 700) }
