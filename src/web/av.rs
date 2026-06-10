use std::{fmt::Write as _, path::Path, process::Stdio};

use salvo::{Request, Response};
use serde::Deserialize;
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::io::ReaderStream;

use crate::{
    config,
    error::{AppResult, Error},
};

const VIDEO_FILE_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "mpeg", "mpg", "ts", "m2ts", "3gp",
    "ogv", "hevc",
];
const AUDIO_FILE_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "aac", "flac", "wav", "ogg", "oga", "opus", "wma", "aiff", "aif", "alac", "m4p",
];
const HLS_SEGMENT_DURATION: f64 = 6.0;
const HLS_SEGMENT_TARGET_DURATION: u64 = 6;

pub(crate) fn is_video_file_extension(ext: &str) -> bool {
    VIDEO_FILE_EXTENSIONS
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
}

pub(crate) fn is_audio_file_extension(ext: &str) -> bool {
    AUDIO_FILE_EXTENSIONS
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
}

pub(crate) async fn preview_video_file(
    path: &Path,
    req: &Request,
    res: &mut Response,
) -> AppResult {
    let route_path = req.uri().path().to_string();
    // unwrap is fine here, because we should have already
    // serialized the passed path from the same query param
    let path_string = req.query::<String>("path").unwrap();

    let hls = req
        .query::<String>("hls")
        .unwrap_or_else(|| "master".to_string());

    match hls.as_str() {
        "master" => {
            let metadata = probe_media(path).await?;
            let playlist = video_master_playlist(&metadata, &route_path, &path_string);
            render_hls_playlist(res, playlist);
        }
        "variant" => {
            let variant = req
                .query::<String>("variant")
                .ok_or(Error::BadRequest("no variant param"))?;
            let metadata = probe_media(path).await?;
            let variant = video_variant_by_name(&metadata, &variant).ok_or(Error::NotFound)?;
            let playlist = media_variant_playlist(
                metadata.duration,
                &route_path,
                &path_string,
                variant.config.name,
            )?;
            render_hls_playlist(res, playlist);
        }
        "segment" => {
            let variant = req
                .query::<String>("variant")
                .ok_or(Error::BadRequest("no variant param"))?;
            let segment = req
                .query::<u64>("segment")
                .ok_or(Error::BadRequest("no segment param"))?;
            let metadata = probe_media(path).await?;
            let variant = video_variant_by_name(&metadata, &variant).ok_or(Error::NotFound)?;
            stream_video_segment(path, &metadata, &variant, segment, res)?;
        }
        _ => return Err(Error::BadRequest("unknown hls request")),
    }

    Ok(())
}

pub(crate) async fn preview_audio_file(
    path: &Path,
    req: &Request,
    res: &mut Response,
) -> AppResult {
    let hls = req
        .query::<String>("hls")
        .unwrap_or_else(|| "master".to_string());

    let route_path = req.uri().path().to_string();
    // unwrap is fine here, because we should have already
    // serialized the passed path from the same query param
    let path_string = req.query::<String>("path").unwrap();

    match hls.as_str() {
        "master" => {
            let playlist = audio_master_playlist(&route_path, &path_string);
            render_hls_playlist(res, playlist);
        }
        "variant" => {
            let variant = req
                .query::<String>("variant")
                .ok_or(Error::BadRequest("no variant param"))?;
            let metadata = probe_media(path).await?;
            let variant = audio_variant_by_name(&variant).ok_or(Error::NotFound)?;
            let playlist =
                media_variant_playlist(metadata.duration, &route_path, &path_string, variant.name)?;
            render_hls_playlist(res, playlist);
        }
        "segment" => {
            let variant = req
                .query::<String>("variant")
                .ok_or(Error::BadRequest("no variant param"))?;
            let segment = req
                .query::<u64>("segment")
                .ok_or(Error::BadRequest("no segment param"))?;
            let metadata = probe_media(path).await?;
            let variant = audio_variant_by_name(&variant).ok_or(Error::NotFound)?;
            stream_audio_segment(path, &metadata, &variant, segment, res)?;
        }
        _ => return Err(Error::BadRequest("unknown hls request")),
    }

    Ok(())
}

#[derive(Clone, Copy)]
struct VideoVariant {
    name: &'static str,
    max_height: u32,
    video_bitrate_kbps: u32,
    audio_bitrate_kbps: u32,
}

#[derive(Clone, Copy)]
struct ResolvedVideoVariant {
    config: VideoVariant,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy)]
struct AudioVariant {
    name: &'static str,
    audio_bitrate_kbps: u32,
}

struct MediaMetadata {
    duration: f64,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Default, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Default, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<String>,
}

#[derive(Default, Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
}

const VIDEO_VARIANTS: &[VideoVariant] = &[
    VideoVariant {
        name: "240p",
        max_height: 240,
        video_bitrate_kbps: 400,
        audio_bitrate_kbps: 64,
    },
    VideoVariant {
        name: "360p",
        max_height: 360,
        video_bitrate_kbps: 800,
        audio_bitrate_kbps: 96,
    },
    VideoVariant {
        name: "480p",
        max_height: 480,
        video_bitrate_kbps: 1400,
        audio_bitrate_kbps: 128,
    },
    VideoVariant {
        name: "720p",
        max_height: 720,
        video_bitrate_kbps: 2800,
        audio_bitrate_kbps: 128,
    },
    VideoVariant {
        name: "1080p",
        max_height: 1080,
        video_bitrate_kbps: 5000,
        audio_bitrate_kbps: 192,
    },
];

const AUDIO_VARIANTS: &[AudioVariant] = &[
    AudioVariant {
        name: "64k",
        audio_bitrate_kbps: 64,
    },
    AudioVariant {
        name: "128k",
        audio_bitrate_kbps: 128,
    },
    AudioVariant {
        name: "192k",
        audio_bitrate_kbps: 192,
    },
];

async fn probe_media(path: &Path) -> AppResult<MediaMetadata> {
    let output = Command::new(&config::get().app.ffprobe_bin)
        .arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_entries")
        .arg("format=duration:stream=codec_type,width,height,duration")
        .arg(path)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to run ffprobe: {e}"))?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "ffprobe failed for {} with status {}",
            path.display(),
            output.status
        )
        .into());
    }

    let output: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("failed to parse ffprobe output: {e}"))?;

    let duration = output
        .format
        .as_ref()
        .and_then(|format| parse_duration(format.duration.as_deref()))
        .or_else(|| {
            output
                .streams
                .iter()
                .filter_map(|stream| parse_duration(stream.duration.as_deref()))
                .max_by(f64::total_cmp)
        })
        .ok_or_else(|| anyhow::anyhow!("ffprobe did not return a media duration"))?;

    if duration <= 0.0 {
        return Err(anyhow::anyhow!("media duration must be greater than zero").into());
    }

    let video_stream = output
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"));

    Ok(MediaMetadata {
        duration,
        width: video_stream.and_then(|stream| stream.width),
        height: video_stream.and_then(|stream| stream.height),
    })
}

fn parse_duration(value: Option<&str>) -> Option<f64> {
    value
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration > 0.0)
}

fn video_master_playlist(metadata: &MediaMetadata, route: &str, path: &str) -> String {
    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-INDEPENDENT-SEGMENTS\n");

    for variant in resolved_video_variants(metadata) {
        let _ = writeln!(
            playlist,
            "#EXT-X-STREAM-INF:BANDWIDTH={},AVERAGE-BANDWIDTH={},RESOLUTION={}x{},CODECS=\"avc1.4d401f,mp4a.40.2\"",
            video_bandwidth(&variant),
            video_average_bandwidth(&variant),
            variant.width,
            variant.height
        );
        playlist.push_str(&hls_url(
            route,
            path,
            "variant",
            Some(variant.config.name),
            None,
        ));
        playlist.push('\n');
    }

    playlist
}

fn audio_master_playlist(route: &str, path: &str) -> String {
    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-INDEPENDENT-SEGMENTS\n");

    for variant in AUDIO_VARIANTS {
        let bandwidth = variant.audio_bitrate_kbps * 1000;
        let _ = writeln!(
            playlist,
            "#EXT-X-STREAM-INF:BANDWIDTH={},AVERAGE-BANDWIDTH={},CODECS=\"mp4a.40.2\"",
            bandwidth.saturating_mul(12) / 10,
            bandwidth
        );
        playlist.push_str(&hls_url(route, path, "variant", Some(variant.name), None));
        playlist.push('\n');
    }

    playlist
}

fn media_variant_playlist(
    duration: f64,
    route: &str,
    path: &str,
    variant: &str,
) -> AppResult<String> {
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-TARGETDURATION:{HLS_SEGMENT_TARGET_DURATION}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n"
    );

    for segment in 0..segment_count(duration) {
        if segment > 0 {
            playlist.push_str("#EXT-X-DISCONTINUITY\n");
        }
        let segment_duration = segment_duration(duration, segment)?;
        let _ = writeln!(playlist, "#EXTINF:{segment_duration:.3},");
        playlist.push_str(&hls_url(
            route,
            path,
            "segment",
            Some(variant),
            Some(segment),
        ));
        playlist.push('\n');
    }

    playlist.push_str("#EXT-X-ENDLIST\n");
    Ok(playlist)
}

fn hls_url(
    route: &str,
    path: &str,
    hls: &str,
    variant: Option<&str>,
    segment: Option<u64>,
) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("path", path);
    serializer.append_pair("hls", hls);
    if let Some(variant) = variant {
        serializer.append_pair("variant", variant);
    }
    if let Some(segment) = segment {
        serializer.append_pair("segment", &segment.to_string());
    }
    format!("{route}?{}", serializer.finish())
}

fn render_hls_playlist(res: &mut Response, playlist: String) {
    add_streaming_headers(res, "application/vnd.apple.mpegurl");
    res.body(playlist);
}

fn stream_video_segment(
    path: &Path,
    metadata: &MediaMetadata,
    variant: &ResolvedVideoVariant,
    segment: u64,
    res: &mut Response,
) -> AppResult {
    let start = segment_start(metadata.duration, segment)?;
    let duration = segment_duration(metadata.duration, segment)?;
    let maxrate_kbps = variant.config.video_bitrate_kbps.saturating_mul(14) / 10;
    let bufsize_kbps = variant.config.video_bitrate_kbps.saturating_mul(2);
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    let keyframe_interval = (HLS_SEGMENT_DURATION * 30.0).round() as u32;
    let video_filter = video_segment_filter(variant);

    let mut command = ffmpeg_base_command(start, duration, path);
    command
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a:0?")
        .arg("-c:v")
        .arg("libx264")
        .arg("-preset")
        .arg("veryfast")
        .arg("-profile:v")
        .arg("main")
        .arg("-sc_threshold")
        .arg("0")
        .arg("-g")
        .arg(keyframe_interval.to_string())
        .arg("-keyint_min")
        .arg(keyframe_interval.to_string())
        .arg("-force_key_frames")
        .arg(format!("expr:gte(t,n_forced*{HLS_SEGMENT_DURATION})"))
        .arg("-vf")
        .arg(video_filter)
        .arg("-b:v")
        .arg(format!("{}k", variant.config.video_bitrate_kbps))
        .arg("-maxrate")
        .arg(format!("{maxrate_kbps}k"))
        .arg("-bufsize")
        .arg(format!("{bufsize_kbps}k"))
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg(format!("{}k", variant.config.audio_bitrate_kbps))
        .arg("-ac")
        .arg("2")
        .arg("-ar")
        .arg("48000")
        .arg("-f")
        .arg("mpegts")
        .arg("-mpegts_flags")
        .arg("+initial_discontinuity")
        .arg("-muxdelay")
        .arg("0")
        .arg("-muxpreload")
        .arg("0")
        .arg("pipe:1");

    stream_ffmpeg_stdout(command, res)
}

fn video_segment_filter(variant: &ResolvedVideoVariant) -> String {
    format!(
        "scale=w={}:h={}:force_original_aspect_ratio=decrease:force_divisible_by=2,format=yuv420p",
        variant.width, variant.height
    )
}

fn stream_audio_segment(
    path: &Path,
    metadata: &MediaMetadata,
    variant: &AudioVariant,
    segment: u64,
    res: &mut Response,
) -> AppResult {
    let start = segment_start(metadata.duration, segment)?;
    let duration = segment_duration(metadata.duration, segment)?;

    let mut command = ffmpeg_base_command(start, duration, path);
    command
        .arg("-map")
        .arg("0:a:0")
        .arg("-vn")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg(format!("{}k", variant.audio_bitrate_kbps))
        .arg("-ac")
        .arg("2")
        .arg("-ar")
        .arg("48000")
        .arg("-f")
        .arg("mpegts")
        .arg("-mpegts_flags")
        .arg("+initial_discontinuity")
        .arg("-muxdelay")
        .arg("0")
        .arg("-muxpreload")
        .arg("0")
        .arg("pipe:1");

    stream_ffmpeg_stdout(command, res)
}

fn ffmpeg_base_command(start: f64, duration: f64, path: &Path) -> Command {
    let mut command = Command::new(&config::get().app.ffmpeg_bin);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin")
        .arg("-ss")
        .arg(format!("{start:.3}"))
        .arg("-t")
        .arg(format!("{duration:.3}"))
        .arg("-i")
        .arg(path);
    command
}

fn stream_ffmpeg_stdout(mut command: Command, res: &mut Response) -> AppResult {
    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to run ffmpeg: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("ffmpeg stdout was not piped"))?;
    let stderr = child.stderr.take();

    tokio::spawn(async move {
        let stderr = async move {
            let Some(mut stderr) = stderr else {
                return Ok(String::new());
            };

            let mut output = Vec::new();
            stderr.read_to_end(&mut output).await?;
            Ok::<_, std::io::Error>(String::from_utf8_lossy(&output).into_owned())
        };

        let (status, stderr) = tokio::join!(child.wait(), stderr);

        match status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let exit_code = status.code();
                match stderr {
                    Ok(stderr) => {
                        tracing::warn!(?exit_code, ?command, stderr, "ffmpeg exited with an error");
                    }
                    Err(error) => {
                        tracing::warn!(
                            ?exit_code,
                            ?command,
                            ?error,
                            "ffmpeg exited with an error and failed to read stderr"
                        );
                    }
                }
            }
            Err(error) => tracing::warn!(?error, "failed to wait for ffmpeg"),
        }
    });

    add_streaming_headers(res, "video/mp2t");
    res.stream(ReaderStream::new(stdout));
    Ok(())
}

fn add_streaming_headers(res: &mut Response, content_type: &'static str) {
    _ = res.add_header("content-type", content_type, true);
    _ = res.add_header("cache-control", "no-store", true);
    _ = res.add_header("pragma", "no-cache", true);
}

fn segment_count(duration: f64) -> u64 {
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    let count = (duration / HLS_SEGMENT_DURATION).ceil() as u64;
    count
}

fn segment_start(duration: f64, segment: u64) -> AppResult<f64> {
    #[allow(clippy::cast_precision_loss)]
    let start = segment as f64 * HLS_SEGMENT_DURATION;
    if start >= duration {
        return Err(Error::NotFound);
    }
    Ok(start)
}

fn segment_duration(duration: f64, segment: u64) -> AppResult<f64> {
    let start = segment_start(duration, segment)?;
    Ok((duration - start).min(HLS_SEGMENT_DURATION))
}

fn resolved_video_variants(metadata: &MediaMetadata) -> Vec<ResolvedVideoVariant> {
    let source_height = metadata.height.unwrap_or(1080);
    let mut variants: Vec<_> = VIDEO_VARIANTS
        .iter()
        .copied()
        .filter(|variant| variant.max_height <= source_height)
        .map(|variant| resolve_video_variant(metadata, variant))
        .collect();

    if variants.is_empty()
        && let Some(variant) = VIDEO_VARIANTS.first().copied()
    {
        variants.push(resolve_video_variant(metadata, variant));
    }

    variants
}

fn video_variant_by_name(metadata: &MediaMetadata, name: &str) -> Option<ResolvedVideoVariant> {
    resolved_video_variants(metadata)
        .into_iter()
        .find(|variant| variant.config.name == name)
}

fn audio_variant_by_name(name: &str) -> Option<AudioVariant> {
    AUDIO_VARIANTS
        .iter()
        .copied()
        .find(|variant| variant.name == name)
}

fn resolve_video_variant(metadata: &MediaMetadata, variant: VideoVariant) -> ResolvedVideoVariant {
    let height = even_dimension(
        variant
            .max_height
            .min(metadata.height.unwrap_or(variant.max_height)),
    );
    let width = match (metadata.width, metadata.height) {
        (Some(source_width), Some(source_height)) if source_height > 0 => {
            #[allow(clippy::cast_possible_truncation)]
            #[allow(clippy::cast_sign_loss)]
            let dim = ((f64::from(source_width) / f64::from(source_height)) * f64::from(height))
                .round() as u32;
            even_dimension(dim)
        }
        _ => {
            #[allow(clippy::cast_possible_truncation)]
            #[allow(clippy::cast_sign_loss)]
            let dim = ((16.0 / 9.0) * f64::from(height)).round() as u32;
            even_dimension(dim)
        }
    };

    ResolvedVideoVariant {
        config: variant,
        width,
        height,
    }
}

fn even_dimension(value: u32) -> u32 {
    (value.max(2) / 2) * 2
}

fn video_bandwidth(variant: &ResolvedVideoVariant) -> u32 {
    (variant.config.video_bitrate_kbps + variant.config.audio_bitrate_kbps).saturating_mul(1200)
}

fn video_average_bandwidth(variant: &ResolvedVideoVariant) -> u32 {
    (variant.config.video_bitrate_kbps + variant.config.audio_bitrate_kbps).saturating_mul(1000)
}
