// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use gettextrs::gettext;

use crate::probe::MediaInfo;
use media_core::text::{self, Message};

/// Interlaced share above which the file is deinterlaced (spec §4.1; the DVD rips measured for
/// spec §10 sit at 0.6 % while carrying an "interlaced, top field first" flag, so the flag alone
/// decides nothing).
const INTERLACED_THRESHOLD: f64 = 0.30;

/// Share of repeated fields that makes telecine likely (spec §4.1). v1 only warns.
const TELECINE_THRESHOLD: f64 = 0.15;

/// Share the most frequent crop rectangle needs before it is used (spec §4.2).
const CROP_MODE_SHARE: f64 = 0.50;

/// Below this many removed lines and columns cropping is not worth a filter (spec §4.2).
const CROP_MINIMUM_REMOVED: u32 = 8;

/// How far two measured rectangles may sit apart and still be the same bar (spec §4.2).
///
/// cropdetect reports the edge of an MPEG-2 letterbox a pixel or two differently from sample to
/// sample - ringing around the bar makes the first non-black row move. Counted as separate
/// rectangles the vote splits: on Film E one bar arrived as 702:434:10:70 (37 %),
/// 702:436:10:70 (35 %), 700:434:10:72 (17 %) and 702:432:10:70 (8 %), so nothing reached
/// CROP_MODE_SHARE and a quarter of the frame was encoded black. Together they are 97 %
/// (spec §10). Four rather than two: the widest spread measured is two, and a threshold with no
/// slack over its own measurement is a threshold that holds until the next disc.
const CROP_CLUSTER_TOLERANCE: u32 = 4;

/// cropdetect needs about ten frames to settle, so the first outputs of every sample are dropped
/// (spec §4.2).
const CROP_WARMUP_LINES: usize = 10;

/// cropdetect rounds the rectangle it reports to a multiple of `round`, taking the difference off
/// the picture, and its default of 16 therefore throws away up to 15 rows and columns of image:
/// measured, `round=16` gives Film H `720:416:0:80` where the picture really occupies
/// rows 76 to 499, so four rows of film are cut at each end. `round=2` reports the true rectangle
/// and still satisfies yuv420p, which only needs even numbers - and it costs no stability, the mode
/// holding the same share on all three reference files (spec §4.2, §10.2).
const CROPDETECT: &str = "cropdetect=round=2";

const SAMPLE_POINTS: usize = 5;
const IDET_FRAMES: u32 = 1000;
const CROP_FRAMES: u32 = 250;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FieldCounts {
    pub top_field_first: u64,
    pub bottom_field_first: u64,
    pub progressive: u64,
    pub undetermined: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RepeatedFields {
    pub neither: u64,
    pub top: u64,
    pub bottom: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdetLine {
    MultiFrame(FieldCounts),
    Repeated(RepeatedFields),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Deinterlace {
    #[default]
    None,
    Bwdif,
    /// Inverse telecine: 3:2 pulldown taken back out, 29.97i to 23.976p.
    Ivtc,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Crop {
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
}

impl fmt::Display for Crop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "crop={}:{}:{}:{}",
            self.width, self.height, self.x, self.y
        )
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CropChoice {
    pub crop: Option<Crop>,
    /// How much of all measurements the chosen - or rejected - rectangle holds.
    pub share: f64,
    /// Why there is no crop; empty when there is one.
    pub reason: &'static str,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Analysis {
    pub deinterlace: Deinterlace,
    pub interlaced_ratio: f64,
    pub fields: FieldCounts,
    pub repeats: RepeatedFields,
    pub crop: CropChoice,
    pub warnings: Vec<Message>,
}

/// Read one of idet's two summary lines. ffmpeg writes them to *stderr*, and the multi-frame line
/// is the one to trust - the single-frame one calls a still frame interlaced far too readily.
pub fn parse_idet_line(line: &str) -> Option<IdetLine> {
    if line.contains("Multi frame detection:") {
        return Some(IdetLine::MultiFrame(FieldCounts {
            top_field_first: labelled_number(line, "TFF:")?,
            bottom_field_first: labelled_number(line, "BFF:")?,
            progressive: labelled_number(line, "Progressive:")?,
            undetermined: labelled_number(line, "Undetermined:").unwrap_or_default(),
        }));
    }

    if line.contains("Repeated Fields:") {
        return Some(IdetLine::Repeated(RepeatedFields {
            neither: labelled_number(line, "Neither:")?,
            top: labelled_number(line, "Top:")?,
            bottom: labelled_number(line, "Bottom:")?,
        }));
    }

    None
}

fn labelled_number(line: &str, label: &str) -> Option<u64> {
    line.split_once(label)?
        .1
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Read the `crop=W:H:X:Y` at the end of a cropdetect line.
pub fn parse_cropdetect_line(line: &str) -> Option<Crop> {
    let mut values = line.rsplit_once("crop=")?.1.trim().split(':');
    let mut next = || values.next()?.parse().ok();

    Some(Crop {
        width: next()?,
        height: next()?,
        x: next()?,
        y: next()?,
    })
}

pub fn interlaced_ratio(fields: &FieldCounts) -> f64 {
    let interlaced = fields.top_field_first + fields.bottom_field_first;
    let decided = interlaced + fields.progressive;

    if decided == 0 {
        return 0.0;
    }

    interlaced as f64 / decided as f64
}

pub fn needs_deinterlace(fields: &FieldCounts) -> bool {
    interlaced_ratio(fields) >= INTERLACED_THRESHOLD
}

pub fn telecine_suspected(repeats: &RepeatedFields) -> bool {
    let frames = repeats.neither + repeats.top + repeats.bottom;

    frames > 0 && (repeats.top + repeats.bottom) as f64 / frames as f64 >= TELECINE_THRESHOLD
}

/// `r_frame_rate` as a number; ffprobe writes it as a fraction, `30000/1001`.
pub fn parse_frame_rate(rate: &str) -> Option<f64> {
    let (numerator, denominator) = rate.split_once('/')?;
    let numerator: f64 = numerator.trim().parse().ok()?;
    let denominator: f64 = denominator.trim().parse().ok()?;

    if denominator == 0.0 {
        return None;
    }

    Some(numerator / denominator)
}

fn is_ntsc(frame_rate: Option<f64>) -> bool {
    frame_rate.is_some_and(|rate| (rate - 30000.0 / 1001.0).abs() < 0.1)
}

/// What to do about fields, if anything. Repeated fields come before the interlaced share, because
/// telecined material reads as fully interlaced (measured: TFF 1001 of 1001 with 40 % repeats) and
/// bwdif would halve its vertical resolution instead of rebuilding the original frames.
pub fn choose_deinterlace(
    fields: &FieldCounts,
    repeats: &RepeatedFields,
    frame_rate: Option<f64>,
) -> Deinterlace {
    if telecine_suspected(repeats) && is_ntsc(frame_rate) {
        return Deinterlace::Ivtc;
    }

    if needs_deinterlace(fields) {
        return Deinterlace::Bwdif;
    }

    Deinterlace::None
}

/// Whether two rectangles are the same bar measured twice, rather than two different ones.
fn same_bar(one: &Crop, other: &Crop) -> bool {
    one.width.abs_diff(other.width) <= CROP_CLUSTER_TOLERANCE
        && one.height.abs_diff(other.height) <= CROP_CLUSTER_TOLERANCE
        && one.x.abs_diff(other.x) <= CROP_CLUSTER_TOLERANCE
        && one.y.abs_diff(other.y) <= CROP_CLUSTER_TOLERANCE
}

/// The rectangle the samples agree on, or nothing and the reason for it. The most frequent
/// rectangle wins, never the last line: dark scenes produce narrow rectangles right up to the end
/// of a sample (spec §10, Film A: 720:416:0:80 x392 against 672:416:44:82 x93).
///
/// Rectangles within CROP_CLUSTER_TOLERANCE of each other are counted as one, because they are one.
/// The largest of them is what survives, so a pixel of jitter is left on the picture rather than
/// taken out of it.
pub fn choose_crop(samples: &[Crop], width: u32, height: u32) -> CropChoice {
    if samples.is_empty() {
        return CropChoice {
            reason: "cropdetect produced no usable measurement",
            ..CropChoice::default()
        };
    }

    let mut counts: HashMap<Crop, usize> = HashMap::new();

    for crop in samples {
        *counts.entry(*crop).or_default() += 1;
    }

    let mut ranked: Vec<_> = counts
        .keys()
        .map(|candidate| {
            let members = counts
                .iter()
                .filter(|(other, _)| same_bar(candidate, other));
            let total: usize = members.clone().map(|(_, count)| count).sum();
            let widest = members
                .max_by_key(|(crop, count)| (crop.width * crop.height, *count))
                .map_or(*candidate, |(crop, _)| *crop);

            (widest, total)
        })
        .collect();

    ranked.sort_by_key(|(crop, count)| (*count, crop.width * crop.height));

    let (mode, count) = ranked.pop().unwrap_or_default();
    let share = count as f64 / samples.len() as f64;

    let reason = if share < CROP_MODE_SHARE {
        "the sampled crop rectangles disagree"
    } else if mode.width % 2 != 0 || mode.height % 2 != 0 || mode.x % 2 != 0 || mode.y % 2 != 0 {
        "the crop rectangle is not even on all sides"
    } else if (width - mode.width.min(width)) + (height - mode.height.min(height))
        < CROP_MINIMUM_REMOVED
    {
        "there is nothing worth cropping"
    } else {
        return CropChoice {
            crop: Some(mode),
            share,
            reason: "",
        };
    };

    CropChoice {
        crop: None,
        share,
        reason,
    }
}

/// The same reasons for the window. A `match` over literals rather than `gettext(reason)`, because
/// extraction is static and a variable carries nothing into the catalogue.
fn crop_reason_shown(reason: &str) -> String {
    match reason {
        "cropdetect produced no usable measurement" => {
            gettext("cropdetect produced no usable measurement")
        }
        "the sampled crop rectangles disagree" => gettext("the sampled crop rectangles disagree"),
        "the crop rectangle is not even on all sides" => {
            gettext("the crop rectangle is not even on all sides")
        }
        "there is nothing worth cropping" => gettext("there is nothing worth cropping"),
        other => other.to_string(),
    }
}

/// Where to sample: spread between 10 % and 90 % of the duration, away from intro logos and
/// credits (spec §4).
pub fn sample_points(duration: f64, count: usize) -> Vec<f64> {
    if duration <= 0.0 || count == 0 {
        return Vec::new();
    }

    if count == 1 {
        return vec![duration * 0.5];
    }

    (0..count)
        .map(|index| duration * (0.10 + 0.80 * index as f64 / (count - 1) as f64))
        .collect()
}

pub fn analyze(file: &Path, info: &MediaInfo, verbose: bool) -> Result<Analysis> {
    let video = info
        .video
        .as_ref()
        .context("analyzing a file without a video stream")?;

    let mut analysis = Analysis::default();
    let mut crops: Vec<Crop> = Vec::new();

    for at in sample_points(info.duration, SAMPLE_POINTS) {
        let idet = sample(file, at, "idet", IDET_FRAMES, verbose)?;

        for line in idet.lines() {
            match parse_idet_line(line) {
                Some(IdetLine::MultiFrame(counts)) => {
                    analysis.fields.top_field_first += counts.top_field_first;
                    analysis.fields.bottom_field_first += counts.bottom_field_first;
                    analysis.fields.progressive += counts.progressive;
                    analysis.fields.undetermined += counts.undetermined;
                }
                Some(IdetLine::Repeated(counts)) => {
                    analysis.repeats.neither += counts.neither;
                    analysis.repeats.top += counts.top;
                    analysis.repeats.bottom += counts.bottom;
                }
                None => {}
            }
        }

        let cropdetect = sample(file, at, CROPDETECT, CROP_FRAMES, verbose)?;

        crops.extend(
            cropdetect
                .lines()
                .filter_map(parse_cropdetect_line)
                .skip(CROP_WARMUP_LINES),
        );
    }

    let decided = analysis.fields.top_field_first
        + analysis.fields.bottom_field_first
        + analysis.fields.progressive;

    if decided == 0 {
        analysis.warnings.push(Message::new(
            "idet reported nothing, assuming progressive",
            gettext("idet reported nothing, assuming progressive"),
        ));
    }

    let frame_rate = parse_frame_rate(&video.frame_rate);

    analysis.interlaced_ratio = interlaced_ratio(&analysis.fields);
    analysis.deinterlace = choose_deinterlace(&analysis.fields, &analysis.repeats, frame_rate);

    if telecine_suspected(&analysis.repeats) && analysis.deinterlace != Deinterlace::Ivtc {
        analysis.warnings.push(Message::filled(
            "repeated fields at {rate} fps: telecine outside NTSC is not handled",
            gettext("repeated fields at {rate} fps: telecine outside NTSC is not handled"),
            &[("rate", &format!("{:.3}", frame_rate.unwrap_or_default()))],
        ));
    }

    analysis.crop = choose_crop(&crops, video.width, video.height);

    if analysis.crop.crop.is_none() && !analysis.crop.reason.is_empty() {
        analysis.warnings.push(Message {
            logged: format!(
                "no crop: {} ({:.0} % of {} measurements)",
                analysis.crop.reason,
                analysis.crop.share * 100.0,
                crops.len()
            ),
            shown: text::fill(
                &gettext("no crop: {reason} ({share} % of {count} measurements)"),
                &[
                    ("reason", &crop_reason_shown(analysis.crop.reason)),
                    ("share", &format!("{:.0}", analysis.crop.share * 100.0)),
                    ("count", &crops.len().to_string()),
                ],
            ),
        });
    }

    Ok(analysis)
}

/// Run one analysis filter over a stretch of the file and hand back ffmpeg's stderr, where idet and
/// cropdetect write their results. A sample that fails - a seek past the end, a broken stretch -
/// yields no lines rather than an error; the decision is made from the samples that worked.
fn sample(file: &Path, at: f64, filter: &str, frames: u32, verbose: bool) -> Result<String> {
    let seek = format!("{at:.3}");
    let frames = frames.to_string();
    let mut command = Command::new("ffmpeg");

    command
        .args(["-nostdin", "-hide_banner", "-ss", &seek, "-i"])
        .arg(file)
        .args([
            "-vf",
            filter,
            "-frames:v",
            &frames,
            "-an",
            "-f",
            "null",
            "-",
        ]);

    if verbose {
        eprintln!(
            "ffmpeg -nostdin -hide_banner -ss {seek} -i {} -vf {filter} -frames:v {frames} -an -f null -",
            file.display()
        );
    }

    let output = command
        .output()
        .with_context(|| format!("running ffmpeg's {filter} on {}", file.display()))?;

    Ok(String::from_utf8_lossy(&output.stderr).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTI_FRAME: &str = "[Parsed_idet_0 @ 0x55] Multi frame detection: TFF:   105 BFF:    11 \
                               Progressive:  1859 Undetermined:    26";
    const REPEATED: &str =
        "[Parsed_idet_0 @ 0x55] Repeated Fields: Neither:  1993 Top:     0 Bottom:     8";
    const CROPDETECT: &str = "[Parsed_cropdetect_0 @ 0x55] x1:0 x2:719 y1:80 y2:495 w:720 h:416 \
                              x:0 y:80 pts:80 t:0.080000 limit:0.094118 crop=720:416:0:80";

    fn film_a() -> FieldCounts {
        match parse_idet_line(MULTI_FRAME) {
            Some(IdetLine::MultiFrame(counts)) => counts,
            other => panic!("expected a multi-frame line, got {other:?}"),
        }
    }

    #[test]
    fn reads_the_multi_frame_line() {
        assert_eq!(
            film_a(),
            FieldCounts {
                top_field_first: 105,
                bottom_field_first: 11,
                progressive: 1859,
                undetermined: 26,
            }
        );
    }

    #[test]
    fn reads_the_repeated_fields_line() {
        assert_eq!(
            parse_idet_line(REPEATED),
            Some(IdetLine::Repeated(RepeatedFields {
                neither: 1993,
                top: 0,
                bottom: 8,
            }))
        );
    }

    #[test]
    fn ignores_the_single_frame_line_and_anything_else() {
        let single = "[Parsed_idet_0 @ 0x55] Single frame detection: TFF: 32 BFF: 33 \
                      Progressive: 36 Undetermined: 0";

        assert_eq!(parse_idet_line(single), None);
        assert_eq!(parse_idet_line("frame= 100 fps=0.0 q=-0.0"), None);
        assert_eq!(parse_idet_line(""), None);
    }

    #[test]
    fn film_a_is_not_deinterlaced() {
        assert!(interlaced_ratio(&film_a()) < 0.07);
        assert!(!needs_deinterlace(&film_a()));
    }

    #[test]
    fn a_truly_interlaced_source_is_deinterlaced() {
        let counts = FieldCounts {
            top_field_first: 1600,
            bottom_field_first: 20,
            progressive: 300,
            undetermined: 80,
        };

        assert!(needs_deinterlace(&counts));
    }

    #[test]
    fn nothing_decided_means_no_deinterlace() {
        assert_eq!(interlaced_ratio(&FieldCounts::default()), 0.0);
        assert!(!needs_deinterlace(&FieldCounts::default()));
    }

    #[test]
    fn telecine_is_only_suspected_above_the_threshold() {
        assert!(!telecine_suspected(&RepeatedFields {
            neither: 1993,
            top: 0,
            bottom: 8,
        }));

        assert!(telecine_suspected(&RepeatedFields {
            neither: 1600,
            top: 200,
            bottom: 200,
        }));

        assert!(!telecine_suspected(&RepeatedFields::default()));
    }

    #[test]
    fn reads_the_frame_rate_fraction() {
        assert_eq!(parse_frame_rate("25/1"), Some(25.0));
        assert!((parse_frame_rate("30000/1001").unwrap_or_default() - 29.97).abs() < 0.01);
        assert_eq!(parse_frame_rate("0/0"), None);
        assert_eq!(parse_frame_rate("25"), None);
    }

    /// The three sources measured for spec §10.1, each 1000 frames of idet.
    #[test]
    fn telecined_ntsc_gets_inverse_telecine() {
        let fields = FieldCounts {
            top_field_first: 1001,
            progressive: 0,
            ..FieldCounts::default()
        };
        let repeats = RepeatedFields {
            neither: 601,
            top: 200,
            bottom: 200,
        };

        assert_eq!(
            choose_deinterlace(&fields, &repeats, parse_frame_rate("30000/1001")),
            Deinterlace::Ivtc
        );
    }

    #[test]
    fn truly_interlaced_pal_gets_bwdif_not_inverse_telecine() {
        let fields = FieldCounts {
            top_field_first: 1001,
            progressive: 0,
            ..FieldCounts::default()
        };
        let repeats = RepeatedFields {
            neither: 1001,
            top: 0,
            bottom: 0,
        };

        assert_eq!(
            choose_deinterlace(&fields, &repeats, parse_frame_rate("25/1")),
            Deinterlace::Bwdif
        );
    }

    #[test]
    fn repeated_fields_outside_ntsc_do_not_trigger_inverse_telecine() {
        let fields = FieldCounts {
            top_field_first: 1001,
            progressive: 0,
            ..FieldCounts::default()
        };
        let repeats = RepeatedFields {
            neither: 601,
            top: 200,
            bottom: 200,
        };

        assert_eq!(
            choose_deinterlace(&fields, &repeats, parse_frame_rate("25/1")),
            Deinterlace::Bwdif
        );
    }

    #[test]
    fn a_progressive_film_is_left_alone() {
        let repeats = RepeatedFields {
            neither: 1993,
            top: 0,
            bottom: 8,
        };

        assert_eq!(
            choose_deinterlace(&film_a(), &repeats, parse_frame_rate("25/1")),
            Deinterlace::None
        );
    }

    #[test]
    fn reads_the_crop_rectangle_at_the_end_of_the_line() {
        assert_eq!(
            parse_cropdetect_line(CROPDETECT),
            Some(Crop {
                width: 720,
                height: 416,
                x: 0,
                y: 80,
            })
        );

        assert_eq!(parse_cropdetect_line("no rectangle here"), None);
        assert_eq!(parse_cropdetect_line("crop=720:416:0"), None);
    }

    fn repeat(crop: Crop, times: usize) -> Vec<Crop> {
        vec![crop; times]
    }

    #[test]
    fn the_frequent_rectangle_wins_over_the_dark_scenes() {
        let letterbox = Crop {
            width: 720,
            height: 416,
            x: 0,
            y: 80,
        };
        let dark = Crop {
            width: 672,
            height: 416,
            x: 44,
            y: 82,
        };

        let mut samples = repeat(letterbox, 392);
        samples.extend(repeat(dark, 93));

        let choice = choose_crop(&samples, 720, 576);

        assert_eq!(choice.crop, Some(letterbox));
        assert!(choice.share > 0.80);
    }

    #[test]
    fn the_full_frame_is_not_cropped() {
        let full = Crop {
            width: 720,
            height: 576,
            x: 0,
            y: 0,
        };
        let barely = Crop {
            width: 720,
            height: 560,
            x: 0,
            y: 8,
        };

        let mut samples = repeat(full, 457);
        samples.extend(repeat(barely, 21));

        assert_eq!(choose_crop(&samples, 720, 576).crop, None);
    }

    #[test]
    fn a_rectangle_removing_less_than_eight_lines_is_not_worth_it() {
        let nearly_full = Crop {
            width: 720,
            height: 572,
            x: 0,
            y: 2,
        };

        let choice = choose_crop(&repeat(nearly_full, 100), 720, 576);

        assert_eq!(choice.crop, None);
        assert_eq!(choice.reason, "there is nothing worth cropping");
    }

    /// Measured on Film E (spec §10): one letterbox, reported as four rectangles two
    /// pixels apart. Counted exactly no single one reaches 50 %, and a quarter of the frame stays
    /// black; counted as the one bar they are, they are 97 %.
    #[test]
    fn a_letterbox_that_jitters_by_two_pixels_is_still_one_letterbox() {
        let taller = Crop {
            width: 702,
            height: 436,
            x: 10,
            y: 70,
        };
        let mut samples = repeat(
            Crop {
                width: 702,
                height: 434,
                x: 10,
                y: 70,
            },
            440,
        );
        samples.extend(repeat(taller, 420));
        samples.extend(repeat(
            Crop {
                width: 700,
                height: 434,
                x: 10,
                y: 72,
            },
            208,
        ));
        samples.extend(repeat(
            Crop {
                width: 702,
                height: 432,
                x: 10,
                y: 70,
            },
            96,
        ));
        samples.extend(repeat(
            Crop {
                width: 692,
                height: 434,
                x: 10,
                y: 72,
            },
            20,
        ));
        samples.extend(repeat(
            Crop {
                width: 690,
                height: 434,
                x: 10,
                y: 72,
            },
            11,
        ));

        let choice = choose_crop(&samples, 720, 576);

        // The widest and tallest of the four, so the jitter is never taken out of the picture.
        assert_eq!(choice.crop, Some(taller));
        assert!(
            choice.share > 0.95,
            "the four belong to one bar, share was {}",
            choice.share
        );
    }

    #[test]
    fn scattered_rectangles_are_refused() {
        let samples: Vec<Crop> = (0..10)
            .map(|index| Crop {
                width: 720 - index * 16,
                height: 416,
                x: index * 8,
                y: 80,
            })
            .collect();

        let choice = choose_crop(&samples, 720, 576);

        assert_eq!(choice.crop, None);
        assert_eq!(choice.reason, "the sampled crop rectangles disagree");
    }

    #[test]
    fn an_odd_rectangle_is_refused() {
        let odd = Crop {
            width: 719,
            height: 415,
            x: 0,
            y: 80,
        };

        assert_eq!(
            choose_crop(&repeat(odd, 100), 720, 576).reason,
            "the crop rectangle is not even on all sides"
        );
    }

    #[test]
    fn without_measurements_there_is_no_crop() {
        let choice = choose_crop(&[], 720, 576);

        assert_eq!(choice.crop, None);
        assert_eq!(choice.reason, "cropdetect produced no usable measurement");
    }

    #[test]
    fn samples_stay_between_ten_and_ninety_percent() {
        let points = sample_points(6000.0, 5);

        assert_eq!(points.len(), 5);
        assert!((points[0] - 600.0).abs() < 0.001);
        assert!((points[4] - 5400.0).abs() < 0.001);
        assert!(points.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn a_file_without_a_duration_is_not_sampled() {
        assert!(sample_points(0.0, 5).is_empty());
        assert!(sample_points(-1.0, 5).is_empty());
        assert_eq!(sample_points(100.0, 1), vec![50.0]);
    }
}
