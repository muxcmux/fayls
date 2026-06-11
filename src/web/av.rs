use std::{path::Path, process::Stdio};

use salvo::{Request, Response, fs::NamedFile};
use serde::Deserialize;
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::io::ReaderStream;

use crate::{
    config,
    error::{AppResult, Error},
    web::AuthorizedPath,
};

const VIDEO_FILE_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "mpeg", "mpg", "ts", "m2ts", "3gp",
    "ogv", "hevc",
];
const AUDIO_FILE_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "aac", "flac", "wav", "ogg", "oga", "opus", "wma", "aiff", "aif", "alac", "m4p",
];
const HLS_VIDEO_CHUNK_DURATION: f64 = 6.0;
const HLS_AUDIO_CHUNK_DURATION: f64 = 80.0;

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

pub(crate) fn is_media_file_extension(ext: &str) -> bool {
    is_video_file_extension(ext) || is_audio_file_extension(ext)
}

enum HlsKind {
    Master,
    Audio(Option<u64>),
    Video(Option<u64>),
}

struct HlsRequest {
    route: String,
    path: String,
    kind: HlsKind,
}

#[derive(Default, Deserialize)]
struct FfprobeOutput {
    format: Option<FfprobeFormat>,
    streams: Option<Vec<FfprobeStream>>,
}

#[derive(Default, Deserialize)]
struct FfprobeStream {
    codec_name: Option<String>,
    codec_type: Option<String>,
}

#[derive(Default, Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
    format_name: Option<String>,
}

struct Metadata {
    duration: f64,
    format_names: Vec<String>,
    streams: Vec<FfprobeStream>,
}

impl Metadata {
    fn contains_audio_stream(&self) -> bool {
        self.streams
            .iter()
            .any(|stream| stream.codec_type.as_ref().is_some_and(|ct| ct == "audio"))
    }

    fn contains_video_stream(&self) -> bool {
        self.streams
            .iter()
            .any(|stream| stream.codec_type.as_ref().is_some_and(|ct| ct == "video"))
    }

    fn can_stream_directly(&self, ext: &str) -> bool {
        if is_video_file_extension(ext) && self.contains_video_stream() {
            return self.is_directly_streamable_video();
        }

        if is_audio_file_extension(ext)
            || (self.contains_audio_stream() && !self.contains_video_stream())
        {
            return self.is_directly_streamable_audio();
        }

        false
    }

    fn is_directly_streamable_video(&self) -> bool {
        let video_streams: Vec<_> = self
            .streams
            .iter()
            .filter(|stream| stream.codec_type.as_deref() == Some("video"))
            .collect();

        self.format_names.iter().any(|name| name == "mp4")
            && !video_streams.is_empty()
            && video_streams
                .iter()
                .all(|stream| is_video_codec_directly_streamable(stream.codec_name.as_deref()))
            && self
                .streams
                .iter()
                .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
                .all(|stream| is_audio_codec_directly_streamable(stream.codec_name.as_deref()))
    }

    fn is_directly_streamable_audio(&self) -> bool {
        self.format_names
            .iter()
            .any(|name| matches!(name.as_str(), "mp3" | "mp4" | "flac" | "ogg" | "wav"))
            && self
                .streams
                .iter()
                .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
                .any(|stream| is_audio_codec_directly_streamable(stream.codec_name.as_deref()))
            && self
                .streams
                .iter()
                .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
                .all(|stream| is_audio_codec_directly_streamable(stream.codec_name.as_deref()))
    }
}

fn is_video_codec_directly_streamable(codec_name: Option<&str>) -> bool {
    matches!(codec_name, Some("h264" | "av1"))
}

fn is_audio_codec_directly_streamable(codec_name: Option<&str>) -> bool {
    codec_name.is_some_and(|codec_name| {
        codec_name.starts_with("pcm_")
            || matches!(codec_name, "mp3" | "aac" | "vorbis" | "opus" | "flac")
    })
}

fn hls_request(req: &Request) -> AppResult<HlsRequest> {
    Ok(HlsRequest {
        route: req.uri().path().to_string(),
        path: req
            .query::<String>("path")
            .ok_or(Error::BadRequest("no path param"))?,
        kind: req
            .query::<String>("hls")
            .map_or(HlsKind::Master, |v| match v.as_ref() {
                "master" => HlsKind::Master,
                "video" => HlsKind::Video(req.query::<u64>("segment")),
                _ => HlsKind::Audio(req.query::<u64>("segment")),
            }),
    })
}

pub(crate) async fn should_stream_directly(path: impl AsRef<Path>) -> AppResult<bool> {
    let path = path.as_ref();
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return Ok(false);
    };

    Ok(probe(path)
        .await?
        .can_stream_directly(&ext.to_ascii_lowercase()))
}

pub(crate) async fn preview_media_file(
    path: &AuthorizedPath,
    req: &Request,
    res: &mut Response,
) -> AppResult {
    if req.query::<&str>("hls").is_none() && should_stream_directly(path.as_ref()).await? {
        return stream_direct_media_file(path, req, res).await;
    }

    let hls = hls_request(req)?;

    match hls.kind {
        HlsKind::Master => {
            render_hls_playlist(res, master_video_playlist(path, &hls).await?);
            Ok(())
        }
        HlsKind::Video(segment) => {
            if let Some(segment) = segment {
                stream_video_segment(path, segment, res).await
            } else {
                render_hls_playlist(res, segmented_playlist(path, &hls).await?);
                Ok(())
            }
        }
        HlsKind::Audio(segment) => {
            if let Some(segment) = segment {
                stream_audio_segment(path, segment, res).await
            } else {
                render_hls_playlist(res, segmented_playlist(path, &hls).await?);
                Ok(())
            }
        }
    }
}

async fn stream_direct_media_file(
    path: &AuthorizedPath,
    req: &Request,
    res: &mut Response,
) -> AppResult {
    NamedFile::builder(path.as_ref())
        .disposition_type("inline")
        .send(req.headers(), res)
        .await;
    Ok(())
}

async fn probe(path: impl AsRef<Path>) -> AppResult<Metadata> {
    let path = path.as_ref();
    let output = Command::new(&config::get().preview.ffprobe_bin)
        .arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_entries")
        .arg("format=duration,format_name:stream=codec_type,codec_name")
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
        .ok_or_else(|| anyhow::anyhow!("ffprobe did not return a media duration"))?;

    if duration <= 0.0 {
        return Err(anyhow::anyhow!("media duration must be greater than zero").into());
    }

    Ok(Metadata {
        duration,
        format_names: output
            .format
            .and_then(|format| format.format_name)
            .unwrap_or_default()
            .split(',')
            .map(str::to_owned)
            .collect(),
        streams: output.streams.unwrap_or(vec![]),
    })
}

fn parse_duration(value: Option<&str>) -> Option<f64> {
    value
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration > 0.0)
}

async fn master_video_playlist(
    path: &AuthorizedPath,
    hls_request: &HlsRequest,
) -> AppResult<String> {
    let mut list = String::new();
    list.push_str("#EXTM3U\n");
    list.push_str("#EXT-X-VERSION:6\n");
    list.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
    list.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");

    let video_playlist_url = format!(
        "{}?path={}&hls=video\n",
        hls_request.route, hls_request.path
    );

    if probe(path).await?.contains_audio_stream() {
        let audio_playlist_url =
            format!("{}?path={}&hls=audio", hls_request.route, hls_request.path);
        let audio_playlist_entry = format!(
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"default\",DEFAULT=YES,AUTOSELECT=YES,URI=\"{}\"\n",
            &audio_playlist_url
        );
        list.push_str(&audio_playlist_entry);
        list.push_str("#EXT-X-STREAM-INF:BANDWIDTH=2500000,CODECS=\"avc1.64001f,mp4a.40.2\",AUDIO=\"audio\"\n");
    } else {
        list.push_str("#EXT-X-STREAM-INF:BANDWIDTH=2500000,CODECS=\"avc1.64001f\"\n");
    }

    list.push_str(&video_playlist_url);
    Ok(list)
}

async fn segmented_playlist(path: &AuthorizedPath, hls_request: &HlsRequest) -> AppResult<String> {
    let total_duration = probe(path).await?.duration;
    let (av, chunk_duration) = match hls_request.kind {
        HlsKind::Audio(_) => ("audio", HLS_AUDIO_CHUNK_DURATION),
        _ => ("video", HLS_VIDEO_CHUNK_DURATION),
    };

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    let segments = (total_duration / chunk_duration).ceil() as u64;

    let mut lines = vec![
        "#EXTM3U".into(),
        "#EXT-X-VERSION:3".into(),
        "#EXT-X-MEDIA-SEQUENCE:0".into(),
        "#EXT-X-ALLOW-CACHE:YES".into(),
        "#EXT-X-PLAYLIST-TYPE:VOD".into(),
        format!("#EXT-X-TARGETDURATION:{}", chunk_duration),
    ];

    for segment in 0..segments {
        let duration = segment_duration(total_duration, chunk_duration, segment)?;
        lines.push(format!("#EXTINF:{duration:.4}, nodesc"));
        lines.push(format!(
            "{}?path={}&hls={}&segment={}",
            hls_request.route, hls_request.path, av, segment
        ));
    }

    lines.push("#EXT-X-ENDLIST".into());

    Ok(lines.join("\n"))
}

fn render_hls_playlist(res: &mut Response, playlist: String) {
    add_streaming_headers(res, "application/vnd.apple.mpegurl");
    res.body(playlist);
}

async fn stream_video_segment(
    path: &AuthorizedPath,
    segment: u64,
    res: &mut Response,
) -> AppResult {
    let total_duration = probe(path).await?.duration;
    let start = segment_start(total_duration, HLS_VIDEO_CHUNK_DURATION, segment)?;
    let duration = segment_duration(total_duration, HLS_VIDEO_CHUNK_DURATION, segment)?;

    let mut command = ffmpeg_base_command(start, duration, path);
    command
        .arg("-an")
        .arg("-sn")
        .arg("-force_key_frames")
        .arg(format!("expr:gte(t,n_forced*{HLS_VIDEO_CHUNK_DURATION})"))
        .arg("-fps_mode")
        .arg("cfr")
        .arg("-output_ts_offset")
        .arg(format!("{start}"))
        .arg("-c:v");

    match config::get().preview.encoder {
        config::Encoder::Cpu => {
            command
                .arg("libx264")
                .arg("-preset")
                .arg("veryfast")
                .arg("-vf")
                .arg(format!("scale=-2:{},format=yuv420p", config::get().preview.max_video_height))
                .arg("-x264opts")
                .arg("subme=0:me_range=4:rc_lookahead=10:me=dia:no_chroma_me:8x8dct=0:partitions=none");
        }
        config::Encoder::Vaapi => {
            command
                .arg("h264_vaapi")
                .arg("-vf")
                .arg(format!(
                    "format=nv12,hwupload,scale_vaapi=w=-2:h={}",
                    config::get().preview.max_video_height
                ))
                .arg("-init_hw_device")
                .arg("vaapi=va:/dev/dri/renderD128")
                .arg("-filter_hw_device")
                .arg("va");
        }
        config::Encoder::Nvenc => {
            command
                .arg("h264_nvenc")
                .arg("-vf")
                .arg(format!(
                    "format=nv12,hwupload_cuda,scale_cuda=w=-2:h={}",
                    config::get().preview.max_video_height
                ))
                .arg("-init_hw_device")
                .arg("cuda=hw")
                .arg("-filter_hw_device")
                .arg("hw");
        }
        config::Encoder::V4l => {
            command
                .arg("h264_v4l2m2m")
                .arg("-vf")
                .arg(format!(
                    "scale=-2:{},format=yuv420p",
                    config::get().preview.max_video_height
                ))
                .arg("-b:v")
                .arg("2500k")
                .arg("-num_output_buffers")
                .arg("32")
                .arg("-num_capture_buffers")
                .arg("32");
        }
    }

    command.arg("-f").arg("mpegts").arg("pipe:1");

    stream_ffmpeg_stdout(command, res)
}

async fn stream_audio_segment(
    path: &AuthorizedPath,
    segment: u64,
    res: &mut Response,
) -> AppResult {
    let total_duration = probe(path).await?.duration;
    let start = segment_start(total_duration, HLS_AUDIO_CHUNK_DURATION, segment)?;
    let duration = segment_duration(total_duration, HLS_AUDIO_CHUNK_DURATION, segment)?;

    let mut command = ffmpeg_base_command(start, duration, path);
    command
        .arg("-vn")
        .arg("-sn")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg(format!("{}k", config::get().preview.audio_bitrate_kbps))
        .arg("-output_ts_offset")
        .arg(format!("{start}"))
        .arg("-f")
        .arg("mpegts")
        .arg("pipe:1");

    stream_ffmpeg_stdout(command, res)
}

fn ffmpeg_base_command(start: f64, duration: f64, path: &AuthorizedPath) -> Command {
    let mut command = Command::new(&config::get().preview.ffmpeg_bin);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin")
        .arg("-timelimit")
        .arg("30")
        .arg("-ss")
        .arg(format!("{start:.3}"))
        .arg("-t")
        .arg(format!("{duration:.3}"))
        .arg("-i")
        .arg(path.as_ref());
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

fn segment_start(total_duration: f64, chunk_duration: f64, segment: u64) -> AppResult<f64> {
    #[allow(clippy::cast_precision_loss)]
    let start = segment as f64 * chunk_duration;
    if start >= total_duration {
        return Err(Error::NotFound);
    }
    Ok(start)
}

fn segment_duration(total_duration: f64, chunk_duration: f64, segment: u64) -> AppResult<f64> {
    let start = segment_start(total_duration, chunk_duration, segment)?;
    Ok((total_duration - start).min(chunk_duration))
}
