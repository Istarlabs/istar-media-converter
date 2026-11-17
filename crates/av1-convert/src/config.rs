// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use serde::Deserialize;

use crate::plan::{AudioMode, Settings};

/// `config.toml`. The keys are the long options without their dashes, so the file and `--help`
/// speak one vocabulary and nothing has to be translated between them.
///
/// Unknown keys are refused rather than ignored: a misspelt `10-bit = true` that silently does
/// nothing is the kind of error the move away from a hand-written parser was meant to end.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    pub cq: Option<u8>,
    pub audio: Option<AudioMode>,
    #[serde(rename = "10bit")]
    pub ten_bit: Option<bool>,
    pub crop_anyway: Option<bool>,
    pub audio_langs: Option<String>,
    pub subtitle_langs: Option<String>,
}

/// Fill in the settings the command line did not give. `given` answers "was this typed?", which is
/// what keeps the order right: an explicit flag beats the file, the file beats the built-in
/// default.
pub fn apply(settings: &mut Settings, config: &Config, given: impl Fn(&str) -> bool) {
    if !given("cq")
        && let Some(cq) = config.cq
    {
        settings.cq = cq;
    }

    if !given("audio")
        && let Some(audio) = config.audio
    {
        settings.audio = audio;
    }

    if !given("ten_bit")
        && let Some(on) = config.ten_bit
    {
        settings.ten_bit = on;
    }

    if !given("crop_anyway")
        && let Some(on) = config.crop_anyway
    {
        settings.crop_anyway = on;
    }

    if !given("audio_langs")
        && let Some(langs) = &config.audio_langs
    {
        settings.audio_langs = langs.clone();
    }

    if !given("subtitle_langs")
        && let Some(langs) = &config.subtitle_langs
    {
        settings.subtitle_langs = langs.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, String> {
        media_core::config::parse(text)
    }

    const SAMPLE: &str = "# what av1-convert should do by default\n\
                          cq = 24\n\
                          audio-langs = \"deu,eng\"\n\
                          10bit = true      # dark films band at 8 bit\n\
                          \n\
                          audio=\"opus\"\n";

    fn settings() -> Settings {
        Settings {
            cq: 28,
            audio: AudioMode::Copy,
            ten_bit: false,
            crop_anyway: false,
            audio_langs: "all".to_string(),
            subtitle_langs: "auto".to_string(),
        }
    }

    #[test]
    fn reads_values_past_comments_and_spacing() {
        let config = parse(SAMPLE).unwrap();

        assert_eq!(config.cq, Some(24));
        assert_eq!(config.audio_langs.as_deref(), Some("deu,eng"));
        assert_eq!(config.ten_bit, Some(true));
        assert_eq!(config.audio, Some(AudioMode::Opus));
        assert_eq!(config.crop_anyway, None);
    }

    #[test]
    fn an_empty_or_all_comment_file_changes_nothing() {
        assert_eq!(parse("").unwrap(), Config::default());
        assert_eq!(parse("# cq = 20\n\n").unwrap(), Config::default());
    }

    #[test]
    fn a_misspelt_key_is_refused_with_its_line() {
        let reason = parse("cq = 24\n10-bit = true\n").unwrap_err();

        assert!(reason.starts_with("line 2:"), "{reason}");
        assert!(reason.contains("10-bit"), "{reason}");
    }

    #[test]
    fn a_value_of_the_wrong_kind_is_refused() {
        assert!(parse("cq = \"plenty\"\n").is_err());
        assert!(parse("cq = 300\n").is_err());
        assert!(parse("audio = \"flac\"\n").is_err());
        assert!(parse("10bit = \"perhaps\"\n").is_err());
        assert!(
            parse("audio-langs = deu\n").is_err(),
            "a bare word is not a TOML string"
        );
    }

    #[test]
    fn the_file_fills_in_what_the_command_line_left_out() {
        let mut settings = settings();

        apply(&mut settings, &parse(SAMPLE).unwrap(), |_| false);

        assert_eq!(settings.cq, 24);
        assert_eq!(settings.audio, AudioMode::Opus);
        assert!(settings.ten_bit);
        assert_eq!(settings.audio_langs, "deu,eng");
        assert_eq!(settings.subtitle_langs, "auto");
    }

    #[test]
    fn what_was_typed_wins() {
        let mut settings = Settings {
            cq: 20,
            audio_langs: "jpn".to_string(),
            ..settings()
        };

        apply(&mut settings, &parse(SAMPLE).unwrap(), |name| {
            matches!(name, "cq" | "audio_langs")
        });

        assert_eq!(settings.cq, 20, "the typed --cq must survive the file");
        assert_eq!(settings.audio_langs, "jpn");
        assert_eq!(
            settings.audio,
            AudioMode::Opus,
            "and the rest still comes from the file"
        );
    }
}
