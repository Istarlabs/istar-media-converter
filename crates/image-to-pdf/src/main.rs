// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};
use clap::Parser;
use gettextrs::gettext;

use media_core::app::{self, App};
use media_core::ffmpeg::place;
use media_core::notify::{self, Level};
use media_core::text;

const NAME: &str = "image-to-pdf";

/// The workspace has one catalogue: `Scripts/i18n.py` reads every crate into it.
const CATALOGUE: &str = "av1-convert";

const DEFAULT_NAME: &str = "merged";

/// How many `<name>.N.pdf` names are tried before giving up.
const NAME_ATTEMPTS: u32 = 99;

/// Convert images to a PDF, one page per image.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Options {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(clap::Subcommand, Debug)]
enum Mode {
    /// One image to one PDF, named after the image and written next to it
    Single { file: PathBuf },

    /// All images to one PDF, in the order given, named in a dialog and written next to the first
    Multi {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
}

fn main() -> ExitCode {
    text::init(CATALOGUE);
    app::set(App {
        name: NAME,
        env_prefix: "IMAGE_TO_PDF",
        title: gettext("Image to PDF"),
        icon: "image-x-generic",
    });

    match run(Options::parse().mode) {
        Ok(Some(output)) => {
            let path = output.display().to_string();

            notify::report(
                None,
                true,
                Level::Info,
                &format!("created {path}"),
                &text::fill(&gettext("Created: {path}"), &[("path", &path)]),
            );

            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => {
            let reason = format!("{error:#}");

            eprintln!("{reason}");
            notify::report(
                None,
                true,
                Level::Error,
                &format!("nothing created: {reason}"),
                &text::fill(
                    &gettext("Nothing created - {reason}"),
                    &[("reason", &reason)],
                ),
            );

            ExitCode::FAILURE
        }
    }
}

/// `Ok(None)` means the user backed out of the name dialog.
fn run(mode: Mode) -> Result<Option<PathBuf>> {
    let (inputs, output) = match mode {
        Mode::Single { file } => {
            let stem = file
                .file_stem()
                .with_context(|| format!("{} has no file name", file.display()))?
                .to_string_lossy()
                .into_owned();
            let output = free_name(directory_of(&file), &stem, exists)?;

            (vec![file], output)
        }
        Mode::Multi { files } => {
            let Some(name) = ask_name()? else {
                return Ok(None);
            };
            let output = free_name(directory_of(&files[0]), &name, exists)?;

            (files, output)
        }
    };

    convert(&inputs, &output)?;

    Ok(Some(output))
}

fn directory_of(file: &Path) -> &Path {
    file.parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// A dangling symlink is a name taken as well.
fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// `<name>.pdf`, or `<name>.2.pdf` and on when that is taken. Nothing is ever overwritten.
fn free_name(directory: &Path, name: &str, taken: impl Fn(&Path) -> bool) -> Result<PathBuf> {
    let first = directory.join(format!("{name}.pdf"));

    if !taken(&first) {
        return Ok(first);
    }

    (2..=NAME_ATTEMPTS)
        .map(|number| directory.join(format!("{name}.{number}.pdf")))
        .find(|candidate| !taken(candidate))
        .with_context(|| {
            format!(
                "{} and its {NAME_ATTEMPTS} numbered variants all exist",
                first.display()
            )
        })
}

fn ask_name() -> Result<Option<String>> {
    let field = format!("name:text:{}:{DEFAULT_NAME}", gettext("File name"));
    let Some(answer) = notify::ask_form(
        &gettext("Image to PDF"),
        &gettext("Name of the PDF:"),
        &["--field".to_string(), field],
    )
    .map_err(anyhow::Error::msg)
    .context("asking for the file name")?
    else {
        return Ok(None);
    };

    clean_name(answer.get("name").map_or("", String::as_str))
}

/// What was typed, as a file name stem. Empty means the user cancelled, as it did in the shell
/// script's input box; a name with a path in it would write somewhere the dialog never said.
fn clean_name(typed: &str) -> Result<Option<String>> {
    let typed = typed.trim();
    let stem = match typed.len().checked_sub(4).and_then(|cut| typed.get(cut..)) {
        Some(tail) if tail.eq_ignore_ascii_case(".pdf") => &typed[..typed.len() - 4],
        _ => typed,
    };

    if stem.is_empty() {
        return Ok(None);
    }

    if stem.contains(['/', '\0']) || stem == "." || stem == ".." {
        bail!("the file name {stem:?} is not a plain name");
    }

    Ok(Some(stem.to_string()))
}

/// ImageMagick writes into a partial that is renamed once it has finished, so a PDF that looks
/// complete never is not. `pdf:` names the format, because `<name>.pdf.partial` does not say it.
fn convert(inputs: &[PathBuf], output: &Path) -> Result<()> {
    let mut partial = output.as_os_str().to_owned();
    partial.push(".partial");

    let partial = PathBuf::from(partial);
    let mut target = OsString::from("pdf:");
    target.push(&partial);

    let result = Command::new("magick")
        .args(inputs)
        .arg(&target)
        .stdin(Stdio::null())
        .output()
        .context("starting magick")?;

    if !result.status.success() {
        let _ = fs::remove_file(&partial);

        bail!(
            "magick failed with {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }

    place(&partial, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_free_name_is_the_plain_one() {
        let directory = Path::new("/pictures");

        assert_eq!(
            free_name(directory, "photo", |_| false).unwrap(),
            Path::new("/pictures/photo.pdf")
        );
    }

    #[test]
    fn a_taken_name_gets_a_number_instead_of_being_overwritten() {
        let directory = Path::new("/pictures");
        let taken = |path: &Path| {
            path == Path::new("/pictures/photo.pdf") || path == Path::new("/pictures/photo.2.pdf")
        };

        assert_eq!(
            free_name(directory, "photo", taken).unwrap(),
            Path::new("/pictures/photo.3.pdf")
        );
    }

    #[test]
    fn it_gives_up_when_every_name_is_taken() {
        assert!(free_name(Path::new("/pictures"), "photo", |_| true).is_err());
    }

    #[test]
    fn a_typed_name_loses_its_pdf_suffix_and_its_spaces() {
        assert_eq!(clean_name("merged").unwrap().as_deref(), Some("merged"));
        assert_eq!(clean_name("  trip.PDF ").unwrap().as_deref(), Some("trip"));
        assert_eq!(clean_name("a.b").unwrap().as_deref(), Some("a.b"));
    }

    #[test]
    fn an_empty_name_is_a_cancel() {
        assert_eq!(clean_name("").unwrap(), None);
        assert_eq!(clean_name("   ").unwrap(), None);
        assert_eq!(clean_name(".pdf").unwrap(), None);
    }

    #[test]
    fn a_name_with_a_path_in_it_is_refused() {
        assert!(clean_name("../elsewhere").is_err());
        assert!(clean_name("sub/dir").is_err());
        assert!(clean_name("..").is_err());
    }

    #[test]
    fn the_file_sits_next_to_the_image() {
        assert_eq!(
            directory_of(Path::new("/pictures/a.jpg")),
            Path::new("/pictures")
        );
        assert_eq!(directory_of(Path::new("a.jpg")), Path::new("."));
    }
}
