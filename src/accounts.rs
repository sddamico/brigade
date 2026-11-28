//! What brigade remembers about each account between runs: whether it's
//! limited and until when, and how close the harness last said it was to a
//! limit. Runs on a limited account end or wait without spending a call.

use crate::settings::Settings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

/// How long an account whose harness gave no reset time counts as limited
/// before brigade lets one run try it again. Each try that's limited too
/// doubles it, up to the cap.
const FIRST_BACKOFF_SECS: u64 = 30 * 60;
const MAX_BACKOFF_SECS: u64 = 8 * 60 * 60;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    /// A limit on the whole account, which only a harness that says so sets:
    /// Claude Code's five-hour and seven-day windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<Limit>,
    /// Limits on one model, by model id. Usage can run out for one model and
    /// not another, as when agy credits still cover a cheaper model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, Limit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl AccountState {
    /// The limit that keeps a run on `model` from starting now, if any.
    pub fn limit_on(&self, model: &str, now: u64) -> Option<&Limit> {
        [self.limit.as_ref(), self.models.get(model)]
            .into_iter()
            .flatten()
            .find(|limit| now < limit.free_at())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limit {
    pub since: u64,
    /// When the harness said the limit resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
    /// With no reset time, when one run may try the account again.
    pub retry_at: u64,
    pub backoff_secs: u64,
    /// The harness's own message about the limit.
    pub message: String,
}

impl Limit {
    /// When the account can take runs again, as far as brigade knows.
    pub fn free_at(&self) -> u64 {
        self.until.unwrap_or(self.retry_at)
    }
}

/// The harness's latest word on how much of a limit window the account has
/// used, which only Claude Code reports with every run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub at: u64,
    /// The harness's status: allowed, allowed_warning or rejected.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

/// Holds an account's lock, so only one brigade process at a time reads and
/// changes its state or claims one of its slots.
pub struct AccountLock {
    _file: fs::File,
}

pub fn lock(settings: &Settings, account: &str) -> Result<AccountLock, String> {
    let dir = accounts_dir(settings);
    fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{account}.lock"));
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("can't open {}: {e}", path.display()))?;
    // SAFETY: flock on a file descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!("can't lock {}", path.display()));
    }
    Ok(AccountLock { _file: file })
}

pub fn load(settings: &Settings, account: &str) -> AccountState {
    fs::read_to_string(state_file(settings, account))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The accounts brigade has state for.
pub fn known(settings: &Settings) -> Vec<String> {
    let Ok(entries) = fs::read_dir(accounts_dir(settings)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(String::from))
        .collect();
    names.sort();
    names
}

fn save(settings: &Settings, account: &str, state: &AccountState) -> Result<(), String> {
    let path = state_file(settings, account);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(state).expect("states serialize"))
        .and_then(|_| fs::rename(&tmp, &path))
        .map_err(|e| format!("can't write {}: {e}", path.display()))
}

/// Records that a run on `account` hit a usage limit: on `model` alone, or on
/// the whole account when `model` is None.
pub fn record_limit(
    settings: &Settings,
    account: &str,
    model: Option<&str>,
    message: &str,
    until: Option<u64>,
    now: u64,
) -> Result<(), String> {
    let _lock = lock(settings, account)?;
    let mut state = load(settings, account);
    let slot = match model {
        Some(model) => state
            .models
            .entry(model.to_string())
            .or_insert_with(|| placeholder(now)),
        None => state.limit.get_or_insert_with(|| placeholder(now)),
    };
    let previous = (slot.retry_at != 0).then(|| slot.clone());
    let (backoff_secs, retry_at) = match &previous {
        // The one run let through after the backoff was limited too.
        Some(limit) if limit.until.is_none() && now >= limit.retry_at => {
            let backoff = (limit.backoff_secs * 2).min(MAX_BACKOFF_SECS);
            (backoff, now + backoff)
        }
        // Runs already going when the limit hit find it too; they don't move the retry.
        Some(limit) if now < limit.free_at() => (limit.backoff_secs, limit.retry_at),
        _ => (FIRST_BACKOFF_SECS, now + FIRST_BACKOFF_SECS),
    };
    *slot = Limit {
        since: previous.map_or(now, |l| l.since),
        until,
        retry_at: until.unwrap_or(retry_at),
        backoff_secs,
        message: message.to_string(),
    };
    save(settings, account, &state)
}

/// Where a limit not recorded before goes, marked by its zero retry time.
fn placeholder(now: u64) -> Limit {
    Limit {
        since: now,
        until: None,
        retry_at: 0,
        backoff_secs: 0,
        message: String::new(),
    }
}

/// Records that a run on `model` got through, so neither it nor the account is
/// limited.
pub fn record_success(settings: &Settings, account: &str, model: &str) -> Result<(), String> {
    let _lock = lock(settings, account)?;
    let mut state = load(settings, account);
    let cleared = state.limit.take().is_some() | state.models.remove(model).is_some();
    if cleared {
        save(settings, account, &state)?;
    }
    Ok(())
}

pub fn record_usage(settings: &Settings, account: &str, usage: Usage) -> Result<(), String> {
    let _lock = lock(settings, account)?;
    let mut state = load(settings, account);
    state.usage = Some(usage);
    save(settings, account, &state)
}

/// Forgets a limit the user knows is over, such as agy credits topped up.
pub fn clear(settings: &Settings, account: &str) -> Result<bool, String> {
    let _lock = lock(settings, account)?;
    let mut state = load(settings, account);
    let had = state.limit.take().is_some() | !state.models.is_empty();
    state.models.clear();
    save(settings, account, &state)?;
    Ok(had)
}

fn accounts_dir(settings: &Settings) -> PathBuf {
    settings.state_dir.join("accounts")
}

fn state_file(settings: &Settings, account: &str) -> PathBuf {
    accounts_dir(settings).join(format!("{account}.json"))
}

/// Codex says when a limit resets only in its message: "… or try again at Nov
/// 27th, 2026 12:09 AM." That's local time.
pub fn reset_in_message(message: &str) -> Option<u64> {
    let at = message.split("try again at ").nth(1)?;
    let words: Vec<&str> = at.split_whitespace().take(5).collect();
    let [month, day, year, clock, meridiem] = words.as_slice() else {
        return None;
    };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS.iter().position(|m| month.starts_with(m))?;
    let day: i32 = day
        .trim_end_matches(',')
        .trim_end_matches(|c: char| c.is_alphabetic())
        .parse()
        .ok()?;
    let year: i32 = year.parse().ok()?;
    let (hour, minute) = clock.split_once(':')?;
    let (hour, minute): (i32, i32) = (hour.parse().ok()?, minute.parse().ok()?);
    let hour = match meridiem.trim_end_matches('.') {
        "AM" => hour % 12,
        "PM" => hour % 12 + 12,
        _ => return None,
    };
    local_epoch(year, month as i32, day, hour, minute)
}

fn local_epoch(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> Option<u64> {
    // SAFETY: an all-zero tm is valid, and mktime only reads and normalizes it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_isdst = -1;
    let epoch = unsafe { libc::mktime(&mut tm) };
    u64::try_from(epoch).ok()
}

/// A time as the user's clock shows it: "2026-11-27 00:09 EST".
pub fn local_time(epoch: u64) -> String {
    let secs = epoch as libc::time_t;
    // SAFETY: localtime_r and strftime write only into the buffers given.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        let mut buf = [0u8; 64];
        let len = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%Y-%m-%d %H:%M %Z".as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..len]).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(dir: &std::path::Path) -> Settings {
        Settings::from_env(|name| match name {
            "HOME" => Some(dir.display().to_string()),
            "XDG_STATE_HOME" => Some(dir.join("state").display().to_string()),
            _ => None,
        })
        .unwrap()
    }

    #[test]
    fn reads_codexs_reset_time_as_local_time() {
        let message = "You’ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/settings/usage to purchase more credits or try again at Nov 27th, 2026 12:09 AM.";
        let at = reset_in_message(message).unwrap();
        assert_eq!(local_time(at)[..16], *"2026-11-27 00:09");
        let pm = reset_in_message("try again at Mar 3rd, 2027 1:05 PM.").unwrap();
        assert_eq!(local_time(pm)[..16], *"2027-03-03 13:05");
        assert_eq!(reset_in_message("You've hit your session limit · resets 1am"), None);
    }

    #[test]
    fn an_unknown_reset_backs_off_and_doubles_when_the_retry_is_limited_too() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings(dir.path());
        record_limit(&settings, "agy", Some("hydrogen"), "credits too low", None, 1_000).unwrap();
        let first = load(&settings, "agy").models["hydrogen"].clone();
        assert_eq!(
            (first.retry_at, first.backoff_secs),
            (1_000 + FIRST_BACKOFF_SECS, FIRST_BACKOFF_SECS)
        );

        // Another run limited before the retry time doesn't stretch it.
        record_limit(&settings, "agy", Some("hydrogen"), "credits too low", None, 1_500).unwrap();
        assert_eq!(load(&settings, "agy").models["hydrogen"].retry_at, first.retry_at);

        let retried = first.retry_at + 1;
        record_limit(&settings, "agy", Some("hydrogen"), "credits too low", None, retried).unwrap();
        let second = load(&settings, "agy").models["hydrogen"].clone();
        assert_eq!(second.backoff_secs, FIRST_BACKOFF_SECS * 2);
        assert_eq!(
            (second.retry_at, second.since),
            (retried + FIRST_BACKOFF_SECS * 2, 1_000)
        );

        let state = load(&settings, "agy");
        assert_eq!(state.limit_on("hydrogen", retried), Some(&second));
        assert_eq!(state.limit_on("neon", retried), None, "other models aren't limited");

        record_success(&settings, "agy", "hydrogen").unwrap();
        assert!(load(&settings, "agy").models.is_empty());
    }

    #[test]
    fn a_known_reset_is_when_the_account_frees_up() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings(dir.path());
        record_limit(&settings, "codex", None, "try again later", Some(9_000), 1_000).unwrap();
        let state = load(&settings, "codex");
        assert_eq!(state.limit_on("any-model", 1_000).map(Limit::free_at), Some(9_000));
        assert_eq!(state.limit_on("any-model", 9_000), None, "free once the reset passes");
        assert_eq!(known(&settings), ["codex"]);
        assert!(clear(&settings, "codex").unwrap());
        assert_eq!(load(&settings, "codex").limit, None);
    }
}
