// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VideoStream {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub sample_aspect_ratio: String,
    pub frame_rate: String,
    pub field_order: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct MediaInfo {
    pub video: Option<VideoStream>,
    /// One entry per audio stream, in order, as ffprobe reports its language tag. A stream without
    /// one is `und`, which is what Matroska calls "undetermined".
    pub audio_languages: Vec<String>,
    /// The same, one entry per subtitle stream.
    pub subtitle_languages: Vec<String>,
    pub duration: f64,
}

impl MediaInfo {
    pub fn audio_streams(&self) -> usize {
        self.audio_languages.len()
    }

    pub fn subtitle_streams(&self) -> usize {
        self.subtitle_languages.len()
    }
}

const ENTRIES: &str = "stream=codec_type,codec_name,width,height,sample_aspect_ratio,\
                       r_frame_rate,field_order:stream_tags=language:format=duration";

pub fn probe(file: &Path) -> Result<MediaInfo> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", ENTRIES])
        .args(["-of", "json"])
        .arg(file)
        .output()
        .with_context(|| format!("running ffprobe on {}", file.display()))?;

    if !output.status.success() {
        bail!(
            "ffprobe failed on {}: {}",
            file.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    parse_ffprobe(&String::from_utf8_lossy(&output.stdout))
        .with_context(|| format!("reading what ffprobe reported about {}", file.display()))
}

/// ffprobe's JSON: `width` and `height` arrive as numbers, everything else as a string - including
/// `duration`, and including `sample_aspect_ratio`, which is a ratio like `64:45`. A stream that
/// carries no metadata has no `tags` object at all.
#[derive(Deserialize)]
struct Probed {
    #[serde(default)]
    streams: Vec<ProbedStream>,
    #[serde(default)]
    format: ProbedFormat,
}

#[derive(Deserialize, Default)]
struct ProbedFormat {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct ProbedStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    sample_aspect_ratio: Option<String>,
    r_frame_rate: Option<String>,
    field_order: Option<String>,
    #[serde(default)]
    tags: ProbedTags,
}

#[derive(Deserialize, Default)]
struct ProbedTags {
    language: Option<String>,
}

/// Read `ffprobe -of json`: the first video stream, the language of every audio and subtitle
/// stream, and the duration of the container.
pub fn parse_ffprobe(text: &str) -> Result<MediaInfo> {
    let probed: Probed = serde_json::from_str(text).context("parsing ffprobe's JSON output")?;

    let mut info = MediaInfo {
        // A container that reports no duration says `N/A`, which is as usable as a missing one.
        duration: probed
            .format
            .duration
            .and_then(|duration| duration.parse().ok())
            .unwrap_or_default(),
        ..MediaInfo::default()
    };

    for stream in probed.streams {
        match stream.codec_type.as_deref() {
            Some("audio") => info.audio_languages.push(language(stream.tags.language)),
            Some("subtitle") => info.subtitle_languages.push(language(stream.tags.language)),
            Some("video") if info.video.is_none() => {
                info.video = Some(VideoStream {
                    codec: stream.codec_name.unwrap_or_default(),
                    width: stream.width.unwrap_or_default(),
                    height: stream.height.unwrap_or_default(),
                    sample_aspect_ratio: stream.sample_aspect_ratio.unwrap_or_default(),
                    frame_rate: stream.r_frame_rate.unwrap_or_default(),
                    field_order: stream.field_order.unwrap_or_default(),
                });
            }
            _ => {}
        }
    }

    Ok(info)
}

/// A missing language tag is `und`, which is what Matroska calls "undetermined".
fn language(tag: Option<String>) -> String {
    match tag.as_deref() {
        None | Some("") => "und".to_string(),
        Some(language) => language.to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FILM_G/A1_t00.mkv`, spec §10. The video stream carries a language tag of its
    /// own, and the empty `side_data_list` is what ffprobe adds for an mpeg2 stream.
    const DVD_RIP: &str = r#"{
        "streams": [
            {
                "codec_name": "mpeg2video",
                "codec_type": "video",
                "width": 720,
                "height": 576,
                "sample_aspect_ratio": "64:45",
                "field_order": "tt",
                "r_frame_rate": "25/1",
                "tags": { "language": "eng" },
                "side_data_list": [ {} ]
            },
            {
                "codec_name": "ac3",
                "codec_type": "audio",
                "r_frame_rate": "0/0",
                "tags": { "language": "ger" }
            }
        ],
        "format": { "duration": "5471.800000" }
    }"#;

    /// `BLURAY_1/title_t00.mkv`, spec §10, trimmed to the streams that carry the point: the
    /// first subtitle stream reports width and height and the rest do not.
    const BLU_RAY_RIP: &str = r#"{
        "streams": [
            {
                "codec_name": "h264",
                "codec_type": "video",
                "width": 1920,
                "height": 1080,
                "sample_aspect_ratio": "1:1",
                "field_order": "progressive",
                "r_frame_rate": "24000/1001",
                "tags": { "language": "eng" }
            },
            { "codec_name": "dts", "codec_type": "audio", "r_frame_rate": "0/0",
              "tags": { "language": "eng" } },
            { "codec_name": "dts", "codec_type": "audio", "r_frame_rate": "0/0",
              "tags": { "language": "fra" } },
            { "codec_name": "dts", "codec_type": "audio", "r_frame_rate": "0/0",
              "tags": { "language": "deu" } },
            {
                "codec_name": "hdmv_pgs_subtitle",
                "codec_type": "subtitle",
                "width": 1920,
                "height": 1080,
                "r_frame_rate": "0/0",
                "tags": { "language": "eng" }
            },
            { "codec_name": "hdmv_pgs_subtitle", "codec_type": "subtitle", "r_frame_rate": "0/0",
              "tags": { "language": "deu" } },
            { "codec_name": "hdmv_pgs_subtitle", "codec_type": "subtitle", "r_frame_rate": "0/0",
              "tags": { "language": "swe" } }
        ],
        "format": { "duration": "7511.337000" }
    }"#;

    #[test]
    fn reads_the_video_stream() {
        let video = parse_ffprobe(DVD_RIP)
            .expect("valid JSON")
            .video
            .expect("a video stream");

        assert_eq!(video.codec, "mpeg2video");
        assert_eq!((video.width, video.height), (720, 576));
        assert_eq!(video.sample_aspect_ratio, "64:45");
        assert_eq!(video.frame_rate, "25/1");
        assert_eq!(video.field_order, "tt");
    }

    #[test]
    fn counts_every_audio_stream_and_the_duration() {
        let info = parse_ffprobe(DVD_RIP).expect("valid JSON");

        assert_eq!(info.audio_streams(), 1);
        assert_eq!(info.audio_languages, vec!["ger"]);
        assert!((info.duration - 5471.8).abs() < 0.001);
    }

    /// The video stream has a language tag of its own, which belongs to no audio track.
    #[test]
    fn the_video_language_is_not_counted_as_audio() {
        let info = parse_ffprobe(DVD_RIP).expect("valid JSON");

        assert_eq!(info.audio_languages, vec!["ger"]);
        assert_eq!(info.subtitle_streams(), 0);
    }

    #[test]
    fn keeps_audio_and_subtitles_apart_across_many_streams() {
        let info = parse_ffprobe(BLU_RAY_RIP).expect("valid JSON");

        assert_eq!(info.audio_languages, vec!["eng", "fra", "deu"]);
        assert_eq!(info.subtitle_languages, vec!["eng", "deu", "swe"]);
        assert_eq!(
            info.video.map(|video| (video.codec, video.width)),
            Some(("h264".to_string(), 1920))
        );
    }

    #[test]
    fn a_stream_without_a_language_tag_is_undetermined() {
        let info = parse_ffprobe(r#"{"streams":[{"codec_type":"audio","codec_name":"ac3"}]}"#)
            .expect("valid JSON");

        assert_eq!(info.audio_languages, vec!["und"]);
    }

    #[test]
    fn a_file_without_a_video_stream_has_none() {
        let info = parse_ffprobe(
            r#"{"streams":[{"codec_type":"audio","codec_name":"flac"}],
                "format":{"duration":"180.0"}}"#,
        )
        .expect("valid JSON");

        assert_eq!(info.video, None);
        assert_eq!(info.audio_streams(), 1);
    }

    /// A container with no duration reports `N/A`, and ffprobe leaves `format` out entirely when it
    /// has nothing to say about it.
    #[test]
    fn a_duration_that_is_not_a_number_is_zero() {
        let info =
            parse_ffprobe(r#"{"streams":[],"format":{"duration":"N/A"}}"#).expect("valid JSON");

        assert_eq!(info.duration, 0.0);
        assert_eq!(
            parse_ffprobe(r#"{"streams":[]}"#).expect("valid JSON"),
            info
        );
    }

    /// ffprobe exits 0 having written nothing often enough that this must not read as "no streams".
    #[test]
    fn empty_output_is_an_error_not_an_empty_file() {
        assert!(parse_ffprobe("").is_err());
        assert!(parse_ffprobe("Segmentation fault").is_err());
    }
}
