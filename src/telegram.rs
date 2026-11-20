use anyhow::{anyhow, Context, Result as AnyResult};
use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};
use teloxide::{prelude::*, ApiError, RequestError};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
};

#[derive(Clone, Default)]
pub struct DownloadConfig {
    local_files_dir: Option<PathBuf>,
    token_cache_dir: Option<PathBuf>,
}

impl DownloadConfig {
    async fn new(
        api_url: &reqwest::Url,
        local_files_dir: Option<PathBuf>,
        bot_token: &str,
    ) -> AnyResult<Self> {
        let local_files_dir = match local_files_dir {
            Some(directory) => {
                if api_url.host_str() == Some("api.telegram.org") {
                    return Err(anyhow!(
                        "TELEGRAM_LOCAL_FILES_DIR requires a local TELOXIDE_API_URL"
                    ));
                }
                if !directory.is_absolute() {
                    return Err(anyhow!("TELEGRAM_LOCAL_FILES_DIR must be absolute"));
                }
                let directory = fs::canonicalize(directory)
                    .await
                    .context("Cannot access TELEGRAM_LOCAL_FILES_DIR")?;
                if directory.parent().is_none() || !fs::metadata(&directory).await?.is_dir() {
                    return Err(anyhow!(
                        "TELEGRAM_LOCAL_FILES_DIR must be a dedicated directory"
                    ));
                }
                Some(directory)
            }
            None => None,
        };
        let token_cache_dir = local_files_dir
            .as_ref()
            .map(|directory| directory.join(bot_token));
        Ok(Self {
            local_files_dir,
            token_cache_dir,
        })
    }

    /// Remove only Telegram's disposable cached `file_*` media files for this
    /// bot. Keep the TDLib database, session state, and every other file intact.
    pub async fn cleanup_stale_cache(&self) -> AnyResult<usize> {
        let Some(directory) = &self.token_cache_dir else {
            return Ok(0);
        };
        let mut entries = match fs::read_dir(directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error).context("Cannot inspect Local Bot API cache"),
        };
        let mut removed = 0usize;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("file_") {
                continue;
            }
            let file_type = entry.file_type().await?;
            if !file_type.is_file() {
                continue;
            }
            match fs::remove_file(entry.path()).await {
                Ok(()) => removed += 1,
                Err(error) => log::warn!(
                    "Could not remove a stale Local Bot API media cache file: {error}"
                ),
            }
        }
        Ok(removed)
    }
}

fn parse_api_url(value: &str) -> AnyResult<reqwest::Url> {
    let url = reqwest::Url::parse(value.trim_end_matches('/'))
        .map_err(|_| anyhow!("TELOXIDE_API_URL must be an HTTP(S) base URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "TELOXIDE_API_URL must be an HTTP(S) base URL without credentials, query or fragment"
        ));
    }
    Ok(url)
}

fn optional_env(name: &str) -> AnyResult<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(anyhow!("{name} must be valid UTF-8")),
    }
}

fn http_client_builder(timeout: Duration) -> reqwest::ClientBuilder {
    teloxide::net::default_reqwest_settings()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
}

/// Configure the same endpoint and client for API requests and HTTP downloads.
pub async fn configured_bot() -> AnyResult<(Bot, DownloadConfig)> {
    let api_url = parse_api_url(
        &optional_env("TELOXIDE_API_URL")?
            .unwrap_or_else(|| teloxide::net::TELEGRAM_API_URL.to_owned()),
    )?;
    let token = optional_env("TELOXIDE_TOKEN")?.context("TELOXIDE_TOKEN is required")?;
    let downloads = DownloadConfig::new(
        &api_url,
        std::env::var_os("TELEGRAM_LOCAL_FILES_DIR").map(PathBuf::from),
        &token,
    )
    .await?;
    let timeout_seconds = optional_env("TELEGRAM_REQUEST_TIMEOUT_SECONDS")?
        .unwrap_or_else(|| "300".to_owned())
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .context("TELEGRAM_REQUEST_TIMEOUT_SECONDS must be a positive integer")?;
    let mut client = http_client_builder(Duration::from_secs(timeout_seconds));
    if let Some(proxy) = optional_env("TELOXIDE_PROXY")? {
        client = client.proxy(
            reqwest::Proxy::all(proxy)
                .map_err(|_| anyhow!("Invalid TELOXIDE_PROXY configuration"))?,
        );
    }
    let client = client
        .build()
        .context("Failed to create Telegram HTTP client")?;
    Ok((
        Bot::with_client(token, client).set_api_url(api_url),
        downloads,
    ))
}

#[derive(Debug)]
pub struct FileTooLargeError {
    pub limit_bytes: Option<u64>,
}

impl fmt::Display for FileTooLargeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.limit_bytes {
            Some(limit_bytes) => write!(
                formatter,
                "file exceeds the configured download limit of {limit_bytes} bytes"
            ),
            None => {
                formatter.write_str("Telegram refused to provide the file because it is too large")
            }
        }
    }
}

impl Error for FileTooLargeError {}

fn check_size(size: u64, max_bytes: u64) -> AnyResult<()> {
    if size > max_bytes {
        return Err(FileTooLargeError {
            limit_bytes: Some(max_bytes),
        }
        .into());
    }
    Ok(())
}

fn is_telegram_file_too_large(error: &RequestError) -> bool {
    match error {
        RequestError::Api(ApiError::RequestEntityTooLarge) => true,
        RequestError::Api(ApiError::Unknown(message)) => {
            message.to_ascii_lowercase().contains("file is too big")
        }
        _ => false,
    }
}

fn extract_extension(file_path: &str) -> &str {
    let extension = Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("bin");

    if extension.len() <= 10 && extension.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        extension
    } else {
        "bin"
    }
}

/// Скачивает файл с серверов Telegram по его идентификатору.
pub async fn download_file(
    bot: &Bot,
    file_id: &str,
    declared_size: u64,
    max_bytes: u64,
    directory: &Path,
    config: &DownloadConfig,
) -> AnyResult<String> {
    // In local mode getFile downloads the source into the API server's cache.
    // Reject an advertised oversize file before starting that work.
    check_size(declared_size, max_bytes)?;
    let file = bot
        .get_file(file_id.to_owned().into())
        .send()
        .await
        .map_err(|error| {
            if is_telegram_file_too_large(&error) {
                anyhow::Error::new(FileTooLargeError { limit_bytes: None })
            } else {
                anyhow!("Telegram file lookup failed")
            }
        })?;

    if let Err(error) = check_size(u64::from(file.size), max_bytes) {
        if let Err(cleanup_error) = cleanup_cached_file(config, &file.path).await {
            log::warn!("Could not remove oversized Local Bot API cache file: {cleanup_error}");
        }
        return Err(error);
    }
    let materialized = materialize_file(bot, &file.path, max_bytes, directory, config).await;
    if let Err(error) = cleanup_cached_file(config, &file.path).await {
        log::warn!("Could not remove the consumed Local Bot API source cache file: {error}");
    }
    materialized
}

async fn cleanup_cached_file(config: &DownloadConfig, telegram_path: &str) -> AnyResult<()> {
    let Some(root) = &config.local_files_dir else {
        return Ok(());
    };
    let Some(token_dir) = &config.token_cache_dir else {
        return Ok(());
    };
    let source = fs::canonicalize(telegram_path)
        .await
        .context("Cannot resolve consumed Local Bot API cache file")?;
    let canonical_token_dir = fs::canonicalize(token_dir)
        .await
        .context("Cannot resolve this bot's Local Bot API cache directory")?;
    // Local Bot API versions may place media under nested folders such as
    // <data>/<bot-token>/documents/file_123.mp4. Permit only regular file_*
    // media files whose canonical path remains inside this bot's token cache.
    // Canonicalization prevents symlink/path traversal from escaping the cache.
    let is_media_cache_file = source
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("file_"));
    if !canonical_token_dir.starts_with(root)
        || !source.starts_with(&canonical_token_dir)
        || source == canonical_token_dir
        || !is_media_cache_file
        || !fs::metadata(&source).await?.is_file()
    {
        return Err(anyhow!("Refusing to delete a file outside this bot's Local Bot API cache"));
    }
    fs::remove_file(source)
        .await
        .context("Failed to delete consumed Local Bot API cache file")
}

async fn materialize_file(
    bot: &Bot,
    telegram_path: &str,
    max_bytes: u64,
    directory: &Path,
    config: &DownloadConfig,
) -> AnyResult<String> {
    let extension = extract_extension(telegram_path);
    let file_path = directory.join(format!("input.{extension}"));
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file_path)
        .await
        .context("Failed to create temporary media file")?;

    let download_result: AnyResult<()> = async {
        if let Some(root) = &config.local_files_dir {
            let mut source = open_local_file(root, Path::new(telegram_path)).await?;
            check_size(source.metadata().await?.len(), max_bytes)?;
            copy_limited(&mut source, &mut destination, max_bytes).await?;
        } else {
            let url = download_url(bot, telegram_path)?;
            let mut response = bot
                .client()
                .get(url)
                .send()
                .await
                .map_err(reqwest::Error::without_url)?
                .error_for_status()
                .map_err(reqwest::Error::without_url)?;
            if !response.status().is_success() {
                return Err(anyhow!(
                    "Telegram file download returned an unexpected status"
                ));
            }
            if let Some(size) = response.content_length() {
                check_size(size, max_bytes)?;
            }
            let mut downloaded_bytes = 0_u64;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(reqwest::Error::without_url)?
            {
                downloaded_bytes = downloaded_bytes
                    .checked_add(chunk.len() as u64)
                    .context("Telegram download size overflow")?;
                check_size(downloaded_bytes, max_bytes)?;
                destination.write_all(&chunk).await?;
            }
        }
        destination.flush().await?;
        Ok(())
    }
    .await;

    if let Err(error) = download_result {
        drop(destination);
        if fs::remove_file(&file_path).await.is_err() {
            log::warn!("Failed to remove an incomplete temporary download");
        }
        return Err(error);
    }

    Ok(file_path.to_string_lossy().into_owned())
}

fn download_url(bot: &Bot, telegram_path: &str) -> AnyResult<reqwest::Url> {
    if telegram_path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(anyhow!(
            "Telegram returned a non-relative file path; configure TELEGRAM_LOCAL_FILES_DIR for local Bot API"
        ));
    }
    let mut url = bot.api_url();
    url.path_segments_mut()
        .map_err(|_| anyhow!("Telegram API URL cannot contain file paths"))?
        .pop_if_empty()
        .push("file")
        .push(&format!("bot{}", bot.token()))
        .extend(telegram_path.split('/'));
    Ok(url)
}

async fn open_local_file(root: &Path, source: &Path) -> AnyResult<fs::File> {
    if !source.is_absolute() {
        return Err(anyhow!(
            "Local Bot API must return an absolute path; start it with --local"
        ));
    }
    let source = fs::canonicalize(source)
        .await
        .context("Cannot access the local Bot API file; check the shared volume")?;
    if !source.starts_with(root) || source == root {
        return Err(anyhow!(
            "Local Bot API file is outside TELEGRAM_LOCAL_FILES_DIR"
        ));
    }
    if !fs::metadata(&source).await?.is_file() {
        return Err(anyhow!("Local Bot API path is not a regular file"));
    }
    // Read the API server's cache in place. The caller removes this exact
    // consumed media file only after the copy has completed.
    fs::File::open(source)
        .await
        .context("Failed to open the local Bot API file")
}

async fn copy_limited<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    source: &mut R,
    destination: &mut W,
    max_bytes: u64,
) -> AnyResult<()> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut copied_bytes = 0_u64;
    loop {
        let count = source.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        copied_bytes = copied_bytes
            .checked_add(count as u64)
            .context("Local Telegram copy size overflow")?;
        check_size(copied_bytes, max_bytes)?;
        destination.write_all(&buffer[..count]).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        copy_limited, download_file, download_url, extract_extension, http_client_builder,
        is_telegram_file_too_large, materialize_file, open_local_file, parse_api_url,
        DownloadConfig, FileTooLargeError,
    };
    use std::path::Path;
    use std::time::Duration;
    use teloxide::{ApiError, Bot, RequestError};
    use tokio::fs;

    fn test_bot() -> Bot {
        Bot::with_client(
            "123:test-token",
            http_client_builder(Duration::from_secs(5))
                .no_proxy()
                .build()
                .unwrap(),
        )
        .set_api_url(parse_api_url("http://127.0.0.1:1").unwrap())
    }

    async fn local_config(root: &Path) -> DownloadConfig {
        DownloadConfig::new(&test_bot().api_url(), Some(root.to_path_buf()), "123:test-token")
            .await
            .unwrap()
    }

    fn mock_api(responses: Vec<(&'static str, Vec<u8>)>) -> (Bot, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for (expected_path, response) in responses {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "Expected API request timed out");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("Failed to accept test request: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let path = request_line.split_whitespace().nth(1).unwrap();
                assert_eq!(path.to_ascii_lowercase(), expected_path);
                let mut content_length = 0;
                loop {
                    let mut header = String::new();
                    assert!(reader.read_line(&mut header).unwrap() > 0);
                    if header == "\r\n" {
                        break;
                    }
                    if let Some(length) =
                        header.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        content_length = length.trim().parse::<usize>().unwrap();
                    }
                }
                reader.read_exact(&mut vec![0; content_length]).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len(),
                )
                .unwrap();
                stream.write_all(&response).unwrap();
            }
        });
        (test_bot().set_api_url(parse_api_url(&url).unwrap()), worker)
    }

    #[test]
    fn builds_download_url_with_configured_endpoint_and_prefix() {
        let bot = test_bot().set_api_url(parse_api_url("https://example.com/tg/").unwrap());
        let url = download_url(&bot, "videos/source #1.mkv").unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.com/tg/file/bot123:test-token/videos/source%20%231.mkv",
        );
        let cloud = test_bot().set_api_url(parse_api_url("https://api.telegram.org").unwrap());
        assert_eq!(
            download_url(&cloud, "videos/source.mkv").unwrap().as_str(),
            "https://api.telegram.org/file/bot123:test-token/videos/source.mkv",
        );
        assert!(download_url(&bot, "../secret").is_err());
        assert!(download_url(&bot, "/absolute/path").is_err());
    }

    #[test]
    fn rejects_invalid_api_configuration_without_echoing_values() {
        for value in [
            "file:///tmp/api",
            "https://user:secret@example.com",
            "https://example.com?token=secret",
            "https://example.com#secret",
        ] {
            let error = parse_api_url(value).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
    }

    #[tokio::test]
    async fn rejects_local_directory_with_cloud_api() {
        let root = tempfile::tempdir().unwrap();
        assert!(DownloadConfig::new(
            &parse_api_url("https://api.telegram.org").unwrap(),
            Some(root.path().to_path_buf()),
            "123:test-token",
        )
        .await
        .is_err());
        assert!(
            DownloadConfig::new(
                &test_bot().api_url(),
                Some("relative-cache".into()),
                "123:test-token",
            )
            .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn startup_cleanup_removes_only_this_bots_cached_media_files() {
        let root = tempfile::tempdir().unwrap();
        let token_dir = root.path().join("123:test-token");
        fs::create_dir(&token_dir).await.unwrap();
        fs::write(token_dir.join("file_1.mp4"), b"cached").await.unwrap();
        fs::write(token_dir.join("database"), b"session").await.unwrap();
        fs::create_dir(token_dir.join("nested")).await.unwrap();
        fs::write(token_dir.join("nested/file_2.mp4"), b"nested")
            .await
            .unwrap();

        let config = DownloadConfig {
            local_files_dir: Some(fs::canonicalize(root.path()).await.unwrap()),
            token_cache_dir: Some(token_dir.clone()),
        };
        assert_eq!(config.cleanup_stale_cache().await.unwrap(), 1);
        assert!(!token_dir.join("file_1.mp4").exists());
        assert!(token_dir.join("database").exists());
        assert!(token_dir.join("nested/file_2.mp4").exists());
    }

    #[tokio::test]
    async fn rejects_advertised_size_before_get_file() {
        let directory = tempfile::tempdir().unwrap();
        // No API is listening here. A network request would produce a different error.
        let error = download_file(
            &test_bot(),
            "test-file",
            101,
            100,
            directory.path(),
            &DownloadConfig::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<FileTooLargeError>()
                .unwrap()
                .limit_bytes,
            Some(100),
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn copies_local_file_above_cloud_limit_and_removes_consumed_api_cache() {
        use std::io::{Read, Seek, SeekFrom, Write};

        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let token_dir = root.path().join("123:test-token");
        fs::create_dir(&token_dir).await.unwrap();
        let source_path = token_dir.join("large.mkv");
        let size = 21 * 1024 * 1024;
        let mut source = std::fs::File::create(&source_path).unwrap();
        source.write_all(b"head").unwrap();
        source.set_len(size).unwrap();
        source.seek(SeekFrom::End(-4)).unwrap();
        source.write_all(b"tail").unwrap();
        drop(source);

        let response = format!(
            r#"{{"ok":true,"result":{{"file_id":"test-file","file_unique_id":"test-unique","file_size":{size},"file_path":"{}"}}}}"#,
            source_path
                .to_str()
                .unwrap()
                .replace('\\', "\\\\")
                .replace('"', "\\\""),
        );
        let (bot, server) = mock_api(vec![("/bot123:test-token/getfile", response.into_bytes())]);
        let output = download_file(
            &bot,
            "test-file",
            size,
            size,
            directory.path(),
            &DownloadConfig::new(
                &bot.api_url(),
                Some(root.path().to_path_buf()),
                "123:test-token",
            )
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(fs::metadata(&output).await.unwrap().len(), size);
        assert!(
            !source_path.exists(),
            "consumed Local Bot API cache file must be deleted"
        );
        let mut copied = std::fs::File::open(&output).unwrap();
        let mut bytes = [0; 4];
        copied.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"head");
        copied.seek(SeekFrom::End(-4)).unwrap();
        copied.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"tail");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn downloads_relative_file_from_the_same_api_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let metadata = br#"{"ok":true,"result":{"file_id":"test-file","file_unique_id":"test-unique","file_size":7,"file_path":"videos/source.mkv"}}"#;
        let (bot, server) = mock_api(vec![
            ("/bot123:test-token/getfile", metadata.to_vec()),
            (
                "/file/bot123:test-token/videos/source.mkv",
                b"content".to_vec(),
            ),
        ]);
        let output = download_file(
            &bot,
            "test-file",
            7,
            7,
            directory.path(),
            &DownloadConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(fs::read(output).await.unwrap(), b"content");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn local_copy_enforces_cap_and_cleans_partial_output() {
        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = root.path().join("source.mkv");
        fs::write(&source, b"12345").await.unwrap();
        let error = materialize_file(
            &test_bot(),
            source.to_str().unwrap(),
            4,
            directory.path(),
            &local_config(root.path()).await,
        )
        .await
        .unwrap_err();
        assert!(error.downcast_ref::<FileTooLargeError>().is_some());
        assert!(!directory.path().join("input.mkv").exists());
        assert!(source.exists());

        // The copy itself must enforce the cap even if a file grows after metadata.
        let mut input = &b"12345"[..];
        let mut output = Vec::new();
        let error = copy_limited(&mut input, &mut output, 4).await.unwrap_err();
        assert!(error.downcast_ref::<FileTooLargeError>().is_some());
        assert!(output.len() <= 4);
    }

    #[tokio::test]
    async fn local_paths_must_stay_within_the_configured_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("outside.mkv");
        fs::write(&source, b"data").await.unwrap();
        let canonical_root = fs::canonicalize(root.path()).await.unwrap();
        assert!(open_local_file(&canonical_root, &source).await.is_err());
        assert!(open_local_file(&canonical_root, Path::new("relative.mkv"))
            .await
            .is_err());
        assert!(open_local_file(&canonical_root, &canonical_root)
            .await
            .is_err());

        #[cfg(unix)]
        {
            let link = root.path().join("escape.mkv");
            std::os::unix::fs::symlink(&source, &link).unwrap();
            assert!(open_local_file(&canonical_root, &link).await.is_err());
        }
    }

    #[tokio::test]
    async fn absolute_paths_require_explicit_local_configuration() {
        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = root.path().join("source.mkv");
        fs::write(&source, b"data").await.unwrap();
        assert!(materialize_file(
            &test_bot(),
            source.to_str().unwrap(),
            4,
            directory.path(),
            &DownloadConfig::default(),
        )
        .await
        .is_err());
        assert!(!directory.path().join("input.mkv").exists());
    }

    #[test]
    fn extracts_extension_from_path() {
        assert_eq!(extract_extension("videos/source.mkv"), "mkv");
    }

    #[test]
    fn falls_back_to_bin_without_extension() {
        assert_eq!(extract_extension("videos/source"), "bin");
    }

    #[test]
    fn ignores_dotted_directories_when_file_has_no_extension() {
        assert_eq!(extract_extension("videos.v1/source"), "bin");
    }

    #[test]
    fn extracts_extension_from_basename_with_dotted_directories() {
        assert_eq!(extract_extension("videos.v1/source.mkv"), "mkv");
    }

    #[test]
    fn rejects_unsafe_extension() {
        assert_eq!(extract_extension("videos/source.mp4;rm"), "bin");
    }

    #[test]
    fn recognizes_telegram_file_too_big_error() {
        let error = RequestError::Api(ApiError::Unknown(
            "Bad Request: file is too big".to_string(),
        ));
        assert!(is_telegram_file_too_large(&error));
        assert!(is_telegram_file_too_large(&RequestError::Api(
            ApiError::RequestEntityTooLarge,
        )));
    }

    #[test]
    fn does_not_classify_unrelated_telegram_error_as_too_large() {
        let error = RequestError::Api(ApiError::Unknown(
            "Bad Request: invalid file_id".to_string(),
        ));
        assert!(!is_telegram_file_too_large(&error));
    }
}
