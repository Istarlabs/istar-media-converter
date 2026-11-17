// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use gettextrs::gettext;
use media_core::notify::{self, Level};
use media_core::text;

use crate::on_off;
use crate::plan::{ALL_LANGUAGES, AUTO_LANGUAGES, AudioMode, Settings};

/// The file as "Default settings..." writes it: all of it, every time, so a missing file is simply
/// created and the keys are always the ones `config::Config` knows. Comments written by hand do
/// not survive a save. ASCII only - an editor that opens a UTF-8 file without a BOM may guess
/// Windows-1252 and show a "..." as garbage.
pub fn render(settings: &Settings) -> String {
    let audio = match settings.audio {
        AudioMode::Copy => "copy",
        AudioMode::Opus => "opus",
    };

    format!(
        r#"# av1-convert defaults, as "Default settings" in Dolphin's Video Converter menu writes them.
# Every key is a long option without its dashes, and anything typed on the command line wins.

# NVENC constant quality, 1-63, lower is better
cq = {cq}

# "copy" keeps the audio tracks, "opus" transcodes them at 256 kb/s
audio = "{audio}"

# encode p010le instead of 8 bit
10bit = {ten_bit}

# crop although the file has subtitles, which then sit misplaced
crop-anyway = {crop_anyway}

# which audio tracks to keep, ISO 639-2 ("deu,eng"), or "all"
audio-langs = {audio_langs}

# "auto" drops them once the audio is one language; also "all", "none", or a list like "deu,eng"
subtitle-langs = {subtitle_langs}
"#,
        cq = settings.cq,
        ten_bit = settings.ten_bit,
        crop_anyway = settings.crop_anyway,
        audio_langs = media_core::config::quote(&settings.audio_langs),
        subtitle_langs = media_core::config::quote(&settings.subtitle_langs),
    )
}

/// The form's `--field` arguments, starting from `settings` - defaults, config file, flags.
fn form_arguments(settings: &Settings) -> Vec<String> {
    // i18n: the labels go into kurodlg's colon-separated arguments, so a translation may not
    // contain a ":".
    let fields = [
        format!(
            "cq:int:{}:{}:1-63",
            gettext("Quality, lower is better"),
            settings.cq
        ),
        format!(
            "opus:check:{}:{}",
            gettext("Transcode the audio to Opus, 256 kb/s"),
            on_off(settings.audio == AudioMode::Opus)
        ),
        format!(
            "tenbit:check:{}:{}",
            gettext("Encode 10 bit instead of 8"),
            on_off(settings.ten_bit)
        ),
        format!(
            "cropanyway:check:{}:{}",
            gettext("Crop even with subtitles (they then sit misplaced)"),
            on_off(settings.crop_anyway)
        ),
        format!(
            "audiolangs:text:{}:{}",
            gettext("Audio tracks (deu,eng or all)"),
            settings.audio_langs
        ),
        format!(
            "subtitlelangs:text:{}:{}",
            gettext("Subtitles (auto, all, none or deu,eng)"),
            settings.subtitle_langs
        ),
    ];

    fields
        .into_iter()
        .flat_map(|field| ["--field".to_string(), field])
        .collect()
}

/// The form's answer as settings. An emptied text field means the built-in default.
fn read_answer(answer: &HashMap<String, String>) -> Result<Settings> {
    let cq = answer
        .get("cq")
        .and_then(|cq| cq.parse::<u8>().ok())
        .filter(|cq| (1..=63).contains(cq))
        .with_context(|| format!("the form returned no usable quality: {answer:?}"))?;
    let on = |name: &str| answer.get(name).is_some_and(|value| value == "on");
    let langs = |name: &str, default: &str| {
        answer
            .get(name)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or(default)
            .to_string()
    };

    Ok(Settings {
        cq,
        audio: if on("opus") {
            AudioMode::Opus
        } else {
            AudioMode::Copy
        },
        ten_bit: on("tenbit"),
        crop_anyway: on("cropanyway"),
        audio_langs: langs("audiolangs", ALL_LANGUAGES),
        subtitle_langs: langs("subtitlelangs", AUTO_LANGUAGES),
    })
}

/// Ask, then write the file. `None` means the user backed out and nothing was touched.
fn ask_and_save(current: &Settings) -> Result<Option<PathBuf>> {
    let Some(answer) = notify::ask_form(
        &gettext("AV1 default settings"),
        &gettext("What av1-convert encodes with when nothing else is given:"),
        &form_arguments(current),
    )
    .map_err(anyhow::Error::msg)
    .context("asking for the default settings")?
    else {
        return Ok(None);
    };

    let path = media_core::config::save(&render(&read_answer(&answer)?))
        .map_err(anyhow::Error::msg)
        .context("writing the config file")?;

    Ok(Some(path))
}

/// `--configure`. A config file that is there but does not parse is not overwritten: whatever is
/// in it was written by someone, and a save from defaults would throw it away unseen.
pub fn configure(
    current: &Settings,
    config_problem: Option<&str>,
    notifications: bool,
) -> ExitCode {
    let failure = |logged: String, shown: String| {
        eprintln!("{logged}");
        notify::report(None, notifications, Level::Error, &logged, &shown);

        ExitCode::FAILURE
    };

    if let Some(reason) = config_problem {
        return failure(
            format!("config file left unchanged, it cannot be read: {reason}"),
            text::fill(
                &gettext("The config file cannot be read and stays unchanged - {reason}"),
                &[("reason", reason)],
            ),
        );
    }

    match ask_and_save(current) {
        Ok(Some(path)) => {
            let path = path.display().to_string();

            notify::report(
                None,
                notifications,
                Level::Info,
                &format!("default settings saved to {path}"),
                &text::fill(&gettext("Saved: {path}"), &[("path", &path)]),
            );

            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => {
            let reason = format!("{error:#}");

            failure(
                format!("nothing saved: {reason}"),
                text::fill(
                    &gettext("Nothing saved, the settings window failed - {reason}"),
                    &[("reason", &reason)],
                ),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{self, Config};

    fn settings() -> Settings {
        Settings {
            cq: 24,
            audio: AudioMode::Opus,
            ten_bit: true,
            crop_anyway: true,
            audio_langs: "deu,eng".to_string(),
            subtitle_langs: "none".to_string(),
        }
    }

    fn untouched() -> Settings {
        Settings {
            cq: 28,
            audio: AudioMode::Copy,
            ten_bit: false,
            crop_anyway: false,
            audio_langs: ALL_LANGUAGES.to_string(),
            subtitle_langs: AUTO_LANGUAGES.to_string(),
        }
    }

    fn read_back(written: &Settings) -> Settings {
        let config: Config = media_core::config::parse(&render(written)).unwrap();
        let mut settings = untouched();

        config::apply(&mut settings, &config, |_| false);

        settings
    }

    #[test]
    fn what_is_written_reads_back_as_the_same_settings() {
        assert_eq!(read_back(&settings()), settings());
        assert_eq!(read_back(&untouched()), untouched());
    }

    #[test]
    fn a_value_with_quotes_survives_the_file() {
        let awkward = Settings {
            audio_langs: "deu\"eng".to_string(),
            subtitle_langs: "back\\slash".to_string(),
            ..untouched()
        };

        assert_eq!(read_back(&awkward), awkward);
    }

    #[test]
    fn the_fields_start_from_the_settings_the_run_has() {
        let arguments = form_arguments(&settings());

        for expected in [
            ":24:1-63",
            "opus:check:Transcode the audio to Opus, 256 kb/s:on",
            "tenbit:check:Encode 10 bit instead of 8:on",
            "cropanyway:check:Crop even with subtitles (they then sit misplaced):on",
            "audiolangs:text:Audio tracks (deu,eng or all):deu,eng",
            "subtitlelangs:text:Subtitles (auto, all, none or deu,eng):none",
        ] {
            assert!(
                arguments
                    .iter()
                    .any(|argument| argument.ends_with(expected)),
                "{expected} is missing from {arguments:?}"
            );
        }

        assert_eq!(arguments.iter().filter(|a| *a == "--field").count(), 6);
    }

    #[test]
    fn the_answer_becomes_settings() {
        let answer = notify::parse_form(
            "cq=20\nopus=on\ntenbit=off\ncropanyway=on\naudiolangs= deu \nsubtitlelangs=all\n",
        );

        assert_eq!(
            read_answer(&answer).unwrap(),
            Settings {
                cq: 20,
                audio: AudioMode::Opus,
                ten_bit: false,
                crop_anyway: true,
                audio_langs: "deu".to_string(),
                subtitle_langs: "all".to_string(),
            }
        );
    }

    #[test]
    fn an_emptied_text_field_means_the_default() {
        let answer = notify::parse_form("cq=28\nopus=off\naudiolangs=\nsubtitlelangs=  \n");
        let settings = read_answer(&answer).unwrap();

        assert_eq!(settings, untouched());
    }

    #[test]
    fn an_answer_without_a_usable_quality_is_an_error() {
        assert!(read_answer(&notify::parse_form("opus=on\n")).is_err());
        assert!(read_answer(&notify::parse_form("cq=high\n")).is_err());
        assert!(read_answer(&notify::parse_form("cq=0\n")).is_err());
        assert!(read_answer(&notify::parse_form("cq=64\n")).is_err());
    }
}
