// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;

use anyhow::{Context, Result, bail};

use crate::notify::{self, Level};

/// How many lines of ffmpeg's stderr the log keeps when a run fails (spec §7).
const STDERR_TAIL: usize = 30;

/// The partial file of the running encode, so the Ctrl-C handler knows what to take away. It is the
/// only file these programs ever delete.
static PARTIAL: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Remove the partial file of the encode that is currently running, if there is one. Safe to call
/// from a signal handler and safe to call twice.
pub fn discard_partial() {
    let path = PARTIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();

    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
}

fn remember_partial(path: &Path) {
    *PARTIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path.to_path_buf());
}

/// The partial has been renamed to its final name; the signal handler has nothing left to take away.
pub fn forget_partial() {
    PARTIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Finished,
    Cancelled,
}

/// One ffmpeg run: what it reads, where it writes while it runs, and how long the result should be.
pub struct Job<'a> {
    pub input: &'a Path,
    pub partial: &'a Path,
    pub arguments: &'a [OsString],

    /// Seconds. Progress is a share of it, and a duration of 0 reports nothing.
    pub duration: f64,
}

/// Run ffmpeg into the partial file. Progress is reported as whole percent, once per change; the
/// callback answers `false` to stop the encode, which is what the progress dialog's Cancel button
/// comes back as.
pub fn run(job: &Job, on_progress: &mut dyn FnMut(u8) -> bool) -> Result<Outcome> {
    let mut child = Command::new("ffmpeg")
        .args(job.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting ffmpeg for {}", job.input.display()))?;

    remember_partial(job.partial);

    let stdout = child.stdout.take().context("ffmpeg's -progress pipe")?;
    let stderr = child.stderr.take().context("ffmpeg's stderr pipe")?;

    // ffmpeg's analysis and its errors go to stderr while `-progress` goes to stdout, so stderr
    // needs a reader of its own - a full pipe would stop the encode.
    let collector = thread::spawn(move || {
        let mut tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL);

        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tail.len() == STDERR_TAIL {
                tail.pop_front();
            }

            tail.push_back(line);
        }

        tail
    });

    let mut reported = 0;

    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(seconds) = parse_progress_line(&line) {
            let percent = progress_percent(seconds, job.duration);

            if percent != reported {
                reported = percent;

                if !on_progress(percent) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = collector.join();

                    discard_partial();

                    return Ok(Outcome::Cancelled);
                }
            }
        }
    }

    let status = child
        .wait()
        .with_context(|| format!("waiting for ffmpeg on {}", job.input.display()))?;
    let tail: Vec<String> = collector.join().unwrap_or_default().into();

    if !status.success() {
        discard_partial();

        notify::log(
            Level::Error,
            &format!(
                "ffmpeg failed on {} with {status}\n{}\n{}",
                job.input.display(),
                command_line(job.arguments),
                tail.join("\n")
            ),
        );

        bail!(
            "ffmpeg failed with {status}, see {}",
            notify::log_path().display()
        );
    }

    Ok(Outcome::Finished)
}

/// Give the finished partial its final name without ever replacing a file that is already there.
/// `fs::rename` would overwrite one silently, and a minute or more of encoding sits between the
/// moment the name was picked and this call - time enough for a second run, or the user, to take
/// it. `hard_link` fails with "file exists" instead, and since both paths sit in one directory it
/// is the same filesystem every time; removing the partial afterwards leaves the data under the
/// new name.
pub fn place(partial: &Path, output: &Path) -> Result<()> {
    fs::hard_link(partial, output).with_context(|| {
        format!(
            "putting the result in place as {} - something else created that file while it ran",
            output.display()
        )
    })?;

    fs::remove_file(partial)
        .with_context(|| format!("removing {} after the rename", partial.display()))?;

    forget_partial();

    Ok(())
}

/// `ffmpeg` plus its arguments as one line, quoted well enough to paste into a shell.
pub fn command_line(arguments: &[OsString]) -> String {
    let mut line = String::from("ffmpeg");

    for argument in arguments {
        let text = argument.to_string_lossy();

        line.push(' ');

        if text.contains(|character: char| character.is_whitespace() || character == '\'') {
            line.push_str(&format!("'{}'", text.replace('\'', r"'\''")));
        } else {
            line.push_str(&text);
        }
    }

    line
}

/// Read the position out of one `-progress` line. ffmpeg prints `out_time_us=N/A` before the first
/// frame is out.
pub fn parse_progress_line(line: &str) -> Option<f64> {
    let microseconds: u64 = line.strip_prefix("out_time_us=")?.trim().parse().ok()?;

    Some(microseconds as f64 / 1_000_000.0)
}

pub fn progress_percent(position: f64, duration: f64) -> u8 {
    if duration <= 0.0 {
        return 0;
    }

    (position / duration * 100.0).clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_position_from_the_progress_pipe() {
        assert_eq!(parse_progress_line("out_time_us=1000000"), Some(1.0));
        assert_eq!(parse_progress_line("out_time_us=N/A"), None);
        assert_eq!(parse_progress_line("out_time_ms=1000000"), None);
        assert_eq!(parse_progress_line("progress=end"), None);
    }

    #[test]
    fn the_percentage_stays_inside_its_bounds() {
        assert_eq!(progress_percent(0.0, 5189.8), 0);
        assert_eq!(progress_percent(2594.9, 5189.8), 50);
        assert_eq!(progress_percent(9999.0, 5189.8), 100);
        assert_eq!(progress_percent(10.0, 0.0), 0);
    }

    #[test]
    fn a_file_that_appeared_during_the_encode_is_not_overwritten() {
        let directory =
            std::env::temp_dir().join(format!("media-core-place-{}", std::process::id()));

        fs::create_dir_all(&directory).expect("a directory to test in");

        let partial = directory.join("film.av1.mkv.partial");
        let output = directory.join("film.av1.mkv");

        fs::write(&partial, b"the encode").expect("the partial");
        fs::write(&output, b"something else").expect("the file in the way");

        assert!(place(&partial, &output).is_err());
        assert_eq!(fs::read(&output).unwrap_or_default(), b"something else");
        assert!(partial.exists(), "the encode must survive a refused move");

        fs::remove_file(&output).expect("clearing the way");

        assert!(place(&partial, &output).is_ok());
        assert_eq!(fs::read(&output).unwrap_or_default(), b"the encode");
        assert!(!partial.exists());

        let _ = fs::remove_dir_all(&directory);
    }
}
