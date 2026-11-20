use anyhow::{anyhow, Context, Result as AnyResult};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionMode {
    Remux,
    Transcode,
}

#[derive(Debug)]
struct MediaStreams {
    subtitle_indices: Vec<usize>,
    duration_seconds: Option<f64>,
}

fn build_output_path(file_path: &str) -> PathBuf {
    let path = Path::new(file_path);
    let mut output_name = path
        .file_stem()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "video".into());
    output_name.push(".converted.mp4");
    path.with_file_name(output_name)
}

fn probe_media(file_path: &str) -> AnyResult<MediaStreams> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
            file_path,
        ])
        .output()
        .context("Could not start FFprobe")?;
    if !output.status.success() {
        return Err(anyhow!("FFprobe could not inspect the input video"));
    }
    let json: Value = serde_json::from_slice(&output.stdout)
        .context("FFprobe returned invalid stream information")?;
    let streams = json
        .get("streams")
        .and_then(Value::as_array)
        .context("FFprobe found no media streams")?;

    let mut has_video = false;
    let mut subtitle_indices = Vec::new();
    for stream in streams {
        let index = stream.get("index").and_then(Value::as_u64);
        let codec_type = stream.get("codec_type").and_then(Value::as_str);
        let codec_name = stream.get("codec_name").and_then(Value::as_str).unwrap_or("");
        match codec_type {
            Some("video") => has_video = true,
            // MP4 supports text subtitles through mov_text. Bitmap subtitles
            // (PGS/DVD) cannot be represented faithfully in a normal MP4 file.
            Some("subtitle")
                if matches!(
                    codec_name,
                    "subrip" | "srt" | "ass" | "ssa" | "webvtt" | "mov_text" | "text"
                ) =>
            {
                if let Some(index) = index {
                    subtitle_indices.push(index as usize);
                }
            }
            _ => {}
        }
    }
    if !has_video {
        return Err(anyhow!("The supplied file has no video stream"));
    }
    let duration_seconds = json
        .get("format")
        .and_then(|format| format.get("duration"))
        .and_then(Value::as_str)
        .and_then(|duration| duration.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration > 0.0);
    Ok(MediaStreams {
        subtitle_indices,
        duration_seconds,
    })
}

fn run_ffmpeg(
    file_path: &str,
    output_path: &Path,
    streams: &MediaStreams,
    mode: ConversionMode,
    max_output_bytes: u64,
) -> AnyResult<()> {
    // Recreate output for every attempt so a failed remux can never be mistaken
    // for a completed transcode.
    let _ = std::fs::remove_file(output_path);
    let mut command = Command::new("ffmpeg");
    command.args([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-i",
        file_path,
        "-map",
        "0:v:0",
        "-map",
        "0:a?",
    ]);
    for index in &streams.subtitle_indices {
        command.arg("-map").arg(format!("0:{index}"));
    }
    command.args(["-map_metadata", "0", "-map_chapters", "0"]);

    match mode {
        ConversionMode::Remux => {
            command.args(["-c", "copy"]);
        }
        ConversionMode::Transcode => {
            command.args([
                "-c:v", "libx264",
                "-preset", "ultrafast",
                "-crf", "18",
                "-pix_fmt", "yuv420p",
                "-threads", "1",
                "-filter_threads", "1",
                "-c:a", "aac",
                "-b:a", "192k",
            ]);
            if !streams.subtitle_indices.is_empty() {
                command.args(["-c:s", "mov_text"]);
            }
        }
    }
    command.args(["-movflags", "+faststart", "-fs"]);
    command.arg(max_output_bytes.to_string());
    command.arg(output_path);
    let output = command.output().context("Could not start FFmpeg")?;
    if !output.status.success() {
        let _ = std::fs::remove_file(output_path);
        return Err(ffmpeg_error(&output));
    }
    let metadata = std::fs::metadata(output_path)
        .with_context(|| format!("FFmpeg did not create {}", output_path.display()))?;
    if metadata.len() == 0 {
        let _ = std::fs::remove_file(output_path);
        return Err(anyhow!("FFmpeg created an empty MP4 file"));
    }
    let output_probe = match probe_media(&output_path.to_string_lossy()) {
        Ok(probe) => probe,
        Err(error) => {
            let _ = std::fs::remove_file(output_path);
            return Err(error).context("FFmpeg output failed MP4 validation");
        }
    };
    if let (Some(input_duration), Some(output_duration)) =
        (streams.duration_seconds, output_probe.duration_seconds)
    {
        let tolerance = 0.5_f64.max(input_duration * 0.01);
        if output_duration + tolerance < input_duration {
            let _ = std::fs::remove_file(output_path);
            return Err(anyhow!(
                "FFmpeg output is shorter than the source; the output-size limit may have been reached"
            ));
        }
    }
    Ok(())
}

fn ffmpeg_error(output: &Output) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.lines().last().unwrap_or("unknown FFmpeg error");
    anyhow!("FFmpeg failed (status {:?}): {}", output.status.code(), detail)
}

/// Try a bit-for-bit stream-copy remux first. If MP4 cannot contain the streams,
/// transcode video/audio to broadly compatible H.264/AAC while retaining text
/// subtitles and global metadata where possible.
pub fn convert_video_to_mp4(
    file_path: &str,
    max_output_bytes: u64,
) -> AnyResult<(String, ConversionMode)> {
    let streams = probe_media(file_path)?;
    let output_path = build_output_path(file_path);

    match run_ffmpeg(
        file_path,
        &output_path,
        &streams,
        ConversionMode::Remux,
        max_output_bytes,
    ) {
        Ok(()) => Ok((output_path.to_string_lossy().into_owned(), ConversionMode::Remux)),
        Err(remux_error) => {
            log::warn!(
                "Lossless MP4 remux was incompatible; trying H.264/AAC transcode: {remux_error}"
            );
            run_ffmpeg(
                file_path,
                &output_path,
                &streams,
                ConversionMode::Transcode,
                max_output_bytes,
            )
                .context("MP4 remux and fallback transcode both failed")?;
            Ok((output_path.to_string_lossy().into_owned(), ConversionMode::Transcode))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_output_path, ConversionMode};

    #[test]
    fn keeps_output_distinct_from_input_with_existing_extension() {
        assert_eq!(
            build_output_path("/tmp/example.mkv"),
            std::path::Path::new("/tmp/example.converted.mp4")
        );
    }

    #[test]
    fn keeps_output_distinct_from_mp4_input() {
        assert_eq!(
            build_output_path("/tmp/example.mp4"),
            std::path::Path::new("/tmp/example.converted.mp4")
        );
    }

    #[test]
    fn conversion_modes_are_distinct() {
        assert_ne!(ConversionMode::Remux, ConversionMode::Transcode);
    }
}
