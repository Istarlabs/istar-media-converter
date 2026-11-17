// SPDX-FileCopyrightText: 2026 SavageCore
// SPDX-FileCopyrightText: 2026 Pascal Klose
// SPDX-License-Identifier: MIT

use std::process::Command;

use media_core::notify::{self, Level};

pub struct KResult {
    pub code: i32,
    pub stdout: String,
}

pub fn kdialog(args: &[&str]) -> KResult {
    match Command::new("kdialog").args(args).output() {
        Ok(out) => KResult {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        },
        Err(_) => KResult {
            code: -1,
            stdout: String::new(),
        },
    }
}

/// A desktop notification, and the same words in the log: started from Dolphin there is no
/// terminal, so the log is the only record of a run once the popup is gone.
pub fn notify(title: &str, msg: &str, icon: &str) {
    let level = match icon {
        "dialog-error" => Level::Error,
        "dialog-cancel" | "dialog-warning" => Level::Warn,
        _ => Level::Info,
    };

    notify::log(level, &format!("{title}: {msg}"));
    notify::notify(true, title, msg, icon);
}

pub struct ProgressHandle {
    proxy: zbus::blocking::Proxy<'static>,
}

pub fn pbar_open(title: &str, label: &str) -> Option<ProgressHandle> {
    // kdialog prints "<service> <object-path>" for its progress dialog.
    let r = kdialog(&["--title", title, "--progressbar", label, "100"]);
    let mut parts = r.stdout.split_whitespace();
    let service = parts.next()?.to_string();
    let path = parts.next().unwrap_or("/ProgressDialog").to_string();

    let conn = zbus::blocking::Connection::session().ok()?;
    let proxy =
        zbus::blocking::Proxy::new(&conn, service, path, "org.kde.kdialog.ProgressDialog").ok()?;
    Some(ProgressHandle { proxy })
}

pub fn pbar_set(handle: &Option<ProgressHandle>, value: i32, label: Option<&str>) -> bool {
    let Some(h) = handle else { return true };
    // A failed call means the dialog is gone -> user hit Cancel / closed it.
    if h.proxy.set_property("value", value).is_err() {
        return false;
    }
    if let Some(label) = label {
        let _ = h.proxy.call_method("setLabelText", &(label,));
    }
    true
}

pub fn pbar_close(handle: &Option<ProgressHandle>) {
    if let Some(h) = handle {
        let _ = h.proxy.call_method("close", &());
    }
}
