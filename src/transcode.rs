use std::{path::Path, process::Stdio};

use axum::{
    body::Body,
    http::{HeaderValue, header},
    response::Response,
};
use tokio::process::Command;
use tokio_util::io::ReaderStream;

// Formats Sonum can transcode into on the fly, via ffmpeg, for clients (mostly browsers) with limited support for the source format.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscodeFormat {
    Mp3,
    Opus,
    Aac,
}

impl TranscodeFormat {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "mp3" => Some(Self::Mp3),
            "opus" => Some(Self::Opus),
            "aac" => Some(Self::Aac),
            _ => None,
        }
    }

    fn container(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus => "ogg",
            Self::Aac => "adts",
        }
    }

    fn codec(self) -> &'static str {
        match self {
            Self::Mp3 => "libmp3lame",
            Self::Opus => "libopus",
            Self::Aac => "aac",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Opus => "audio/ogg",
            Self::Aac => "audio/aac",
        }
    }
}

const MIN_BITRATE_KBPS: u32 = 64;
const MAX_BITRATE_KBPS: u32 = 320;
const DEFAULT_BITRATE_KBPS: u32 = 192;

pub(crate) fn clamp_bitrate(requested: Option<u32>) -> u32 {
    requested
        .unwrap_or(DEFAULT_BITRATE_KBPS)
        .clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
}

pub(crate) fn detect_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub(crate) async fn transcode(
    source_path: &Path,
    format: TranscodeFormat,
    bitrate_kbps: Option<u32>,
    seek_seconds: Option<f64>,
) -> anyhow::Result<Response> {
    let bitrate = clamp_bitrate(bitrate_kbps);

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    if let Some(secs) = seek_seconds.filter(|s| *s > 0.0) {
        cmd.args(["-ss", &format!("{secs:.3}")]);
    }
    cmd.arg("-i").arg(source_path).args([
        "-map",
        "0:a:0", // audio only skip embedded cover art video streams
        "-f",
        format.container(),
        "-c:a",
        format.codec(),
        "-b:a",
        &format!("{bitrate}k"),
        "-",
    ]);

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("ffmpeg started without a stdout pipe"))?;

    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    let body = Body::from_stream(ReaderStream::new(stdout));

    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );

    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    Ok(response)
}

pub(crate) fn byte_range_to_seek_seconds(start_byte: u64, bitrate_kbps: u32) -> f64 {
    let bytes_per_sec = (bitrate_kbps as f64 * 1000.0) / 8.0;
    if bytes_per_sec <= 0.0 {
        return 0.0;
    }
    start_byte as f64 / bytes_per_sec
}

pub(crate) fn parse_range_start(header_value: &str) -> Option<u64> {
    let spec = header_value.trim().strip_prefix("bytes=")?;
    let first = spec.split(',').next()?.trim();
    let (start, _end) = first.split_once('-')?;
    start.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_formats_case_insensitively() {
        assert_eq!(TranscodeFormat::parse("mp3"), Some(TranscodeFormat::Mp3));
        assert_eq!(TranscodeFormat::parse("MP3"), Some(TranscodeFormat::Mp3));
        assert_eq!(TranscodeFormat::parse("Opus"), Some(TranscodeFormat::Opus));
        assert_eq!(TranscodeFormat::parse("aac"), Some(TranscodeFormat::Aac));
    }

    #[test]
    fn rejects_unknown_formats() {
        assert_eq!(TranscodeFormat::parse("flac"), None);
        assert_eq!(TranscodeFormat::parse(""), None);
        assert_eq!(TranscodeFormat::parse("mp4"), None);
    }

    #[test]
    fn clamps_bitrate_into_range() {
        assert_eq!(clamp_bitrate(Some(16)), MIN_BITRATE_KBPS);
        assert_eq!(clamp_bitrate(Some(9999)), MAX_BITRATE_KBPS);
        assert_eq!(clamp_bitrate(Some(192)), 192);
        assert_eq!(clamp_bitrate(None), DEFAULT_BITRATE_KBPS);
    }

    #[test]
    fn parses_simple_range_start() {
        assert_eq!(parse_range_start("bytes=1234-"), Some(1234));
        assert_eq!(parse_range_start("bytes=0-999"), Some(0));
    }

    #[test]
    fn parses_first_span_of_multi_range() {
        assert_eq!(parse_range_start("bytes=500-599,700-799"), Some(500));
    }

    #[test]
    fn rejects_malformed_range_headers() {
        assert_eq!(parse_range_start("not-a-range"), None);
        assert_eq!(parse_range_start("bytes=abc-"), None);
        assert_eq!(parse_range_start(""), None);
    }

    #[test]
    fn seek_seconds_scale_with_bitrate() {
        // 192 kbps -> 24,000 bytes/sec
        assert_eq!(byte_range_to_seek_seconds(24_000, 192), 1.0);
        assert_eq!(byte_range_to_seek_seconds(0, 192), 0.0);
        assert_eq!(byte_range_to_seek_seconds(1_000, 0), 0.0);
    }
}
