// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::app;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Level::Info => app::get().icon,
            Level::Warn => "dialog-warning",
            Level::Error => "dialog-error",
        }
    }
}

/// Append one entry to the log. Continuation lines are indented, which is how `Scripts/logs.py`
/// tells a multi-line entry - a plan, an ffmpeg command, a stderr tail - from the next event.
pub fn log(level: Level, message: &str) {
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut entry = String::new();

    for (index, line) in message.lines().enumerate() {
        if index == 0 {
            entry.push_str(&format!("[{timestamp}] {} {line}\n", level.label()));
        } else {
            entry.push_str(&format!("    {line}\n"));
        }
    }

    let path = log_path();

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // A log that cannot be written must not take the run down with it.
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(entry.as_bytes()));
}

/// `$<PREFIX>_LOG`, else `$XDG_STATE_HOME/<name>/<name>.log`.
pub fn log_path() -> PathBuf {
    let app = app::get();

    if let Some(override_path) = env::var_os(format!("{}_LOG", app.env_prefix)) {
        return PathBuf::from(override_path);
    }

    let state = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));

    state.join(app.name).join(format!("{}.log", app.name))
}

/// Started from Dolphin there is no terminal, so this is the only thing the user sees.
pub fn notify(enabled: bool, summary: &str, body: &str, icon: &str) {
    if !enabled {
        return;
    }

    let sent = Command::new("notify-send")
        .args(["-a", app::get().name, "-i", icon, summary, body])
        .status()
        .is_ok_and(|status| status.success());

    if !sent {
        let _ = Command::new("kdialog")
            .args(["--passivepopup", &format!("{summary}\n{body}"), "10"])
            .status();
    }
}

/// Where an outcome goes: the log in English, and the progress window's details area in the
/// language of the session, which is the one place a run started from Dolphin is being watched.
///
/// A notification only when there is no window - it could not be started, or ffprobe failed before
/// it was opened. Started from Dolphin there is no terminal, so an outcome nobody is told about is
/// one nobody sees.
pub fn report(
    dialog: Option<&Progress>,
    notifications: bool,
    level: Level,
    logged: &str,
    shown: &str,
) {
    log(level, logged);

    match dialog {
        Some(dialog) => dialog.detail(shown),
        None => notify(notifications, &app::get().title, shown, level.icon()),
    }
}

/// A `kurodlg --form` window, `arguments` being its `--field` and `--preset` options. `Ok(None)`
/// means the user backed out; `Err` means there was no form to back out of.
///
/// kurodlg exits 1 on Cancel, printing nothing - and so does Qt's option parser on an option it does
/// not know, which is what a kurodlg older than `--preset` says. Only the latter writes to stderr,
/// and read as a cancel it would end every run without converting and without a word.
pub fn ask_form(
    title: &str,
    text: &str,
    arguments: &[String],
) -> Result<Option<HashMap<String, String>>, String> {
    let binary = kurodlg_binary();
    let answer = Command::new(&binary)
        .args(["--title", title, "--form", text])
        .args(arguments)
        .output()
        .map_err(|error| format!("cannot run {}: {error}", binary.display()))?;
    let complaint = String::from_utf8_lossy(&answer.stderr).trim().to_string();

    match answer.status.code() {
        Some(0) => Ok(Some(parse_form(&String::from_utf8_lossy(&answer.stdout)))),
        Some(1) if complaint.is_empty() => Ok(None),
        _ => Err(format!(
            "{} refused the form ({}): {complaint}",
            binary.display(),
            answer.status
        )),
    }
}

/// kurodlg prints one `NAME=value` line per field. The value may hold a `=` of its own; the name
/// cannot.
pub fn parse_form(printed: &str) -> HashMap<String, String> {
    printed
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// The `kurodlg --progress` window, driven over D-Bus. It is the only progress a run started from
/// Dolphin can show, and it is also the only way to cancel one.
///
/// kurodlg rather than kdialog because kdialog's progress dialog is a QProgressDialog: it sizes
/// itself and takes no details area. The interface below is kdialog's, member for member, which is
/// why the swap is the two strings it looks like.
pub struct Progress {
    proxy: zbus::blocking::Proxy<'static>,
}

/// Where kurodlg is.
///
/// A process started from a service menu does not get `~/.local/bin` in its PATH - Dolphin's is
/// `/usr/local/bin:/usr/bin:/bin:…` - so the bare name resolves in a terminal and nowhere else,
/// which is why the progress window was missing from every run started the way this program is
/// meant to be started. Both binaries install into the same directory, so the one next to this
/// executable is the right one; the bare name stays as the fallback for a `cargo run` out of
/// `target/`, where there is no sibling.
fn kurodlg_binary() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|directory| directory.join("kurodlg")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("kurodlg"))
}

pub fn progress_open(enabled: bool, title: &str, label: &str) -> Option<Progress> {
    if !enabled {
        return None;
    }

    match progress_start(title, label) {
        Ok(progress) => Some(progress),
        Err(reason) => {
            // A missing progress window is not worth losing an encode over, but it must not be
            // invisible either: this returned None without a word until 2026/09/22, and no run
            // could be read back to find out why.
            log(Level::Warn, &format!("no progress window: {reason}"));

            None
        }
    }
}

fn progress_start(title: &str, label: &str) -> Result<Progress, String> {
    let binary = kurodlg_binary();
    let started = Command::new(&binary)
        .args(["--title", title, "--progress", label, "100"])
        .output()
        .map_err(|error| format!("cannot run {}: {error}", binary.display()))?;

    // kurodlg answers with the bus name and the object path as two fields on one line,
    // "net.kurokyuu.kurodlg-118245 /ProgressDialog".
    let printed = String::from_utf8_lossy(&started.stdout);
    let mut fields = printed.split_whitespace();
    let service = fields
        .next()
        .ok_or_else(|| {
            format!(
                "{} printed no bus name: {}",
                binary.display(),
                String::from_utf8_lossy(&started.stderr).trim()
            )
        })?
        .to_string();
    let path = fields.next().unwrap_or("/ProgressDialog").to_string();

    let connection = zbus::blocking::Connection::session()
        .map_err(|error| format!("no session bus: {error}"))?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        service,
        path,
        "net.kurokyuu.kurodlg.ProgressDialog",
    )
    .map_err(|error| format!("cannot reach the dialog on the bus: {error}"))?;

    let _ = proxy.call_method("showCancelButton", &(true,));

    // Open, not merely present: what the run reports goes in there, and a disclosure triangle
    // nobody notices is the same as no report at all.
    let _ = proxy.call_method("setDetailsVisible", &(true,));

    Ok(Progress { proxy })
}

impl Progress {
    pub fn set(&self, percent: u8, label: &str) {
        let _ = self.proxy.set_property("value", i32::from(percent));
        let _ = self.proxy.call_method("setLabelText", &(label,));
    }

    /// Only the dialog saying so counts as a cancel. A call that fails means it is gone -
    /// closed, crashed, or a hiccup on the bus - and none of that is a reason to throw away an
    /// encode that is running perfectly well.
    pub fn cancelled(&self) -> bool {
        self.proxy
            .call::<_, _, bool>("wasCancelled", &())
            .unwrap_or(false)
    }

    pub fn detail(&self, text: &str) {
        let _ = self.proxy.call_method("appendDetail", &(text,));
    }

    /// The run is over and the window stays up under a Close button, because what it collected is
    /// only worth collecting if it is still there to be read. This process may exit; the dialog is
    /// one of its own.
    ///
    /// `complete` fills the bar: ffmpeg's last progress line lands a percent or two short, and a
    /// window left standing at 99 % reads as a run that stopped. A run that failed or was cancelled
    /// keeps the bar where it was, which is where it actually got to.
    pub fn finished(&self, label: &str, complete: bool) {
        if complete {
            let _ = self.proxy.set_property("value", i32::from(100u8));
        }

        let _ = self.proxy.call_method("setLabelText", &(label,));
        let _ = self.proxy.call_method("setFinished", &());
    }
}

pub fn format_size(bytes: u64) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    const MIB: f64 = (1u64 << 20) as f64;

    let bytes = bytes as f64;

    if bytes >= GIB {
        format!("{:.2} GiB", bytes / GIB)
    } else {
        format!("{:.0} MiB", bytes / MIB)
    }
}

/// The line the success notification carries: `4.27 GiB -> 1.23 GiB (-71 %)`.
pub fn size_change(input: u64, output: u64) -> String {
    let percent = if input == 0 {
        0.0
    } else {
        (output as f64 - input as f64) / input as f64 * 100.0
    };

    format!(
        "{} -> {} ({percent:+.0} %)",
        format_size(input),
        format_size(output)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_lines_the_form_prints() {
        let answer = parse_form("preset=DVD film\ncq=28\nopus=off\nout=a=b\n\n");

        assert_eq!(answer["preset"], "DVD film");
        assert_eq!(answer["cq"], "28");
        assert_eq!(answer["opus"], "off");
        assert_eq!(answer["out"], "a=b");
        assert_eq!(answer.len(), 4);
        assert!(parse_form("").is_empty());
    }

    #[test]
    fn sizes_read_like_the_file_manager_shows_them() {
        assert_eq!(format_size(4_585_017_344), "4.27 GiB");
        assert_eq!(format_size(1_320_702_443), "1.23 GiB");
        assert_eq!(format_size(751_828_992), "717 MiB");
    }

    #[test]
    fn the_change_carries_its_sign() {
        assert_eq!(
            size_change(4_585_017_344, 1_320_702_443),
            "4.27 GiB -> 1.23 GiB (-71 %)"
        );
        assert!(size_change(1_000_000_000, 1_500_000_000).contains("+50 %"));
        assert!(size_change(0, 0).contains("+0 %"));
    }
}
