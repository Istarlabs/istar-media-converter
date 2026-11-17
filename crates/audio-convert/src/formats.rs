// SPDX-FileCopyrightText: 2026 SavageCore
// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: MIT

/// A supported target audio format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Mp3,
    Ogg,
    Flac,
    Wav,
    M4a,
    Opus,
    Alac,
}

use Format::*;

impl Format {
    pub const ALL: [Format; 7] = [Mp3, Ogg, Flac, Wav, M4a, Opus, Alac];

    /// Parses the stable lowercase identifier (CLI arg, config key, menu tag).
    pub fn from_key(s: &str) -> Option<Format> {
        Self::ALL.into_iter().find(|f| f.key() == s)
    }

    /// Stable lowercase identifier used in CLI args, config keys and menu tags.
    pub fn key(self) -> &'static str {
        match self {
            Mp3 => "mp3",
            Ogg => "ogg",
            Flac => "flac",
            Wav => "wav",
            M4a => "m4a",
            Opus => "opus",
            Alac => "alac",
        }
    }

    /// The `[Desktop Action <id>]` of this format in the service menu.
    pub fn action_id(self) -> &'static str {
        match self {
            Mp3 => "convertToMp3",
            Ogg => "convertToOgg",
            Flac => "convertToFlac",
            Wav => "convertToWav",
            M4a => "convertToM4a",
            Opus => "convertToOpus",
            Alac => "convertToAlac",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Mp3 => "audio-x-mp3",
            Ogg => "audio-x-vorbis+ogg",
            Flac => "audio-x-flac",
            Wav => "audio-x-wav",
            M4a => "audio-x-m4a",
            Opus => "audio-x-opus+ogg",
            Alac => "audio-x-generic",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Mp3 => "MP3",
            Ogg => "OGG (Vorbis)",
            Flac => "FLAC",
            Wav => "WAV",
            M4a => "M4A (AAC)",
            Opus => "Opus",
            Alac => "ALAC (M4A)",
        }
    }

    /// File extension of the output container.
    pub fn extension(self) -> &'static str {
        match self {
            M4a | Alac => "m4a",
            other => other.key(),
        }
    }

    pub fn is_lossy(self) -> bool {
        matches!(self, Mp3 | Ogg | M4a | Opus)
    }

    pub fn quality_options(self) -> &'static [&'static str] {
        match self {
            Mp3 => &["V0", "V2", "V4", "128k", "192k", "256k", "320k"],
            Ogg => &["Q3", "Q5", "Q6", "Q8", "Q10"],
            M4a => &["128k", "192k", "256k", "320k"],
            Opus => &["64k", "96k", "128k", "192k", "256k"],
            Flac | Wav | Alac => &["lossless"],
        }
    }

    pub fn default_quality(self) -> &'static str {
        match self {
            Mp3 => "V0",
            Ogg => "Q6",
            M4a => "192k",
            Opus => "128k",
            Flac | Wav | Alac => "lossless",
        }
    }

    pub fn quality_desc(self, q: &str) -> &'static str {
        match (self, q) {
            (Mp3, "V0") => "VBR ~245 kbps (best)",
            (Mp3, "V2") => "VBR ~190 kbps",
            (Mp3, "V4") => "VBR ~165 kbps",
            (Mp3, "128k") => "CBR 128 kbps",
            (Mp3, "192k") => "CBR 192 kbps",
            (Mp3, "256k") => "CBR 256 kbps",
            (Mp3, "320k") => "CBR 320 kbps (max)",
            (Ogg, "Q3") => "~112 kbps",
            (Ogg, "Q5") => "~160 kbps",
            (Ogg, "Q6") => "~192 kbps (default)",
            (Ogg, "Q8") => "~256 kbps",
            (Ogg, "Q10") => "~500 kbps (best)",
            (M4a, "128k") => "128 kbps",
            (M4a, "192k") => "192 kbps (default)",
            (M4a, "256k") => "256 kbps",
            (M4a, "320k") => "320 kbps",
            (Opus, "64k") => "64 kbps (voice)",
            (Opus, "96k") => "96 kbps",
            (Opus, "128k") => "128 kbps (default)",
            (Opus, "192k") => "192 kbps",
            (Opus, "256k") => "256 kbps (transparent)",
            _ => "",
        }
    }

    pub fn ffmpeg_args(self, quality: &str) -> Vec<String> {
        match self {
            Mp3 => {
                if quality.len() == 2
                    && quality.starts_with('V')
                    && quality.as_bytes()[1].is_ascii_digit()
                {
                    vec![
                        "-codec:a".into(),
                        "libmp3lame".into(),
                        "-q:a".into(),
                        quality[1..].into(),
                    ]
                } else {
                    vec![
                        "-codec:a".into(),
                        "libmp3lame".into(),
                        "-b:a".into(),
                        quality.into(),
                    ]
                }
            }
            Ogg => {
                let q = quality.strip_prefix('Q').unwrap_or("6");
                vec![
                    "-codec:a".into(),
                    "libvorbis".into(),
                    "-q:a".into(),
                    q.into(),
                ]
            }
            M4a => vec![
                "-codec:a".into(),
                "aac".into(),
                "-b:a".into(),
                quality.into(),
            ],
            Opus => vec![
                "-codec:a".into(),
                "libopus".into(),
                "-b:a".into(),
                quality.into(),
            ],
            Flac => vec![
                "-codec:a".into(),
                "flac".into(),
                "-compression_level".into(),
                "8".into(),
            ],
            Wav => vec!["-codec:a".into(), "pcm_s16le".into()],
            Alac => vec!["-codec:a".into(), "alac".into()],
        }
    }
}

pub fn configurable_formats() -> Vec<Format> {
    Format::ALL
        .into_iter()
        .filter(|f| f.quality_options() != ["lossless"])
        .collect()
}

// German display names differ only for the "verb" wrapped around the format,
// the format labels themselves stay the same (they're mostly proper nouns).
pub fn action_name_en(fmt: Format, quality_label: &str) -> String {
    format!("Convert to {}{}", fmt.display(), quality_label)
}

pub fn action_name_de(fmt: Format, quality_label: &str) -> String {
    format!("In {} konvertieren{}", fmt.display(), quality_label)
}

pub fn quality_label(quality: &str) -> String {
    if quality != "lossless" {
        format!(" ({})", quality)
    } else {
        String::new()
    }
}

/// ffprobe's codec names stay strings: the list of codecs is open-ended and only these matter.
pub fn codec_category(codec: &str) -> &'static str {
    match codec {
        "mp3" | "vorbis" | "aac" | "opus" | "wmav1" | "wmav2" | "ac3" | "eac3" | "mp2"
        | "amrnb" | "amrwb" | "wmavoice" => "lossy",
        "flac" | "alac" | "wavpack" | "ape" | "tta" | "truehd" | "mlp" | "pcm_s16le"
        | "pcm_s24le" | "pcm_s32le" | "pcm_f32le" | "pcm_f64le" | "pcm_s16be" => "lossless",
        _ => "unknown",
    }
}

// Picks a sensible container extension for a raw copied audio stream.
// "mka" (Matroska audio) is the catch-all fallback since it can hold
// virtually any codec without re-encoding.
pub fn extract_extension_for_codec(codec: &str) -> &str {
    match codec {
        "aac" | "alac" => "m4a",
        "mp3" => "mp3",
        "flac" => "flac",
        "vorbis" => "ogg",
        "opus" => "opus",
        "ac3" => "ac3",
        "eac3" => "eac3",
        "dts" => "dts",
        "wmav1" | "wmav2" => "wma",
        "pcm_s16le" | "pcm_s24le" | "pcm_s32le" | "pcm_f32le" | "pcm_f64le" | "pcm_s16be" => "wav",
        _ => "mka",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_finds_its_format_again() {
        for format in Format::ALL {
            assert!(Format::from_key(format.key()) == Some(format));
        }

        assert!(Format::from_key("mp4").is_none());
    }

    #[test]
    fn every_default_is_one_of_the_options() {
        for format in Format::ALL {
            assert!(
                format.quality_options().contains(&format.default_quality()),
                "{}",
                format.key()
            );
        }
    }

    #[test]
    fn mp3_tells_a_vbr_level_from_a_bitrate() {
        assert_eq!(
            Mp3.ffmpeg_args("V2"),
            ["-codec:a", "libmp3lame", "-q:a", "2"]
        );
        assert_eq!(
            Mp3.ffmpeg_args("320k"),
            ["-codec:a", "libmp3lame", "-b:a", "320k"]
        );
    }

    #[test]
    fn ogg_quality_loses_its_prefix() {
        assert_eq!(
            Ogg.ffmpeg_args("Q10"),
            ["-codec:a", "libvorbis", "-q:a", "10"]
        );
    }

    #[test]
    fn only_formats_with_a_choice_are_configurable() {
        let keys: Vec<&str> = configurable_formats().iter().map(|f| f.key()).collect();

        assert_eq!(keys, ["mp3", "ogg", "m4a", "opus"]);
    }

    #[test]
    fn alac_and_aac_share_a_container() {
        assert_eq!(Alac.extension(), "m4a");
        assert_eq!(M4a.extension(), "m4a");
        assert_eq!(Opus.extension(), "opus");
    }

    #[test]
    fn codecs_sort_into_lossy_and_lossless() {
        assert_eq!(codec_category("mp3"), "lossy");
        assert_eq!(codec_category("flac"), "lossless");
        assert_eq!(codec_category("dts"), "unknown");
    }

    #[test]
    fn a_stream_copy_gets_the_container_of_its_codec() {
        assert_eq!(extract_extension_for_codec("vorbis"), "ogg");
        assert_eq!(extract_extension_for_codec("pcm_s24le"), "wav");
        assert_eq!(extract_extension_for_codec("truehd"), "mka");
    }

    #[test]
    fn the_quality_shows_in_the_menu_label_unless_lossless() {
        assert_eq!(quality_label("V0"), " (V0)");
        assert_eq!(quality_label("lossless"), "");
        assert_eq!(
            action_name_de(Opus, &quality_label("128k")),
            "In Opus konvertieren (128k)"
        );
    }
}
