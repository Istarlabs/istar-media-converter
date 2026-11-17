// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use gettextrs::gettext;
use media_core::text;

use crate::analyze::{Analysis, Crop, Deinterlace};
use crate::probe::MediaInfo;

/// How many `<stem>.av1.N.mkv` names are tried before giving up.
const NAME_ATTEMPTS: u32 = 99;

/// What spec §11 asks for when DTS at 768 kb/s is transcoded rather than copied.
const OPUS_BITRATE: &str = "256k";

/// ISO 639-2 has two codes for the same language - a bibliographic one and a terminological one -
/// and rips use both: the DVD of Film D says `ger`, the first Blu-ray
/// says `deu`. Only the pairs seen on actual discs are listed; an unknown code simply matches
/// itself.
const LANGUAGE_ALIASES: [(&str, &str); 9] = [
    ("deu", "ger"),
    ("fra", "fre"),
    ("nld", "dut"),
    ("ces", "cze"),
    ("isl", "ice"),
    ("ell", "gre"),
    ("zho", "chi"),
    ("ron", "rum"),
    ("slk", "slo"),
];

/// Everything, as `--audio-langs` spells it.
pub const ALL_LANGUAGES: &str = "all";

/// Nothing, as `--subtitle-langs` spells it.
pub const NO_LANGUAGES: &str = "none";

/// "Follow the audio", the default for `--subtitle-langs`.
pub const AUTO_LANGUAGES: &str = "auto";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioMode {
    /// Keep the original tracks untouched
    #[default]
    Copy,
    /// Transcode to Opus
    Opus,
}

/// What the user gets to decide, as one thing rather than four arguments in a row. clap fills it
/// straight from the command line, and the dialog in `main` overwrites the fields it asked about.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct Settings {
    /// NVENC constant quality
    #[arg(long, default_value_t = 28)]
    pub cq: u8,

    /// What to do with the audio tracks
    #[arg(long, value_enum, default_value_t = AudioMode::Copy)]
    pub audio: AudioMode,

    /// Encode 10 bit instead of 8 (p010le); costs a little size, avoids banding
    #[arg(long = "10bit")]
    pub ten_bit: bool,

    /// Crop even when the file carries subtitles - their bitmaps then sit misplaced
    #[arg(long)]
    pub crop_anyway: bool,

    /// Which audio tracks to keep, as ISO 639-2 codes: "deu,eng", or "all"
    #[arg(long, default_value = ALL_LANGUAGES, value_name = "LANGS")]
    pub audio_langs: String,

    /// Which subtitle tracks to keep: "deu,eng", "all", "none", or "auto" - none once the audio is
    /// down to a single language, all otherwise
    #[arg(long, default_value = AUTO_LANGUAGES, value_name = "LANGS")]
    pub subtitle_langs: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub input: PathBuf,
    pub output: PathBuf,
    pub partial: PathBuf,
    pub deinterlace: Deinterlace,
    pub crop: Option<Crop>,
    pub cq: u8,
    pub audio: AudioMode,
    pub ten_bit: bool,
    /// Which audio streams to map, or `None` for all of them.
    pub audio_tracks: Option<Vec<usize>>,
    /// Which subtitle streams to map, `None` for all and `Some(empty)` for none.
    pub subtitle_tracks: Option<Vec<usize>>,
    pub duration: f64,
    pub audio_streams: usize,
    pub subtitle_streams: usize,
}

impl Plan {
    pub fn new(
        input: &Path,
        info: &MediaInfo,
        analysis: &Analysis,
        settings: &Settings,
    ) -> Result<Plan> {
        let output = output_path(input)?;
        let audio = select_audio(&info.audio_languages, &settings.audio_langs);
        let keeps_everything = audio.tracks.len() == info.audio_streams();
        let subtitles = select_subtitles(
            &info.subtitle_languages,
            &settings.subtitle_langs,
            one_language(&info.audio_languages, &audio),
        );

        Ok(Plan {
            input: input.to_path_buf(),
            partial: partial_path(&output),
            output,
            deinterlace: analysis.deinterlace,
            // The subtitles that are *kept* are the ones a crop would displace, so dropping them
            // gives the crop back.
            crop: crop_for(analysis.crop.crop, subtitles.kept, settings.crop_anyway),
            cq: settings.cq,
            audio: settings.audio,
            ten_bit: settings.ten_bit,
            duration: info.duration,
            audio_streams: audio.tracks.len(),
            subtitle_streams: subtitles.kept,
            subtitle_tracks: subtitles.tracks,
            audio_tracks: if keeps_everything {
                None
            } else {
                Some(audio.tracks)
            },
        })
    }

    /// Deinterlace first, crop second; nothing at all when neither is needed (spec §5).
    pub fn filter_chain(&self) -> Option<String> {
        let mut filters: Vec<String> = Vec::new();

        match self.deinterlace {
            Deinterlace::None => {}
            // send_field, not send_frame: material that measures interlaced came off a sensor and
            // carries 50 real fields a second, so one frame per field keeps the motion the
            // recording has (spec §4.1, measured on the DVD-VR camera rip).
            Deinterlace::Bwdif => {
                filters.push("bwdif=mode=send_field:deint=interlaced".to_string());
            }
            // fieldmatch rebuilds the progressive frames, yadif catches what it could not match,
            // decimate drops the duplicate that 3:2 pulldown added (spec §5.1).
            Deinterlace::Ivtc => {
                filters.push("fieldmatch,yadif=deint=interlaced,decimate".to_string());
            }
        }

        if let Some(crop) = self.crop {
            filters.push(crop.to_string());
        }

        if filters.is_empty() {
            None
        } else {
            Some(filters.join(","))
        }
    }

    pub fn ffmpeg_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["-nostdin".into(), "-hide_banner".into(), "-i".into()];

        args.push(self.input.clone().into_os_string());

        if let Some(chain) = self.filter_chain() {
            args.push("-vf".into());
            args.push(chain.into());
        }

        args.push("-map".into());
        args.push("0:v:0".into());

        match &self.audio_tracks {
            None => {
                args.push("-map".into());
                args.push("0:a?".into());
            }
            Some(tracks) => {
                for track in tracks {
                    args.push("-map".into());
                    args.push(format!("0:a:{track}").into());
                }
            }
        }

        match &self.subtitle_tracks {
            None => {
                args.push("-map".into());
                args.push("0:s?".into());
            }
            Some(tracks) => {
                for track in tracks {
                    args.push("-map".into());
                    args.push(format!("0:s:{track}").into());
                }
            }
        }

        for argument in [
            "-map_chapters",
            "0",
            "-c:v",
            "av1_nvenc",
            "-preset",
            "p7",
            "-tune",
            "hq",
            "-rc",
            "vbr",
            "-cq",
        ] {
            args.push(argument.into());
        }

        args.push(self.cq.to_string().into());

        // Without `-b:v 0` nvenc keeps a default bitrate cap that quietly overrides `-cq`.
        args.push("-b:v".into());
        args.push("0".into());

        if self.ten_bit {
            args.push("-pix_fmt".into());
            args.push("p010le".into());
        }

        match self.audio {
            AudioMode::Copy => {
                args.push("-c:a".into());
                args.push("copy".into());
            }
            // libopus refuses the layout DVD audio arrives in: "Invalid channel layout 5.1(side)".
            // aformat maps it to the nearest layout the encoder does accept.
            AudioMode::Opus => {
                for argument in [
                    "-c:a",
                    "libopus",
                    "-b:a",
                    OPUS_BITRATE,
                    "-af",
                    "aformat=channel_layouts=7.1|5.1|stereo|mono",
                ] {
                    args.push(argument.into());
                }
            }
        }

        // `-f matroska` is what the output file is missing: ffmpeg picks the muxer from the
        // extension, and `.partial` tells it nothing ("Unable to choose an output format").
        for argument in [
            "-c:s",
            "copy",
            "-f",
            "matroska",
            "-progress",
            "pipe:1",
            "-nostats",
        ] {
            args.push(argument.into());
        }

        args.push(self.partial.clone().into_os_string());

        args
    }

    /// The plan in one line, in English: this is what the log file and `--analyze-only` carry, and
    /// both are read next to the source whatever the desktop speaks.
    pub fn summary(&self) -> String {
        self.describe(false)
    }

    /// The same line for the progress window, in the language of the session.
    pub fn summary_shown(&self) -> String {
        self.describe(true)
    }

    fn describe(&self, translate: bool) -> String {
        // Both spellings are needed side by side, so each piece names its English text and its
        // lookup; the `gettext` call has to stay literal here or xgettext never sees the string.
        let pick = |english: &str, translated: String| {
            if translate {
                translated
            } else {
                english.to_string()
            }
        };

        let crop = match self.crop {
            Some(crop) => crop.to_string(),
            None => pick("no crop", gettext("no crop")),
        };

        let fields = match self.deinterlace {
            Deinterlace::None => pick("no deinterlace", gettext("no deinterlace")),
            Deinterlace::Bwdif => pick("deinterlace", gettext("deinterlace")),
            Deinterlace::Ivtc => pick("inverse telecine", gettext("inverse telecine")),
        };

        let audio = match self.audio {
            AudioMode::Copy => pick("audio copied", gettext("audio copied")),
            AudioMode::Opus => pick("audio to Opus", gettext("audio to Opus")),
        };

        let tracks = match &self.audio_tracks {
            None => String::new(),
            Some(tracks) => format!(
                " ({})",
                text::fill(
                    &pick("{count} of them", gettext("{count} of them")),
                    &[("count", &tracks.len().to_string())]
                )
            ),
        };

        let subtitles = match &self.subtitle_tracks {
            None => pick("subtitles copied", gettext("subtitles copied")),
            Some(tracks) if tracks.is_empty() => pick("no subtitles", gettext("no subtitles")),
            Some(tracks) => text::fill(
                &pick(
                    "{count} subtitle track(s)",
                    gettext("{count} subtitle track(s)"),
                ),
                &[("count", &tracks.len().to_string())],
            ),
        };

        let depth = if self.ten_bit {
            format!(" {}", pick("10 bit", gettext("10 bit")))
        } else {
            String::new()
        };

        format!(
            "{fields}, {crop}, cq {}{depth}, {audio}{tracks}, {subtitles}",
            self.cq
        )
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AudioSelection {
    /// Which of the file's audio streams to keep, in order. Never empty.
    pub tracks: Vec<usize>,
    /// Set when nothing matched and everything was kept instead.
    pub fell_back: bool,
}

/// One spelling per language, so `deu` and `ger` in the same file do not count as two.
fn canonical(code: &str) -> &str {
    LANGUAGE_ALIASES
        .iter()
        .find(|(_, bibliographic)| code == *bibliographic)
        .map_or(code, |(terminological, _)| *terminological)
}

/// Do two ISO 639-2 codes mean the same language? See `LANGUAGE_ALIASES`.
fn same_language(left: &str, right: &str) -> bool {
    left == right
        || LANGUAGE_ALIASES
            .iter()
            .any(|(a, b)| (left == *a && right == *b) || (left == *b && right == *a))
}

/// The audio streams to keep. A Blu-ray carries fifteen of them and the output is two thirds audio
/// (measured on Blu-ray 2), so this is where a rip actually shrinks - but a file left without
/// any audio would be worse than a large one, so a wish that matches nothing keeps everything and
/// says so.
pub fn select_audio(languages: &[String], wanted: &str) -> AudioSelection {
    let everything = AudioSelection {
        tracks: (0..languages.len()).collect(),
        fell_back: false,
    };

    let wanted = wanted.trim();

    if wanted.is_empty() || wanted.eq_ignore_ascii_case(ALL_LANGUAGES) {
        return everything;
    }

    let wanted: Vec<String> = wanted
        .split(',')
        .map(|code| code.trim().to_ascii_lowercase())
        .filter(|code| !code.is_empty())
        .collect();

    let tracks: Vec<usize> = languages
        .iter()
        .enumerate()
        .filter(|(_, language)| {
            wanted
                .iter()
                .any(|code| same_language(code, &language.to_ascii_lowercase()))
        })
        .map(|(index, _)| index)
        .collect();

    if tracks.is_empty() {
        return AudioSelection {
            fell_back: true,
            ..everything
        };
    }

    AudioSelection {
        tracks,
        fell_back: false,
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SubtitleSelection {
    /// Which subtitle streams to keep, or `None` for all of them. `Some(empty)` is none at all.
    pub tracks: Option<Vec<usize>>,
    pub kept: usize,
}

/// Which subtitle streams to keep. `auto` is the default and means "follow the audio": once the
/// audio is down to a single language the film is being kept in one language, and subtitles for it
/// are not what the disc carries them for. Keeping none also frees the crop, since it is only the
/// subtitles that block it (§4.2 rule 7).
pub fn select_subtitles(
    languages: &[String],
    wanted: &str,
    audio_is_one_language: bool,
) -> SubtitleSelection {
    let everything = SubtitleSelection {
        tracks: None,
        kept: languages.len(),
    };
    let nothing = SubtitleSelection {
        tracks: Some(Vec::new()),
        kept: 0,
    };

    let wanted = wanted.trim();

    if wanted.eq_ignore_ascii_case(AUTO_LANGUAGES) || wanted.is_empty() {
        return if audio_is_one_language {
            nothing
        } else {
            everything
        };
    }

    if wanted.eq_ignore_ascii_case(ALL_LANGUAGES) {
        return everything;
    }

    if wanted.eq_ignore_ascii_case(NO_LANGUAGES) {
        return nothing;
    }

    let wanted: Vec<String> = wanted
        .split(',')
        .map(|code| code.trim().to_ascii_lowercase())
        .filter(|code| !code.is_empty())
        .collect();

    let tracks: Vec<usize> = languages
        .iter()
        .enumerate()
        .filter(|(_, language)| {
            wanted
                .iter()
                .any(|code| same_language(code, &language.to_ascii_lowercase()))
        })
        .map(|(index, _)| index)
        .collect();

    SubtitleSelection {
        kept: tracks.len(),
        tracks: Some(tracks),
    }
}

/// Is the audio down to one language? Two German tracks still count as one.
pub fn one_language(languages: &[String], selection: &AudioSelection) -> bool {
    if selection.fell_back {
        return false;
    }

    let mut kept: Vec<&str> = selection
        .tracks
        .iter()
        .filter_map(|index| languages.get(*index).map(|code| canonical(code)))
        .collect();

    kept.sort_unstable();
    kept.dedup();

    kept.len() == 1
}

/// The crop that survives the subtitle streams. DVD subtitles are bitmaps placed at absolute
/// coordinates in the uncropped frame, and nothing carries that placement over: measured on Film
/// D, mpv squeezes the subtitle plane into the cropped picture so the line lands
/// on top of the image, and ffmpeg's own renderer cuts it off entirely. The crop is an
/// optimisation, the subtitles are content (spec §4.2).
///
/// `anyway` is the caller saying they know: a track that only translates the closing text cards -
/// nine events in that same film - is not worth 28 % of the pixels across two hours.
pub fn crop_for(detected: Option<Crop>, subtitle_streams: usize, anyway: bool) -> Option<Crop> {
    if subtitle_streams > 0 && !anyway {
        return None;
    }

    detected
}

/// The first free `<stem>.av1.mkv` next to the source. Nothing is ever overwritten, so a name whose
/// partial file is still lying around counts as taken as well.
pub fn output_path(input: &Path) -> Result<PathBuf> {
    for index in 1..=NAME_ATTEMPTS {
        let candidate = numbered(input, index);

        if !candidate.exists() && !partial_path(&candidate).exists() {
            return Ok(candidate);
        }
    }

    bail!(
        "no free output name next to {} after {NAME_ATTEMPTS} attempts",
        input.display()
    );
}

pub fn numbered(input: &Path, index: u32) -> PathBuf {
    let mut name = input
        .file_stem()
        .unwrap_or_else(|| OsStr::new("video"))
        .to_os_string();

    if index <= 1 {
        name.push(".av1.mkv");
    } else {
        name.push(format!(".av1.{index}.mkv"));
    }

    input.with_file_name(name)
}

pub fn partial_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_os_string();

    name.push(".partial");

    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use media_core::ffmpeg::command_line;

    use super::*;

    fn plan() -> Plan {
        Plan {
            input: PathBuf::from("/films/A1_t00.mkv"),
            output: PathBuf::from("/films/A1_t00.av1.mkv"),
            partial: PathBuf::from("/films/A1_t00.av1.mkv.partial"),
            deinterlace: Deinterlace::None,
            crop: None,
            cq: 28,
            audio: AudioMode::Copy,
            ten_bit: false,
            audio_tracks: None,
            subtitle_tracks: None,
            duration: 5189.8,
            audio_streams: 1,
            subtitle_streams: 0,
        }
    }

    fn letterbox() -> Crop {
        Crop {
            width: 688,
            height: 544,
            x: 20,
            y: 16,
        }
    }

    #[test]
    fn neither_filter_means_no_filter_chain() {
        assert_eq!(plan().filter_chain(), None);
    }

    #[test]
    fn deinterlacing_comes_before_cropping() {
        let chain = Plan {
            deinterlace: Deinterlace::Bwdif,
            crop: Some(letterbox()),
            ..plan()
        };

        assert_eq!(
            chain.filter_chain().as_deref(),
            Some("bwdif=mode=send_field:deint=interlaced,crop=688:544:20:16")
        );
    }

    #[test]
    fn inverse_telecine_replaces_the_deinterlacer_and_keeps_the_crop_last() {
        let chain = Plan {
            deinterlace: Deinterlace::Ivtc,
            crop: Some(letterbox()),
            ..plan()
        };

        assert_eq!(
            chain.filter_chain().as_deref(),
            Some("fieldmatch,yadif=deint=interlaced,decimate,crop=688:544:20:16")
        );
    }

    /// The audio languages of Blu-ray 2, in the order the rip carries them.
    fn home_alone() -> Vec<String> {
        [
            "eng", "eng", "fra", "por", "deu", "ita", "jpn", "spa", "rus", "spa", "bul", "ces",
            "hun", "tha", "tur",
        ]
        .iter()
        .map(|code| code.to_string())
        .collect()
    }

    #[test]
    fn keeps_only_the_languages_asked_for() {
        let chosen = select_audio(&home_alone(), "deu,eng");

        assert_eq!(chosen.tracks, vec![0, 1, 4]);
        assert!(!chosen.fell_back);
    }

    #[test]
    fn the_same_language_under_its_other_code_still_matches() {
        let dvd = vec!["ger".to_string(), "eng".to_string()];

        assert_eq!(select_audio(&dvd, "deu").tracks, vec![0]);
        assert_eq!(select_audio(&home_alone(), "ger").tracks, vec![4]);
        assert_eq!(select_audio(&["fre".to_string()], "fra").tracks, vec![0]);
    }

    #[test]
    fn all_means_all_and_so_does_nothing() {
        assert_eq!(select_audio(&home_alone(), "all").tracks.len(), 15);
        assert_eq!(select_audio(&home_alone(), "  ").tracks.len(), 15);
        assert!(!select_audio(&home_alone(), "all").fell_back);
    }

    /// A file with no German track must not come out silent.
    #[test]
    fn a_wish_that_matches_nothing_keeps_everything_and_says_so() {
        let chosen = select_audio(&["eng".to_string(), "fra".to_string()], "deu");

        assert_eq!(chosen.tracks, vec![0, 1]);
        assert!(chosen.fell_back);
    }

    #[test]
    fn the_selection_becomes_one_map_per_track() {
        let line = command_line(
            &Plan {
                audio_tracks: Some(vec![0, 1, 4]),
                ..plan()
            }
            .ffmpeg_args(),
        );

        assert!(
            line.contains("-map 0:v:0 -map 0:a:0 -map 0:a:1 -map 0:a:4 -map 0:s?"),
            "{line}"
        );
        assert!(!line.contains("0:a?"), "{line}");
    }

    #[test]
    fn keeping_everything_stays_one_optional_map() {
        let line = command_line(&plan().ffmpeg_args());

        assert!(line.contains("-map 0:v:0 -map 0:a? -map 0:s?"), "{line}");
    }

    /// The subtitle languages of Blu-ray 1.
    fn bluray_1_subtitles() -> Vec<String> {
        ["eng", "dan", "nld", "fin", "fra", "deu", "nor", "swe"]
            .iter()
            .map(|code| code.to_string())
            .collect()
    }

    #[test]
    fn auto_drops_the_subtitles_once_the_audio_is_one_language() {
        let chosen = select_subtitles(&bluray_1_subtitles(), "auto", true);

        assert_eq!(chosen.tracks, Some(Vec::new()));
        assert_eq!(chosen.kept, 0);
    }

    #[test]
    fn auto_keeps_them_while_the_audio_is_still_several() {
        let chosen = select_subtitles(&bluray_1_subtitles(), "auto", false);

        assert_eq!(chosen.tracks, None);
        assert_eq!(chosen.kept, 8);
    }

    #[test]
    fn subtitles_can_be_named_all_or_none_outright() {
        assert_eq!(
            select_subtitles(&bluray_1_subtitles(), "none", false).kept,
            0
        );
        assert_eq!(select_subtitles(&bluray_1_subtitles(), "all", true).kept, 8);
        assert_eq!(
            select_subtitles(&bluray_1_subtitles(), "deu,eng", true).tracks,
            Some(vec![0, 5])
        );
    }

    #[test]
    fn two_tracks_of_the_same_language_are_still_one_language() {
        let languages = vec!["eng".to_string(), "deu".to_string(), "ger".to_string()];
        let both_german = AudioSelection {
            tracks: vec![1, 2],
            fell_back: false,
        };

        assert!(one_language(&languages, &both_german));
        assert!(!one_language(
            &languages,
            &AudioSelection {
                tracks: vec![0, 1],
                fell_back: false
            }
        ));
    }

    /// Falling back to every track is not "one language", whatever the file happens to hold.
    #[test]
    fn a_fallback_never_counts_as_one_language() {
        let languages = vec!["eng".to_string()];
        let fallen_back = AudioSelection {
            tracks: vec![0],
            fell_back: true,
        };

        assert!(!one_language(&languages, &fallen_back));
    }

    #[test]
    fn dropping_the_subtitles_gives_the_crop_back() {
        assert_eq!(crop_for(Some(letterbox()), 8, false), None);
        assert_eq!(crop_for(Some(letterbox()), 0, false), Some(letterbox()));
    }

    #[test]
    fn no_subtitles_means_no_subtitle_map() {
        let line = command_line(
            &Plan {
                subtitle_tracks: Some(Vec::new()),
                ..plan()
            }
            .ffmpeg_args(),
        );

        assert!(!line.contains("0:s"), "{line}");
    }

    #[test]
    fn subtitles_take_the_crop_away() {
        assert_eq!(crop_for(Some(letterbox()), 0, false), Some(letterbox()));
        assert_eq!(crop_for(Some(letterbox()), 1, false), None);
        assert_eq!(crop_for(None, 0, false), None);
    }

    #[test]
    fn crop_anyway_puts_it_back_but_invents_nothing() {
        assert_eq!(crop_for(Some(letterbox()), 1, true), Some(letterbox()));
        assert_eq!(crop_for(None, 1, true), None);
    }

    #[test]
    fn opus_carries_the_layout_fix_that_libopus_needs() {
        let line = command_line(
            &Plan {
                audio: AudioMode::Opus,
                ..plan()
            }
            .ffmpeg_args(),
        );

        assert!(line.contains("-c:a libopus -b:a 256k"), "{line}");
        assert!(
            line.contains("-af aformat=channel_layouts=7.1|5.1|stereo|mono"),
            "{line}"
        );
        assert!(!line.contains("-c:a copy"), "{line}");
    }

    #[test]
    fn ten_bit_only_adds_the_pixel_format_when_it_was_asked_for() {
        let eight = command_line(&plan().ffmpeg_args());
        let ten = command_line(
            &Plan {
                ten_bit: true,
                ..plan()
            }
            .ffmpeg_args(),
        );

        assert!(!eight.contains("-pix_fmt"), "{eight}");
        assert!(ten.contains("-pix_fmt p010le"), "{ten}");
    }

    #[test]
    fn a_crop_alone_is_the_whole_chain() {
        let chain = Plan {
            crop: Some(letterbox()),
            ..plan()
        };

        assert_eq!(chain.filter_chain().as_deref(), Some("crop=688:544:20:16"));
    }

    #[test]
    fn the_command_writes_the_partial_and_never_overwrites() {
        let arguments = plan().ffmpeg_args();
        let line = command_line(&arguments);

        assert!(line.ends_with("/films/A1_t00.av1.mkv.partial"), "{line}");
        assert!(!arguments.iter().any(|argument| argument == "-y"));
        assert!(!arguments.iter().any(|argument| argument == "-vf"));
    }

    #[test]
    fn the_muxer_is_named_because_partial_is_not_an_extension_ffmpeg_knows() {
        let line = command_line(&plan().ffmpeg_args());

        assert!(line.contains("-f matroska"), "{line}");
    }

    #[test]
    fn the_bitrate_cap_is_switched_off_next_to_the_quality_setting() {
        let arguments = plan().ffmpeg_args();
        let line = command_line(&arguments);

        assert!(line.contains("-cq 28 -b:v 0"), "{line}");
    }

    #[test]
    fn a_name_with_spaces_stays_one_argument() {
        let spaced = Plan {
            input: PathBuf::from("/films/Film A - 2006.mkv"),
            ..plan()
        };
        let line = command_line(&spaced.ffmpeg_args());

        assert!(line.contains("'/films/Film A - 2006.mkv'"), "{line}");
    }

    #[test]
    fn the_output_sits_next_to_the_source() {
        let input = Path::new("/films/Ein Film äöü.mkv");

        assert_eq!(
            numbered(input, 1),
            PathBuf::from("/films/Ein Film äöü.av1.mkv")
        );
        assert_eq!(
            numbered(input, 2),
            PathBuf::from("/films/Ein Film äöü.av1.2.mkv")
        );
    }

    #[test]
    fn the_partial_is_the_output_plus_a_suffix() {
        assert_eq!(
            partial_path(Path::new("/films/A1_t00.av1.mkv")),
            PathBuf::from("/films/A1_t00.av1.mkv.partial")
        );
    }
}
