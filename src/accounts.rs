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
    /// The quotas the harness last reported, when brigade was asked to read them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quotas: Vec<Quota>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotas_read_at: Option<u64>,
    /// The models the account's server said it had loaded, when last asked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serving: Vec<String>,
}

/// A usage window a set of models on an account share, as the harness reports
/// it: Claude Code's weekly windows, agy's model groups, Codex's five-hour and
/// weekly limits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    pub name: String,
    /// The share of the window used, from 0 to 1.
    pub used: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    /// The models it covers, by the harness's names for them; every model on
    /// the account when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
}

impl Quota {
    pub fn covers(&self, model: &str) -> bool {
        let model = comparable(model);
        self.models.as_ref().is_none_or(|models| {
            models.iter().any(|m| {
                let m = comparable(m);
                model == m || model.contains(&m)
            })
        })
    }

    fn used_up(&self) -> bool {
        self.used >= 1.0
    }
}

/// Harnesses spell one model differently in different places: agy's "Claude
/// Opus 5.5" is its `claude-opus-5-5`, and its "Gemini 3.8 Flash" is
/// `gemini-3.8-flash`.
fn comparable(name: &str) -> String {
    name.to_lowercase().replace(['.', ' ', '_'], "-")
}

impl AccountState {
    /// The limit that keeps a run on `model` from starting now, if any, and its
    /// model when it's on the model alone.
    pub fn limit_on(&self, model: &str, now: u64) -> Option<(Limit, Option<String>)> {
        if let Some(limit) = self.limit.as_ref().filter(|l| now < l.free_at()) {
            return Some((limit.clone(), None));
        }
        if let Some(limit) = self.models.get(model).filter(|l| now < l.free_at()) {
            return Some((limit.clone(), Some(model.to_string())));
        }
        self.quota_limit(model, now)
    }

    /// A used-up quota covering `model`, as a limit until it resets.
    fn quota_limit(&self, model: &str, now: u64) -> Option<(Limit, Option<String>)> {
        let read_at = self.quotas_read_at?;
        let quota = self
            .quotas
            .iter()
            .find(|q| q.used_up() && q.covers(model) && q.resets_at.unwrap_or(read_at + FIRST_BACKOFF_SECS) > now)?;
        let free_at = quota.resets_at.unwrap_or(read_at + FIRST_BACKOFF_SECS);
        let limit = Limit {
            since: read_at,
            until: quota.resets_at,
            retry_at: free_at,
            backoff_secs: 0,
            message: format!("{} is used up", quota.name),
            probe: None,
        };
        Some((limit, quota.models.is_some().then(|| model.to_string())))
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
    /// The one run let through to try the limit once its backoff ran out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<String>,
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
        probe: None,
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
        probe: None,
    }
}

pub enum Check {
    Clear,
    /// The limit holding the run back, and its model when it's on the model alone.
    Limited(Limit, Option<String>),
}

/// Whether a run on `model` may start now, for a caller holding the account's
/// lock. Once a backoff runs out, the first run to ask becomes the probe and
/// the rest stay held until it records how it went, unless `alive` says the
/// probe run has ended without doing so.
pub fn check(
    settings: &Settings,
    account: &str,
    model: &str,
    run: &str,
    now: u64,
    alive: &dyn Fn(&str) -> bool,
) -> Result<Check, String> {
    let mut state = load(settings, account);
    let mut changed = false;
    let mut held = None;
    for scope in [None, Some(model)] {
        let limit = match scope {
            None => state.limit.as_mut(),
            Some(model) => state.models.get_mut(model),
        };
        let Some(limit) = limit else { continue };
        let probing = limit.until.is_none() && limit.probe.as_deref().is_some_and(|probe| probe != run && alive(probe));
        if now < limit.free_at() || probing {
            held = Some((limit.clone(), scope.map(String::from)));
            break;
        }
        if limit.until.is_none() && limit.probe.as_deref() != Some(run) {
            limit.probe = Some(run.to_string());
            changed = true;
        }
    }
    if changed {
        save(settings, account, &state)?;
    }
    Ok(match held.or_else(|| state.quota_limit(model, now)) {
        Some((limit, scope)) => Check::Limited(limit, scope),
        None => Check::Clear,
    })
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

pub fn record_quotas(settings: &Settings, account: &str, quotas: Vec<Quota>, now: u64) -> Result<(), String> {
    let _lock = lock(settings, account)?;
    let mut state = load(settings, account);
    state.quotas = quotas;
    state.quotas_read_at = Some(now);
    save(settings, account, &state)
}

pub fn record_serving(settings: &Settings, account: &str, models: Vec<String>) -> Result<(), String> {
    let _lock = lock(settings, account)?;
    let mut state = load(settings, account);
    state.serving = models;
    save(settings, account, &state)
}

pub fn record_usage(settings: &Settings, account: &str, usage: Usage) -> Result<(), String> {
    let _lock = lock(settings, account)?;
    let mut state = load(settings, account);
    state.usage = Some(usage);
    save(settings, account, &state)
}

/// Forgets a limit the user knows is over, such as agy credits topped up.
/// `account` is a name brigade knows, checked by the caller, since it names a
/// file.
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
/// Codex's "try again at Nov 28th, 2026 1:26 AM.", in local time. A reset
/// later the same day comes without the date ("try again at 1:26 AM."), which
/// is the next time that clock time comes round after `now`.
pub fn reset_in_message(message: &str, now: u64) -> Option<u64> {
    let at = message.split("try again at ").nth(1)?;
    let words: Vec<&str> = at.split_whitespace().take(5).collect();
    if let [clock, meridiem, ..] = words.as_slice()
        && let Some((hour, minute)) = clock_time(clock, meridiem)
    {
        let (year, month, day) = local_date(now);
        let today = local_epoch(year, month, day, hour, minute)?;
        return if today > now {
            Some(today)
        } else {
            local_epoch(year, month, day + 1, hour, minute)
        };
    }
    let [month, day, year, clock, meridiem] = words.as_slice() else {
        return None;
    };
    let month = month_index(month)?;
    let day: i32 = day
        .trim_end_matches(',')
        .trim_end_matches(|c: char| c.is_alphabetic())
        .parse()
        .ok()?;
    let year: i32 = year.parse().ok()?;
    let (hour, minute) = clock_time(clock, meridiem)?;
    local_epoch(year, month, day, hour, minute)
}

/// "1:26" and "AM." as a 24-hour (hour, minute).
fn clock_time(clock: &str, meridiem: &str) -> Option<(i32, i32)> {
    let (hour, minute) = clock.split_once(':')?;
    let (hour, minute): (i32, i32) = (hour.parse().ok()?, minute.parse().ok()?);
    match meridiem.trim_end_matches('.') {
        "AM" => Some((hour % 12, minute)),
        "PM" => Some((hour % 12 + 12, minute)),
        _ => None,
    }
}

/// When a Claude Code `/usage` window resets: "Dec 2, 2am (America/New_York)",
/// or just "3:30pm (...)" for later today. The zone is the machine's own, which
/// is the one Claude Code reports in.
pub fn claude_reset(when: &str, now: u64) -> Option<u64> {
    let (when, zone) = match when.split_once(" (") {
        Some((when, zone)) => (when.trim(), zone.split(')').next()),
        None => (when.trim(), None),
    };
    // brigade reads times in this machine's zone; one given in another zone
    // would be off by the difference, so it isn't used.
    if let (Some(zone), Some(local)) = (zone, local_zone())
        && zone != local
    {
        return None;
    }
    let (date, clock) = match when.rsplit_once(", ") {
        Some((date, clock)) => (Some(date), clock),
        None => (None, when),
    };
    let clock = clock.to_lowercase();
    let (time, meridiem) = clock.split_at(clock.len().checked_sub(2)?);
    let (hour, minute) = match time.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None => (time.parse::<i32>().ok()?, 0),
    };
    let hour = match meridiem {
        "am" => hour % 12,
        "pm" => hour % 12 + 12,
        _ => return None,
    };
    let today = local_date(now);
    let (month, day) = match date {
        Some(date) => {
            let (month, day) = date.split_once(' ')?;
            (month_index(month)?, day.parse().ok()?)
        }
        None => (today.1, today.2),
    };
    let at = local_epoch(today.0, month, day, hour, minute)?;
    match (at < now, date) {
        (false, _) => Some(at),
        // A time with no date that's already past is tomorrow's.
        (true, None) => Some(at + 24 * 60 * 60),
        // A date that's already past is next year's.
        (true, Some(_)) => local_epoch(today.0 + 1, month, day, hour, minute),
    }
}

fn month_index(name: &str) -> Option<i32> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS.iter().position(|m| name.starts_with(m)).map(|i| i as i32)
}

/// The local (year, 0-based month, day) of an epoch time.
fn local_date(epoch: u64) -> (i32, i32, i32) {
    let secs = epoch as libc::time_t;
    // SAFETY: localtime_r writes only into the tm given.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm
    };
    (tm.tm_year + 1900, tm.tm_mon, tm.tm_mday)
}

/// An RFC 3339 UTC time, "2026-12-03T12:42:51Z", as agy reports resets.
pub fn utc_epoch(text: &str) -> Option<u64> {
    let (date, time) = text.trim_end_matches('Z').split_once('T')?;
    let mut date = date.split('-').map(|p| p.parse::<i32>());
    let mut time = time.split(['.', ':']).map(|p| p.parse::<i32>());
    // SAFETY: an all-zero tm is valid, and timegm only reads and normalizes it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = date.next()?.ok()? - 1900;
    tm.tm_mon = date.next()?.ok()? - 1;
    tm.tm_mday = date.next()?.ok()?;
    tm.tm_hour = time.next()?.ok()?;
    tm.tm_min = time.next()?.ok()?;
    tm.tm_sec = time.next()?.ok()?;
    u64::try_from(unsafe { libc::timegm(&mut tm) }).ok()
}

#[cfg(test)]
pub fn local_epoch_for_tests(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> u64 {
    local_epoch(year, month, day, hour, minute).unwrap()
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
/// This machine's time zone name ("America/New_York"), as the C library finds
/// it: from $TZ, or else from where /etc/localtime links into the zoneinfo
/// database. None when it can't tell.
fn local_zone() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        return Some(tz.trim_start_matches(':').to_string()).filter(|z| !z.is_empty());
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let target = target.to_string_lossy();
    target.split_once("zoneinfo/").map(|(_, zone)| zone.to_string())
}

/// "2026-11-28 00:43:07", for log lines.
pub fn local_timestamp(epoch: u64) -> String {
    let secs = epoch as libc::time_t;
    // SAFETY: localtime_r and strftime write only into the buffers given.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        let mut buf = [0u8; 64];
        let len = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%Y-%m-%d %H:%M:%S".as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..len]).into_owned()
    }
}

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
        let now = local_epoch_for_tests(2026, 10, 26, 23, 0);
        let at = reset_in_message(message, now).unwrap();
        assert_eq!(local_time(at)[..16], *"2026-11-27 00:09");
        let pm = reset_in_message("try again at Mar 3rd, 2027 1:05 PM.", now).unwrap();
        assert_eq!(local_time(pm)[..16], *"2027-03-03 13:05");
        assert_eq!(
            reset_in_message("You've hit your session limit · resets 1am", now),
            None
        );
        // Later the same day, Codex leaves the date out.
        let later = reset_in_message("purchase more credits or try again at 11:40 PM.", now).unwrap();
        assert_eq!(local_time(later)[..16], *"2026-11-26 23:40");
        let tomorrow = reset_in_message("or try again at 1:26 AM.", now).unwrap();
        assert_eq!(local_time(tomorrow)[..16], *"2026-11-27 01:26");
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
        assert_eq!(
            state.limit_on("hydrogen", retried),
            Some((second.clone(), Some("hydrogen".into())))
        );
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
        assert_eq!(
            state.limit_on("any-model", 1_000).map(|(l, _)| l.free_at()),
            Some(9_000)
        );
        assert_eq!(state.limit_on("any-model", 9_000), None, "free once the reset passes");
        assert_eq!(known(&settings), ["codex"]);
        assert!(clear(&settings, "codex").unwrap());
        assert_eq!(load(&settings, "codex").limit, None);
    }

    #[test]
    fn a_used_up_quota_limits_the_models_it_covers_until_it_resets() {
        let state = AccountState {
            quotas: vec![
                Quota {
                    name: "Gemini Models, weekly".into(),
                    used: 1.0,
                    resets_at: Some(5_000),
                    models: Some(vec!["gemini-4-neon".into(), "gemini-3.8-flash".into()]),
                },
                Quota {
                    name: "Claude and GPT models, 5h".into(),
                    used: 0.07,
                    resets_at: Some(4_000),
                    models: Some(vec!["claude-opus-5.5".into()]),
                },
            ],
            quotas_read_at: Some(1_000),
            ..AccountState::default()
        };
        let (limit, scope) = state.limit_on("gemini-3.8-flash", 2_000).unwrap();
        assert_eq!((limit.until, scope.as_deref()), (Some(5_000), Some("gemini-3.8-flash")));
        assert_eq!(limit.message, "Gemini Models, weekly is used up");
        assert_eq!(state.limit_on("claude-opus-5-5", 2_000), None, "its group has room");
        let mut spent = state.clone();
        spent.quotas[1].used = 1.0;
        let (limit, _) = spent
            .limit_on("claude-opus-5-5", 2_000)
            .expect("agy's \"Claude Opus 5.5\" is claude-opus-5-5");
        assert_eq!(limit.message, "Claude and GPT models, 5h is used up");
        assert_eq!(state.limit_on("gemini-4-neon", 5_000), None, "free once it resets");
    }

    #[test]
    fn reads_claudes_reset_times() {
        let now = local_epoch(2026, 10, 27, 22, 0).unwrap();
        assert_eq!(
            local_time(claude_reset("Dec 2, 2am", now).unwrap())[..16],
            *"2026-12-02 02:00"
        );
        assert_eq!(
            local_time(claude_reset("3:30pm", now).unwrap())[..16],
            *"2026-11-28 15:30"
        );
        assert_eq!(
            local_time(claude_reset("Jan 4, 9am", now).unwrap())[..16],
            *"2027-01-04 09:00"
        );
        assert_eq!(utc_epoch("2026-12-03T12:42:51Z"), Some(1_796_301_771));
    }

    #[test]
    fn a_claude_reset_in_another_zone_isnt_read_as_this_ones() {
        let now = local_epoch_for_tests(2026, 10, 27, 22, 0);
        if let Some(zone) = local_zone() {
            assert!(claude_reset(&format!("1am ({zone})"), now).is_some());
            assert_eq!(claude_reset("1am (Mars/Olympus_Mons)", now), None);
        }
    }
}
