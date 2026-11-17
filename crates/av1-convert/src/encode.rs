// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs;

use anyhow::{Context, Result, bail};
use media_core::ffmpeg::{self, Job, Outcome};

use crate::plan::Plan;
use crate::probe::{self, MediaInfo};

/// How far the output's duration may sit from the input's before the job counts as failed (spec §6).
const DURATION_TOLERANCE: f64 = 1.0;

/// Run the encode into the partial file. Progress is reported as whole percent, once per change;
/// the callback answers `false` to stop the encode, which is what the progress dialog's Cancel
/// button comes back as.
pub fn run(plan: &Plan, on_progress: &mut dyn FnMut(u8) -> bool) -> Result<Outcome> {
    let arguments = plan.ffmpeg_args();

    ffmpeg::run(
        &Job {
            input: &plan.input,
            partial: &plan.partial,
            arguments: &arguments,
            duration: plan.duration,
        },
        on_progress,
    )
}

/// Check the encoded partial against the source and, only if it holds up, put it in place. An
/// exit code of 0 is not enough: ffmpeg returns it for a file that stops after two minutes.
pub fn finish(plan: &Plan) -> Result<u64> {
    let produced = probe::probe(&plan.partial)
        .with_context(|| format!("probing the encoded file for {}", plan.input.display()))?;

    if let Err(error) = verify(plan, &produced) {
        ffmpeg::discard_partial();

        return Err(error);
    }

    ffmpeg::place(&plan.partial, &plan.output)?;

    let size = fs::metadata(&plan.output)
        .with_context(|| format!("reading the size of {}", plan.output.display()))?
        .len();

    Ok(size)
}

pub fn verify(plan: &Plan, produced: &MediaInfo) -> Result<()> {
    if produced.video.is_none() {
        bail!("the encoded file has no video stream");
    }

    let drift = (produced.duration - plan.duration).abs();

    if drift > DURATION_TOLERANCE {
        bail!(
            "the encoded file is {drift:.1} s off: {:.1} s in, {:.1} s out",
            plan.duration,
            produced.duration
        );
    }

    if produced.audio_streams() != plan.audio_streams {
        bail!(
            "the encoded file has {} audio streams instead of {}",
            produced.audio_streams(),
            plan.audio_streams
        );
    }

    if produced.subtitle_streams() != plan.subtitle_streams {
        bail!(
            "the encoded file has {} subtitle streams instead of {}",
            produced.subtitle_streams(),
            plan.subtitle_streams
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::probe::VideoStream;

    fn plan() -> Plan {
        Plan {
            input: PathBuf::from("/films/A1_t00.mkv"),
            output: PathBuf::from("/films/A1_t00.av1.mkv"),
            partial: PathBuf::from("/films/A1_t00.av1.mkv.partial"),
            deinterlace: crate::analyze::Deinterlace::None,
            crop: None,
            cq: 28,
            audio: crate::plan::AudioMode::Copy,
            ten_bit: false,
            audio_tracks: None,
            subtitle_tracks: None,
            duration: 5189.8,
            audio_streams: 1,
            subtitle_streams: 0,
        }
    }

    fn produced(duration: f64, audio_streams: usize) -> MediaInfo {
        MediaInfo {
            video: Some(VideoStream {
                codec: "av1".to_string(),
                width: 688,
                height: 544,
                ..VideoStream::default()
            }),
            audio_languages: vec!["ger".to_string(); audio_streams],
            subtitle_languages: Vec::new(),
            duration,
        }
    }

    #[test]
    fn an_encode_of_the_same_length_with_the_same_audio_passes() {
        assert!(verify(&plan(), &produced(5189.3, 1)).is_ok());
    }

    #[test]
    fn a_truncated_encode_fails_even_though_ffmpeg_was_happy() {
        let error = verify(&plan(), &produced(120.0, 1))
            .unwrap_err()
            .to_string();

        assert!(error.contains("5069.8 s off"), "{error}");
    }

    #[test]
    fn a_lost_audio_stream_fails() {
        let error = verify(&plan(), &produced(5189.8, 0))
            .unwrap_err()
            .to_string();

        assert!(error.contains("0 audio streams instead of 1"), "{error}");
    }

    #[test]
    fn a_file_without_video_fails() {
        let empty = MediaInfo {
            duration: 5189.8,
            audio_languages: vec!["ger".to_string()],
            subtitle_languages: Vec::new(),
            video: None,
        };

        assert!(verify(&plan(), &empty).is_err());
    }

    #[test]
    fn a_dropped_subtitle_track_fails() {
        let with_subtitles = Plan {
            subtitle_streams: 1,
            ..plan()
        };
        let error = verify(&with_subtitles, &produced(5189.8, 1))
            .unwrap_err()
            .to_string();

        assert!(error.contains("0 subtitle streams instead of 1"), "{error}");
    }
}
