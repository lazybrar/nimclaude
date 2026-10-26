// SPDX-License-Identifier: GPL-2.0-or-later

use std::{
    env, fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static ECHO: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn echo_to_stderr() {
    ECHO.store(true, Ordering::Relaxed);
}

pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn debug() -> bool {
    env::var("NIMCLAUDE_LOG").is_ok_and(|v| v == "debug")
}

pub fn path() -> PathBuf {
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("nimclaude").join("nimclaude.log")
}

pub fn log(message: &str) {
    let line = format!("{} {message}\n", timestamp());
    if ECHO.load(Ordering::Relaxed) {
        eprint!("{line}");
    }
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    rotate(&path);
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(line.as_bytes());
    }
}

pub fn clip(text: &str, max: usize) -> String {
    let flat = text.replace('\n', "\\n");
    match flat.char_indices().nth(max) {
        Some((end, _)) => format!("{}... ({} bytes)", &flat[..end], text.len()),
        None => flat,
    }
}

fn rotate(path: &PathBuf) {
    if fs::metadata(path).is_ok_and(|m| m.len() > 5_000_000) {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_long_text() {
        assert_eq!(clip("abc", 5), "abc");
        assert!(clip("abcdef", 3).starts_with("abc..."));
    }

    #[test]
    fn timestamp_has_iso_shape() {
        let t = timestamp();
        assert_eq!(t.len(), 20);
        assert!(t.starts_with("20") && t.ends_with('Z'));
    }
}
