// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

mod analyze;
mod config;
mod defaults;
mod encode;
mod plan;
mod probe;

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches};
use gettextrs::{gettext, ngettext};

use media_core::app::{self, App};
use media_core::ffmpeg::{self, command_line};
use media_core::notify::{self, Level, Progress};
use media_core::text;
use plan::{AudioMode, Plan, Settings};

const NAME: &str = "av1-convert";

/// Convert video files to AV1 with NVENC, deinterlacing and cropping as the analysis suggests.
#[derive(clap::Parser, Debug)]
#[command(version, about, long_about = None)]
struct Options {
    /// Print the plan (detected values and the resulting ffmpeg command) instead of encoding
    #[arg(long, visible_alias = "dry-run")]
    analyze_only: bool,

    #[command(flatten)]
    settings: Settings,

    /// Print the raw ffmpeg commands and the parsed values
    #[arg(long)]
    verbose: bool,

    /// Send desktop notifications; on by default when stdout is not a terminal
    #[arg(long)]
    notify: bool,

    /// Ask for quality, audio and bit depth in a dialog before starting
    #[arg(long)]
    ask: bool,

    /// Open a form for the default settings and save them to the config file
    #[arg(long)]
    configure: bool,

    /// The files to convert, processed one after another
    #[arg(required_unless_present = "configure", conflicts_with = "configure")]
    files: Vec<PathBuf>,
}

fn main() -> ExitCode {
    text::init(NAME);
    app::set(App {
        name: NAME,
        env_prefix: "AV1_CONVERT",
        title: gettext("AV1 conversion"),
        icon: "video-x-generic",
    });

    let matches = Options::command().get_matches();

    let mut options = match Options::from_arg_matches(&matches) {
        Ok(options) => options,
        Err(error) => error.exit(),
    };

    // The config file fills in what was not typed, so an explicit flag always wins over it.
    let (config, config_problem) = match media_core::config::load::<config::Config>() {
        Ok(config) => (config, None),
        Err(reason) => (config::Config::default(), Some(reason)),
    };

    config::apply(&mut options.settings, &config, |name| {
        matches.value_source(name) == Some(ValueSource::CommandLine)
    });

    options.notify |= !std::io::stdout().is_terminal();

    if options.configure {
        return defaults::configure(&options.settings, config_problem.as_deref(), options.notify);
    }

    let interrupted = options.notify;

    // Ctrl-C and SIGTERM must not leave a half file behind that looks finished.
    let _ = ctrlc::set_handler(move || {
        ffmpeg::discard_partial();
        notify::log(Level::Warn, "interrupted, the partial file was removed");

        // The one outcome that still goes to a notification: this runs on the signal handler's
        // thread, where the dialog - and the bus connection it is driven over - is out of reach.
        notify::notify(
            interrupted,
            &gettext("AV1 conversion interrupted"),
            &gettext("The partial file was removed, the source is untouched."),
            "dialog-warning",
        );

        std::process::exit(130);
    });

    if options.ask {
        match collect_settings(&mut options.settings) {
            Ok(true) => {}
            Ok(false) => return ExitCode::SUCCESS,
            Err(error) => {
                let reason = format!("{error:#}");

                eprintln!("{reason}");
                notify::report(
                    None,
                    options.notify,
                    Level::Error,
                    &format!("nothing converted: {reason}"),
                    &text::fill(
                        &gettext("Nothing converted, the settings window failed - {reason}"),
                        &[("reason", &reason)],
                    ),
                );

                return ExitCode::FAILURE;
            }
        }
    }

    // One window for the whole selection, opened before the first file is even probed: it is where
    // every outcome is reported now, and an analysis that fails has to have somewhere to say so.
    let progress = notify::progress_open(
        options.notify,
        &gettext("Convert to AV1"),
        &opening_label(&options.files),
    );

    // Reported here rather than when it was found: before the window there is nowhere to show it.
    if let Some(reason) = &config_problem {
        eprintln!("config file ignored: {reason}");
        notify::report(
            progress.as_ref(),
            options.notify,
            Level::Warn,
            &format!("config file ignored, the built-in defaults apply: {reason}"),
            &text::fill(
                &gettext("The config file was ignored, the built-in defaults apply - {reason}"),
                &[("reason", reason)],
            ),
        );
    }

    let mut failed = 0;

    for file in &options.files {
        if let Err(error) = convert(file, &options, progress.as_ref()) {
            let message = format!("{}: {error:#}", file.display());

            eprintln!("{message}");
            notify::report(
                progress.as_ref(),
                options.notify,
                Level::Error,
                &message,
                &text::fill(
                    &gettext("{file}: failed - {reason}"),
                    &[
                        ("file", &file_name(file)),
                        ("reason", &format!("{error:#}")),
                    ],
                ),
            );

            failed += 1;
        }
    }

    if let Some(dialog) = &progress {
        dialog.finished(&closing_label(options.files.len(), failed), failed == 0);
    }

    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn file_name(file: &Path) -> String {
    file.file_name()
        .unwrap_or(file.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// What the window says before the first plan is ready.
fn opening_label(files: &[PathBuf]) -> String {
    match files {
        [file] => file_name(file),
        _ => text::fill(
            &ngettext("{count} file", "{count} files", files.len() as u32),
            &[("count", &files.len().to_string())],
        ),
    }
}

/// And what it says once nothing more will happen in it.
fn closing_label(files: usize, failed: usize) -> String {
    if failed == 0 {
        return text::fill(
            &ngettext("{count} file done", "{count} files done", files as u32),
            &[("count", &files.to_string())],
        );
    }

    text::fill(
        &ngettext(
            "{failed} of {count} file failed",
            "{failed} of {count} files failed",
            files as u32,
        ),
        &[
            ("failed", &failed.to_string()),
            ("count", &files.to_string()),
        ],
    )
}

/// Whole settings to start from, filled into the form's fields, which stay open to adjust: cq 20 is
/// what noisy camera footage needs (measured against the hand-made encode of the DVD-VR rip), cq 28
/// is what a DVD film was validated at.
const PRESETS: [(&str, u8, AudioMode, bool); 4] = [
    ("film", 28, AudioMode::Copy, false),
    ("camera", 20, AudioMode::Copy, false),
    ("archive", 20, AudioMode::Copy, true),
    ("small", 32, AudioMode::Opus, false),
];

/// The labels sit here rather than in `PRESETS` because extraction is static: a translated string
/// has to be a literal inside the `gettext()` call, and a `const` array of them extracts nothing.
fn preset_label(tag: &str) -> String {
    // i18n: the preset names and the field labels go into kurodlg's colon- and bar-separated
    // arguments, so a translation may contain neither ":" nor "|".
    match tag {
        "film" => gettext("DVD film"),
        "camera" => gettext("Camera or noisy footage"),
        "archive" => gettext("Archive"),
        "small" => gettext("Save space"),
        _ => gettext("Custom"),
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// The form's `--field` and `--preset` arguments. The fields start from `settings` - defaults,
/// config file, flags - and kurodlg opens the combo on whichever preset they amount to, or Custom.
fn form_arguments(settings: &Settings) -> Vec<String> {
    let labels: Vec<String> = PRESETS.iter().map(|(tag, ..)| preset_label(tag)).collect();

    let mut arguments = vec![
        "--field".to_string(),
        format!(
            "preset:combo:{}:{}|{}",
            gettext("Preset"),
            labels.join("|"),
            preset_label("custom")
        ),
        "--field".to_string(),
        // av1_nvenc takes -cq from 0 to 63, and 0 means "automatic" rather than best.
        format!(
            "cq:int:{}:{}:1-63",
            gettext("Quality, lower is better"),
            settings.cq
        ),
        "--field".to_string(),
        format!(
            "opus:check:{}:{}",
            gettext("Transcode the audio to Opus, 256 kb/s"),
            on_off(settings.audio == AudioMode::Opus)
        ),
        "--field".to_string(),
        format!(
            "tenbit:check:{}:{}",
            gettext("Encode 10 bit instead of 8"),
            on_off(settings.ten_bit)
        ),
    ];

    for ((_, cq, audio, ten_bit), label) in PRESETS.iter().zip(&labels) {
        arguments.push("--preset".to_string());
        arguments.push(format!(
            "preset:{label}:cq={cq}:opus={}:tenbit={}",
            on_off(*audio == AudioMode::Opus),
            on_off(*ten_bit)
        ));
    }

    arguments
}

/// The form's answer, carried into `settings`. The preset it names is not read: the fields already
/// hold what it stood for, plus whatever was changed afterwards.
fn apply_answer(settings: &mut Settings, answer: &HashMap<String, String>) -> Result<()> {
    settings.cq = answer
        .get("cq")
        .and_then(|cq| cq.parse().ok())
        .with_context(|| format!("the form returned no usable quality: {answer:?}"))?;
    settings.audio = if answer.get("opus").is_some_and(|opus| opus == "on") {
        AudioMode::Opus
    } else {
        AudioMode::Copy
    };
    settings.ten_bit = answer.get("tenbit").is_some_and(|ten_bit| ten_bit == "on");

    Ok(())
}

/// Ask how to encode, in one window. `Ok(false)` means the user backed out, which ends the run
/// without converting anything and without calling that a failure.
fn collect_settings(settings: &mut Settings) -> Result<bool> {
    let Some(answer) = notify::ask_form(
        &gettext("Convert to AV1"),
        &gettext("How should it be encoded?"),
        &form_arguments(settings),
    )
    .map_err(anyhow::Error::msg)
    .context("asking for the settings")?
    else {
        return Ok(false);
    };

    apply_answer(settings, &answer)?;

    Ok(true)
}

fn convert(file: &Path, options: &Options, dialog: Option<&Progress>) -> Result<()> {
    let name = file_name(file);
    let info = probe::probe(file)?;

    let Some(video) = info.video.clone() else {
        return skip(
            &name,
            "there is no video stream",
            &gettext("there is no video stream"),
            options,
            dialog,
        );
    };

    if video.codec == "av1" {
        return skip(
            &name,
            "it is already AV1",
            &gettext("it is already AV1"),
            options,
            dialog,
        );
    }

    let analysis = analyze::analyze(file, &info, options.verbose)?;
    let plan = Plan::new(file, &info, &analysis, &options.settings)?;
    let arguments = plan.ffmpeg_args();
    let mut warnings = analysis.warnings.clone();

    if plan::select_audio(&info.audio_languages, &options.settings.audio_langs).fell_back {
        warnings.push(text::Message::filled(
            "every audio track kept: none of them is {langs}, and a file without audio would be \
             worse than a large one",
            gettext(
                "every audio track kept: none of them is {langs}, and a file without audio would \
                 be worse than a large one",
            ),
            &[("langs", &options.settings.audio_langs)],
        ));
    }

    if analysis.crop.crop.is_some() && plan.subtitle_streams > 0 {
        let streams = plan.subtitle_streams.to_string();

        warnings.push(if plan.crop.is_some() {
            text::Message::filled(
                "cropped anyway: the {count} subtitle stream(s) will sit misplaced, their bitmaps \
                 belong to the uncropped frame",
                gettext(
                    "cropped anyway: the {count} subtitle stream(s) will sit misplaced, their \
                     bitmaps belong to the uncropped frame",
                ),
                &[("count", &streams)],
            )
        } else {
            text::Message::filled(
                "no crop: {count} subtitle stream(s) are kept, and their bitmaps are placed in the \
                 uncropped frame",
                gettext(
                    "no crop: {count} subtitle stream(s) are kept, and their bitmaps are placed in \
                     the uncropped frame",
                ),
                &[("count", &streams)],
            )
        });
    }

    for warning in &warnings {
        notify::report(
            dialog,
            options.notify,
            Level::Warn,
            &format!("{name}: {}", warning.logged),
            &text::fill(
                &gettext("{file}: {warning}"),
                &[("file", &name), ("warning", &warning.shown)],
            ),
        );

        if options.analyze_only || options.verbose {
            println!("  warning: {}", warning.logged);
        }
    }

    if options.analyze_only {
        println!("{name}");
        println!(
            "  {} {}x{} SAR {} at {} fps, field order {}, {:.0} min, {} audio and {} subtitle stream(s)",
            video.codec,
            video.width,
            video.height,
            video.sample_aspect_ratio,
            video.frame_rate,
            video.field_order,
            info.duration / 60.0,
            info.audio_streams(),
            info.subtitle_streams()
        );
        println!(
            "  subtitles: {} -> keeping {}",
            info.subtitle_streams(),
            match &plan.subtitle_tracks {
                None => "all of them".to_string(),
                Some(tracks) if tracks.is_empty() => "none".to_string(),
                Some(tracks) => tracks
                    .iter()
                    .filter_map(|track| info.subtitle_languages.get(*track).cloned())
                    .collect::<Vec<_>>()
                    .join(", "),
            }
        );
        println!(
            "  audio: {} ({}) -> keeping {}",
            info.audio_streams(),
            info.audio_languages.join(", "),
            match &plan.audio_tracks {
                None => "all of them".to_string(),
                Some(tracks) => tracks
                    .iter()
                    .map(|track| info.audio_languages[*track].clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            }
        );
        println!(
            "  interlaced {:.1} % (TFF {}, BFF {}, progressive {}), repeated fields {} top / {} bottom",
            analysis.interlaced_ratio * 100.0,
            analysis.fields.top_field_first,
            analysis.fields.bottom_field_first,
            analysis.fields.progressive,
            analysis.repeats.top,
            analysis.repeats.bottom
        );
        // The measured rectangle, not the one that survived: a crop dropped over subtitles is
        // exactly when the number matters, and printing "none" hides what --crop-anyway would do.
        println!(
            "  crop: {} ({:.0} % of the measurements){}",
            match analysis.crop.crop {
                Some(crop) => crop.to_string(),
                None => "none".to_string(),
            },
            analysis.crop.share * 100.0,
            if analysis.crop.crop.is_some() && plan.crop.is_none() {
                " - dropped, see the warning"
            } else {
                ""
            }
        );
        println!("  plan: {}", plan.summary());
        println!("  -> {}", plan.output.display());
        println!("  {}", command_line(&arguments));

        return Ok(());
    }

    let source_size = std::fs::metadata(file)
        .with_context(|| format!("reading the size of {}", file.display()))?
        .len();

    notify::report(
        dialog,
        options.notify,
        Level::Info,
        &format!("{name}: {}\n{}", plan.summary(), command_line(&arguments)),
        &text::fill(
            // i18n: {plan} is the technical line the log carries too - "deinterlace, crop 704x448, cq 28"
            &gettext("{file}: converting - {plan}"),
            &[("file", &name), ("plan", &plan.summary_shown())],
        ),
    );

    if options.verbose {
        println!("{}", command_line(&arguments));
    }

    let terminal = std::io::stdout().is_terminal();

    let outcome = encode::run(&plan, &mut |percent| {
        if terminal {
            print!("\r{name}: {percent} %");
            let _ = std::io::stdout().flush();
        }

        match dialog {
            Some(dialog) => {
                dialog.set(percent, &format!("{name}: {percent} %"));

                !dialog.cancelled()
            }
            None => true,
        }
    });

    if terminal {
        println!();
    }

    if outcome? == ffmpeg::Outcome::Cancelled {
        notify::report(
            dialog,
            options.notify,
            Level::Warn,
            &format!("{name}: cancelled, the partial file was removed"),
            &text::fill(
                &gettext("{file}: cancelled, the partial file was removed"),
                &[("file", &name)],
            ),
        );

        if let Some(dialog) = dialog {
            dialog.finished(&gettext("Cancelled"), false);
        }

        // The rest of the selection is not wanted either once the user has said stop.
        std::process::exit(130);
    }

    let encoded_size = encode::finish(&plan)?;
    let change = notify::size_change(source_size, encoded_size);

    if encoded_size > source_size {
        notify::report(
            dialog,
            options.notify,
            Level::Warn,
            &format!("{name}: the AV1 file is larger than the source, both were kept"),
            &text::fill(
                &gettext("{file}: the AV1 file is larger than the source, both were kept"),
                &[("file", &name)],
            ),
        );
    }

    notify::report(
        dialog,
        options.notify,
        Level::Info,
        &format!("{name}: {change} -> {}", plan.output.display()),
        &text::fill(
            // i18n: {change} reads "4.27 GiB -> 1.23 GiB (-71 %)"
            &gettext("{file}: done, {change}"),
            &[("file", &name), ("change", &change)],
        ),
    );

    if terminal {
        println!("{name}: {change} -> {}", plan.output.display());
    }

    Ok(())
}

/// `reason` twice: English is what the log carries, whatever language the desktop is in.
fn skip(
    name: &str,
    reason: &str,
    translated_reason: &str,
    options: &Options,
    dialog: Option<&Progress>,
) -> Result<()> {
    let message = format!("{name}: skipped, {reason}");

    notify::report(
        dialog,
        options.notify,
        Level::Warn,
        &message,
        &text::fill(
            &gettext("{file}: skipped, {reason}"),
            &[("file", name), ("reason", translated_reason)],
        ),
    );

    if std::io::stdout().is_terminal() {
        println!("{message}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            cq: 24,
            audio: AudioMode::Opus,
            ten_bit: true,
            crop_anyway: false,
            audio_langs: "all".to_string(),
            subtitle_langs: "auto".to_string(),
        }
    }

    #[test]
    fn the_fields_start_from_the_settings_the_run_has() {
        let arguments = form_arguments(&settings());

        assert!(
            arguments
                .iter()
                .any(|argument| argument.starts_with("cq:int:") && argument.ends_with(":24:1-63"))
        );
        assert!(
            arguments
                .iter()
                .any(|argument| argument.starts_with("opus:check:") && argument.ends_with(":on"))
        );
        assert!(
            arguments
                .iter()
                .any(|argument| argument.starts_with("tenbit:check:") && argument.ends_with(":on"))
        );
    }

    #[test]
    fn every_preset_sets_all_three_fields() {
        let arguments = form_arguments(&settings());
        let presets: Vec<&String> = arguments
            .iter()
            .filter(|argument| argument.starts_with("preset:") && !argument.contains(":combo:"))
            .collect();

        assert_eq!(presets.len(), PRESETS.len());
        assert!(presets.contains(&&"preset:DVD film:cq=28:opus=off:tenbit=off".to_string()));
        assert!(presets.contains(&&"preset:Save space:cq=32:opus=on:tenbit=off".to_string()));
        assert!(
            arguments.contains(
                &"preset:combo:Preset:DVD film|Camera or noisy footage|Archive|Save space|Custom"
                    .to_string()
            )
        );
    }

    #[test]
    fn the_answer_overwrites_what_the_form_asked_about() {
        let mut settings = settings();
        let answer = notify::parse_form("preset=Custom\ncq=20\nopus=off\ntenbit=off\n");

        apply_answer(&mut settings, &answer).unwrap();

        assert_eq!(settings.cq, 20);
        assert_eq!(settings.audio, AudioMode::Copy);
        assert!(!settings.ten_bit);
        assert_eq!(settings.audio_langs, "all");
    }

    #[test]
    fn an_answer_without_a_quality_is_an_error() {
        let mut settings = settings();

        assert!(apply_answer(&mut settings, &notify::parse_form("opus=on\n")).is_err());
        assert!(apply_answer(&mut settings, &notify::parse_form("cq=high\n")).is_err());
        assert_eq!(settings.cq, 24);
    }
}
