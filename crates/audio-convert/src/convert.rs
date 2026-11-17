// SPDX-FileCopyrightText: 2026 SavageCore
// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: MIT

// Progress bar approach adapted from the yt-dlp KDE service menu by Fabio Mucciante:
//   - each ffmpeg writes progress to a temp file (NOT a pipe - a pipe blocks updates)
//   - workers poll that file into shared per-job progress slots
//   - the main thread aggregates the slots and drives the dialog over D-Bus;
//     a failed D-Bus call means the user closed the dialog -> all ffmpegs are killed

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::config::{load_config, quality_for};
use crate::formats::{Format, codec_category, extract_extension_for_codec, quality_label};
use crate::ui::{kdialog, notify, pbar_close, pbar_open, pbar_set};

// Runs a command with a hard deadline so a probe on a dead mount can't hang
// the whole tool. Returns None on timeout or spawn failure.
fn output_with_timeout(mut cmd: Command, timeout: Duration) -> Option<std::process::Output> {
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

struct ProbeInfo {
    codec: Option<String>,
    duration: Option<f64>,
}

// Codec of the first audio stream and container duration in one ffprobe call.
fn probe(filepath: &str) -> ProbeInfo {
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "a:0",
        "-show_entries",
        "stream=codec_name:format=duration",
        "-of",
        "default=noprint_wrappers=1",
        filepath,
    ]);

    let mut info = ProbeInfo {
        codec: None,
        duration: None,
    };
    let Some(out) = output_with_timeout(cmd, Duration::from_secs(30)) else {
        return info;
    };

    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(v) = line.strip_prefix("codec_name=") {
            let v = v.trim().to_lowercase();
            if !v.is_empty() {
                info.codec = Some(v);
            }
        } else if let Some(v) = line.strip_prefix("duration=") {
            info.duration = v.trim().parse::<f64>().ok().filter(|d| *d > 0.0);
        }
    }
    info
}

pub fn require_ffmpeg() -> bool {
    if which::which("ffmpeg").is_err() {
        kdialog(&[
            "--error",
            "<b>ffmpeg not found.</b><br><br>Install via your package manager:<br>\
             • <tt>sudo apt install ffmpeg</tt> - Debian / Ubuntu / Mint<br>\
             • <tt>sudo dnf install ffmpeg</tt> - Fedora<br>\
             • <tt>sudo pacman -S ffmpeg</tt> - Arch / Manjaro<br>\
             • <tt>sudo zypper install ffmpeg</tt> - openSUSE",
        ]);
        return false;
    }
    true
}

// Asks once per batch instead of once per file.
fn warn_if_lossy_batch(lossy_count: usize, total: usize, output_fmt: Format) -> bool {
    if lossy_count == 0 {
        return true;
    }

    let (title, body) = if output_fmt.is_lossy() {
        (
            "Lossy to lossy conversion",
            "Re-encoding between lossy formats permanently degrades quality. \
             A lossless source is strongly recommended.",
        )
    } else {
        (
            "Lossy to lossless conversion",
            "Wrapping lossy audio in a lossless container <b>will not recover quality</b> \
             - the output will simply be larger.",
        )
    };

    let msg = format!(
        "<b>{}</b><br><br>{} of {} input file(s) {} lossy-encoded.<br>\
         Target format: <i>{}</i><br><br>{}<br><br><b>Convert anyway?</b>",
        title,
        lossy_count,
        total,
        if lossy_count == 1 { "is" } else { "are" },
        output_fmt.key().to_uppercase(),
        body
    );
    let r = kdialog(&[
        "--title",
        "Audio Converter - Warning",
        "--warningyesno",
        msg.as_str(),
        "--yes-label",
        "Convert",
        "--no-label",
        "Cancel",
    ]);
    r.code == 0
}

// Finds the last "out_time_ms=<digits>" value in ffmpeg's -progress output file.
fn last_out_time_ms(content: &str) -> Option<u64> {
    content
        .lines()
        .filter_map(|l| l.strip_prefix("out_time_ms="))
        .filter_map(|v| v.trim().parse::<u64>().ok())
        .next_back()
}

fn short_name(name: &str) -> String {
    if name.chars().count() > 50 {
        let truncated: String = name.chars().take(50).collect();
        format!("{}…", truncated)
    } else {
        name.to_string()
    }
}

fn file_display_name(input_path: &Path, fallback: &str) -> String {
    input_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(fallback)
        .to_string()
}

fn output_path_for(input: &Path, fmt: Format) -> PathBuf {
    let suffix = fmt.extension();
    let mut candidate = input.with_extension(suffix);
    if candidate == input {
        let stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let new_stem = format!("{}_{}", stem, fmt.key());
        candidate = input.with_file_name(new_stem).with_extension(suffix);
    }
    candidate
}

struct Job {
    input: String,
    output: PathBuf,
    name: String,
    duration: Option<f64>,
    weight: f64,
}

enum JobResult {
    Success,
    Failed(String),
    Cancelled,
}

struct BatchOutcome {
    done: usize,
    cancelled: bool,
    errors: Vec<String>,
}

// Weight each job by its duration so the bar moves evenly across files of
// different lengths; files with an unknown duration get the average of the
// known ones (or 1.0 if none is known).
fn assign_weights(jobs: &mut [Job]) {
    let known: Vec<f64> = jobs.iter().filter_map(|j| j.duration).collect();
    let fallback = if known.is_empty() {
        1.0
    } else {
        known.iter().sum::<f64>() / known.len() as f64
    };
    for job in jobs {
        job.weight = job.duration.unwrap_or(fallback).max(0.1);
    }
}

// Runs one ffmpeg process, storing its progress (0..=1000 permille) in the
// shared slot and honoring the batch-wide cancel flag.
fn run_one(
    job: &Job,
    extra_args: &[String],
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> JobResult {
    // ffmpeg wants the file to already exist to append progress to it;
    // NamedTempFile creates it and deletes it again on drop.
    let prog_file = match tempfile::Builder::new().prefix("dac_prog_").tempfile() {
        Ok(f) => f,
        Err(e) => return JobResult::Failed(format!("failed to create progress file ({})", e)),
    };
    let prog_path = prog_file.path().to_path_buf();

    // stderr goes to a temp file instead of a pipe: a pipe that nobody reads
    // while ffmpeg runs can fill up and deadlock the child on a chatty error.
    let err_file = match tempfile::Builder::new().prefix("dac_err_").tempfile() {
        Ok(f) => f,
        Err(e) => return JobResult::Failed(format!("failed to create stderr file ({})", e)),
    };
    let err_handle = match err_file.reopen() {
        Ok(f) => f,
        Err(e) => return JobResult::Failed(format!("failed to open stderr file ({})", e)),
    };

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-i")
        .arg(&job.input)
        .arg("-progress")
        .arg(&prog_path)
        .arg("-nostats")
        .arg("-loglevel")
        .arg("error");
    for a in extra_args {
        cmd.arg(a);
    }
    cmd.arg(&job.output);
    cmd.stdout(Stdio::null())
        .stderr(Stdio::from(err_handle))
        .stdin(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return JobResult::Failed(format!("failed to start ffmpeg ({})", e)),
    };

    let mut was_cancelled = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(500));

        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            was_cancelled = true;
            break;
        }

        if let (Ok(content), Some(dur)) = (fs::read_to_string(&prog_path), job.duration)
            && let Some(us) = last_out_time_ms(&content)
        {
            let pct = ((us as f64 / 1e6) / dur).min(1.0);
            progress.store((pct * 1000.0) as u32, Ordering::Relaxed);
        }
    }

    let status = child.wait();

    if was_cancelled {
        let _ = fs::remove_file(&job.output);
        return JobResult::Cancelled;
    }

    if matches!(status, Ok(s) if s.success()) {
        progress.store(1000, Ordering::Relaxed);
        JobResult::Success
    } else {
        let stderr_text = fs::read_to_string(err_file.path()).unwrap_or_default();
        let _ = fs::remove_file(&job.output);
        JobResult::Failed(stderr_text.chars().take(400).collect())
    }
}

// Runs the jobs on a small worker pool while the main thread aggregates
// progress and watches the dialog for Cancel.
fn run_jobs(jobs: &[Job], extra_args: &[String], title: &str) -> BatchOutcome {
    if jobs.is_empty() {
        return BatchOutcome {
            done: 0,
            cancelled: false,
            errors: Vec::new(),
        };
    }

    let handle = pbar_open(title, &format!("Starting… (0 of {})", jobs.len()));

    let total_weight: f64 = jobs.iter().map(|j| j.weight).sum::<f64>().max(0.001);
    let progress: Vec<AtomicU32> = jobs.iter().map(|_| AtomicU32::new(0)).collect();
    let results: Mutex<Vec<Option<JobResult>>> =
        Mutex::new((0..jobs.len()).map(|_| None).collect());
    let next_job = AtomicUsize::new(0);
    let cancel = AtomicBool::new(false);
    let workers_done = AtomicUsize::new(0);

    // ponytail: cap at 4 workers - audio encoders are single-threaded, more
    // mostly contends on I/O; raise the cap if huge batches ever feel slow.
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(4)
        .min(jobs.len());

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next_job.fetch_add(1, Ordering::SeqCst);
                    if i >= jobs.len() || cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    let result = run_one(&jobs[i], extra_args, &progress[i], &cancel);
                    results.lock().unwrap()[i] = Some(result);
                }
                workers_done.fetch_add(1, Ordering::SeqCst);
            });
        }

        // Main thread: aggregate progress and ping the dialog every tick - a
        // failed call is the only way to notice the user hit Cancel.
        while workers_done.load(Ordering::SeqCst) < workers {
            std::thread::sleep(Duration::from_millis(500));

            let done_weight: f64 = jobs
                .iter()
                .zip(&progress)
                .map(|(j, p)| j.weight * p.load(Ordering::Relaxed) as f64 / 1000.0)
                .sum();
            let pct = ((done_weight / total_weight) * 100.0).min(100.0) as i32;
            let completed = results
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.is_some())
                .count();

            let label = format!("Processing… ({} of {} done)", completed, jobs.len());
            if !pbar_set(&handle, pct, Some(&label)) {
                cancel.store(true, Ordering::SeqCst);
            }
        }
    });

    pbar_close(&handle);

    let mut done = 0;
    let mut errors = Vec::new();
    for (job, slot) in jobs.iter().zip(results.into_inner().unwrap()) {
        match slot {
            Some(JobResult::Success) => done += 1,
            Some(JobResult::Failed(msg)) => {
                errors.push(format!("{}:\n{}", short_name(&job.name), msg))
            }
            Some(JobResult::Cancelled) | None => {}
        }
    }

    BatchOutcome {
        done,
        cancelled: cancel.into_inner(),
        errors,
    }
}

// Final error dialog / success notification shared by convert and extract.
// `verb` is the past-tense operation name ("Converted" / "Extracted").
fn report_results(done: usize, total: usize, errors: &[String], verb: &str, success_body: &str) {
    if !errors.is_empty() {
        let mut preview = errors
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n");
        if errors.len() > 3 {
            preview.push_str(&format!("\n\n…and {} more", errors.len() - 3));
        }
        let error_msg = format!(
            "{} {} of {} file(s).\n\nErrors:\n{}",
            verb, done, total, preview
        );
        media_core::notify::log(media_core::notify::Level::Error, &error_msg);
        kdialog(&[
            "--title",
            "Audio Converter - Errors",
            "--error",
            error_msg.as_str(),
        ]);
        notify(
            "Audio Converter - Finished with errors",
            &format!(
                "{}/{} {}, {} failed.",
                done,
                total,
                verb.to_lowercase(),
                errors.len()
            ),
            "dialog-error",
        );
    } else if done > 0 {
        notify("Audio Converter - Done", success_body, "audio-x-generic");
    }
}

fn notify_cancelled(done: usize, total: usize) {
    notify(
        "Audio Converter - Cancelled",
        &format!("Cancelled - {} of {} file(s) finished", done, total),
        "dialog-cancel",
    );
}

pub fn convert_files(files: &[String], fmt: Format, quality: &str) {
    // -vn is always safe here, even for audio-only inputs: we never want to
    // accidentally mux a video/thumbnail stream into an audio-only container.
    let mut ffmpeg_args = vec!["-vn".to_string()];
    ffmpeg_args.extend(fmt.ffmpeg_args(quality));

    let total = files.len();
    let mut errors: Vec<String> = Vec::new();
    let mut jobs: Vec<Job> = Vec::new();
    let mut lossy_count = 0;

    for filepath in files {
        let input_path = Path::new(filepath.as_str());
        let name = file_display_name(input_path, filepath);
        if !input_path.exists() {
            errors.push(format!("File not found: {}", filepath));
            continue;
        }

        let output = output_path_for(input_path, fmt);
        if output.exists() {
            errors.push(format!(
                "{}: output '{}' already exists - skipped",
                name,
                file_display_name(&output, "?")
            ));
            continue;
        }

        let info = probe(filepath);
        if info.codec.as_deref().map(codec_category) == Some("lossy") {
            lossy_count += 1;
        }
        jobs.push(Job {
            input: filepath.clone(),
            output,
            name,
            duration: info.duration,
            weight: 0.0,
        });
    }

    if !warn_if_lossy_batch(lossy_count, total, fmt) {
        return;
    }
    assign_weights(&mut jobs);

    let q_suffix = quality_label(quality);
    let title = format!("Audio Converter - {}{}", fmt.key().to_uppercase(), q_suffix);
    let outcome = run_jobs(&jobs, &ffmpeg_args, &title);

    if outcome.cancelled {
        notify_cancelled(outcome.done, total);
        return;
    }

    errors.extend(outcome.errors);
    let success_body = format!(
        "✔  {} file{} → {}{}",
        outcome.done,
        if outcome.done != 1 { "s" } else { "" },
        fmt.key().to_uppercase(),
        q_suffix
    );
    report_results(outcome.done, total, &errors, "Converted", &success_body);
}

pub fn extract_files(files: &[String]) {
    // Explicit -map so we copy exactly the stream we probed (a:0) - without
    // it ffmpeg picks the "best" audio stream, which may be a different one.
    let extract_args = vec![
        "-map".to_string(),
        "0:a:0".to_string(),
        "-c:a".to_string(),
        "copy".to_string(),
    ];

    let total = files.len();
    let mut errors: Vec<String> = Vec::new();
    let mut jobs: Vec<Job> = Vec::new();

    for filepath in files {
        let input_path = Path::new(filepath.as_str());
        let name = file_display_name(input_path, filepath);
        if !input_path.exists() {
            errors.push(format!("File not found: {}", filepath));
            continue;
        }

        let info = probe(filepath);
        let ext = info
            .codec
            .as_deref()
            .map(extract_extension_for_codec)
            .unwrap_or("mka");

        let mut output = input_path.with_extension(ext);
        if output == input_path {
            let stem = input_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("output");
            let new_stem = format!("{}_audio", stem);
            output = input_path.with_file_name(new_stem).with_extension(ext);
        }
        if output.exists() {
            errors.push(format!(
                "{}: output '{}' already exists - skipped",
                name,
                file_display_name(&output, "?")
            ));
            continue;
        }

        jobs.push(Job {
            input: filepath.clone(),
            output,
            name,
            duration: info.duration,
            weight: 0.0,
        });
    }

    assign_weights(&mut jobs);
    let outcome = run_jobs(&jobs, &extract_args, "Audio Converter - Extract");

    if outcome.cancelled {
        notify_cancelled(outcome.done, total);
        return;
    }

    errors.extend(outcome.errors);
    let success_body = format!(
        "✔  Extracted audio from {} file{}",
        outcome.done,
        if outcome.done != 1 { "s" } else { "" }
    );
    report_results(outcome.done, total, &errors, "Extracted", &success_body);
}

// Shows a format-picker menu (reusing the same style of dialog as Configure),
// then hands off to the normal transcoding path with the format's saved
// quality setting.
pub fn run_extract_convert(files: &[String]) {
    if files.is_empty() {
        kdialog(&["--error", "No input files provided."]);
        return;
    }

    let mut menu_args: Vec<String> = vec![
        "--title".into(),
        "Extract & Convert Audio".into(),
        "--menu".into(),
        "Select the target audio format:".into(),
    ];
    for f in Format::ALL {
        menu_args.push(f.key().to_string());
        menu_args.push(f.display().to_string());
    }
    let r = kdialog(&menu_args.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    if r.code != 0 {
        return;
    }
    let Some(chosen_fmt) = Format::from_key(r.stdout.trim()) else {
        return;
    };

    let cfg = load_config();
    convert_files(files, chosen_fmt, &quality_for(&cfg, chosen_fmt));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(duration: Option<f64>) -> Job {
        Job {
            input: String::new(),
            output: PathBuf::new(),
            name: String::new(),
            duration,
            weight: 0.0,
        }
    }

    #[test]
    fn the_last_progress_value_counts() {
        assert_eq!(
            last_out_time_ms("out_time_ms=1000\nprogress=continue\nout_time_ms=2500000\n"),
            Some(2_500_000)
        );
        assert_eq!(last_out_time_ms("out_time_ms=N/A\n"), None);
        assert_eq!(last_out_time_ms(""), None);
    }

    #[test]
    fn an_unknown_duration_weighs_as_much_as_the_average() {
        let mut jobs = [job(Some(100.0)), job(Some(300.0)), job(None)];

        assign_weights(&mut jobs);

        assert_eq!(jobs[0].weight, 100.0);
        assert_eq!(jobs[2].weight, 200.0);
    }

    #[test]
    fn without_any_known_duration_every_job_weighs_one() {
        let mut jobs = [job(None), job(None)];

        assign_weights(&mut jobs);

        assert_eq!(jobs[0].weight, 1.0);
    }

    #[test]
    fn the_output_replaces_the_extension() {
        assert_eq!(
            output_path_for(Path::new("/music/song.wav"), Format::Mp3),
            PathBuf::from("/music/song.mp3")
        );
    }

    #[test]
    fn the_output_never_lands_on_its_own_input() {
        assert_eq!(
            output_path_for(Path::new("/music/song.mp3"), Format::Mp3),
            PathBuf::from("/music/song_mp3.mp3")
        );
    }

    #[test]
    fn a_long_name_is_cut_for_the_error_list() {
        assert_eq!(short_name("short.mp3"), "short.mp3");
        assert_eq!(short_name(&"a".repeat(60)), format!("{}…", "a".repeat(50)));
    }
}
