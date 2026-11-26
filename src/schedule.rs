//! Calendar schedule integration reading the external `calendar-bar` JSON caches:
//! - `~/.local/state/calendar-bar/events.json`
//! - `~/.config/calendar-bar/genres.json`
//!
//! Provides the top bar next-event island and the calendar panel agenda + filter tabs.

use crate::panels::{Panel, is_open, toggle};
use crate::placement::BarPlacement;
use crate::theme::{TextStyle, colors, is_dark};
use crate::ui::*;
use anti_webui::prelude::*;
use anti_webui::shell::prelude::*;
use serde_json::Value;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FRESHNESS_SECS: i64 = 15 * 60;
const AGENDA_WIDTH: f32 = 336.0;
const AGENDA_MAX_H: f32 = 200.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalEvent {
    pub title: String,
    pub start_raw: String,
    pub end_raw: Option<String>,
    pub all_day: bool,
    pub calendar_id: String,
    pub calendar_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventsCache {
    pub status: String,
    pub updated_at: Option<String>,
    pub events: Vec<CalEvent>,
}

impl Default for EventsCache {
    fn default() -> Self {
        Self {
            status: "auth_required".into(),
            updated_at: None,
            events: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarMeta {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GenreSettings {
    pub enabled: bool,
    pub calendar_genres: HashMap<String, String>,
    pub calendars: Vec<CalendarMeta>,
    pub hidden_calendar_ids: HashSet<String>,
}

impl GenreSettings {
    pub fn is_visible(&self, calendar_id: &str) -> bool {
        !self.hidden_calendar_ids.contains(calendar_id)
    }

    pub fn genre(&self, calendar_id: &str) -> &str {
        self.calendar_genres
            .get(calendar_id)
            .map(String::as_str)
            .unwrap_or("personal")
    }

    pub fn accent(&self, calendar_id: &str, dark: bool) -> Color {
        let hex = match (dark, self.genre(calendar_id)) {
            (false, "school") => 0xa95718,
            (false, "work") => 0x315cb3,
            (false, "friend") => 0x22745e,
            (false, "hobby") => 0x8650a6,
            (false, _) => 0x626775,
            (true, "school") => 0xf0b06e,
            (true, "work") => 0xa3baff,
            (true, "friend") => 0x79cfb2,
            (true, "hobby") => 0xd0a2ed,
            (true, _) => 0xb7bccb,
        };
        rgb(hex)
    }

    pub fn tint(&self, calendar_id: &str, dark: bool) -> Color {
        self.accent(calendar_id, dark)
            .with_alpha(if dark { 0.14 } else { 0.08 })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ScheduleState {
    events: EventsCache,
    genres: GenreSettings,
}

thread_local! {
    static STATE: Cell<Option<Signal<ScheduleState>>> = const { Cell::new(None) };
}

fn events_path() -> PathBuf {
    home_dir().join(".local/state/calendar-bar/events.json")
}

fn genres_path() -> PathBuf {
    home_dir().join(".config/calendar-bar/genres.json")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn parse_events_json(text: &str) -> EventsCache {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return EventsCache {
            status: "parse_error".into(),
            updated_at: None,
            events: Vec::new(),
        };
    };
    let status = v
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("error")
        .to_owned();
    let updated_at = v
        .get("updated_at")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let events = v
        .get("events")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let title = e.get("title").and_then(Value::as_str)?.to_owned();
                    let start_raw = e.get("start").and_then(Value::as_str)?.to_owned();
                    let end_raw = e.get("end").and_then(Value::as_str).map(str::to_owned);
                    let all_day = e.get("all_day").and_then(Value::as_bool).unwrap_or(false);
                    let calendar_id = e
                        .get("calendar_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let calendar_name = e
                        .get("calendar_name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    Some(CalEvent {
                        title,
                        start_raw,
                        end_raw,
                        all_day,
                        calendar_id,
                        calendar_name,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    EventsCache {
        status,
        updated_at,
        events,
    }
}

fn parse_genres_json(text: &str) -> GenreSettings {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return GenreSettings::default();
    };
    let enabled = v.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let calendar_genres = v
        .get("calendar_genres")
        .and_then(Value::as_object)
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, val)| Some((k.clone(), val.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let calendars = v
        .get("calendars")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let id = c.get("id").and_then(Value::as_str)?.to_owned();
                    let name = c
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&id)
                        .to_owned();
                    Some(CalendarMeta { id, name })
                })
                .collect()
        })
        .unwrap_or_default();
    let hidden_calendar_ids = v
        .get("hidden_calendar_ids")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    GenreSettings {
        enabled,
        calendar_genres,
        calendars,
        hidden_calendar_ids,
    }
}

fn read_state_from_disk() -> ScheduleState {
    let events = fs::read_to_string(events_path())
        .map(|t| parse_events_json(&t))
        .unwrap_or_default();
    let genres = fs::read_to_string(genres_path())
        .map(|t| parse_genres_json(&t))
        .unwrap_or_default();
    ScheduleState { events, genres }
}

fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn state_signal() -> Signal<ScheduleState> {
    STATE.with(|cell| {
        cell.get().unwrap_or_else(|| {
            let (sig, _) = create_root(|| signal(read_state_from_disk()));
            cell.set(Some(sig));
            sig
        })
    })
}

/// Starts watching `events.json` and `genres.json` in a low-overhead background thread.
pub fn install() {
    let sig = state_signal();
    let tx = ui_channel(move |next: ScheduleState| {
        if sig.is_alive() {
            sig.set(next);
        }
    });
    let _ = std::thread::Builder::new()
        .name("calendar-bar-watch".into())
        .spawn(move || {
            let ep = events_path();
            let gp = genres_path();
            let mut last_e = file_stamp(&ep);
            let mut last_g = file_stamp(&gp);
            loop {
                std::thread::sleep(Duration::from_secs(3));
                let cur_e = file_stamp(&ep);
                let cur_g = file_stamp(&gp);
                if cur_e != last_e || cur_g != last_g {
                    last_e = cur_e;
                    last_g = cur_g;
                    tx.send(read_state_from_disk());
                }
            }
        });
}

fn persist_hidden_calendars(hidden: &HashSet<String>) {
    let path = genres_path();
    let mut doc = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let mut list: Vec<String> = hidden.iter().cloned().collect();
    list.sort();
    if let Some(obj) = doc.as_object_mut() {
        obj.insert(
            "hidden_calendar_ids".into(),
            Value::Array(list.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(formatted) = serde_json::to_string_pretty(&doc) {
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, formatted).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

fn toggle_calendar(id: &str) {
    let sig = state_signal();
    let mut hidden_after = HashSet::new();
    sig.update(|s| {
        if !s.genres.hidden_calendar_ids.remove(id) {
            s.genres.hidden_calendar_ids.insert(id.to_owned());
        }
        hidden_after = s.genres.hidden_calendar_ids.clone();
    });
    persist_hidden_calendars(&hidden_after);
}

fn show_all_calendars() {
    let sig = state_signal();
    sig.update(|s| s.genres.hidden_calendar_ids.clear());
    persist_hidden_calendars(&HashSet::new());
}

// --- Zero-dependency date / RFC3339 math ---

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = u64::from(month);
    let d = u64::from(day);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Civil (year, month, day) from days since 1970-01-01.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y as i32, m as u32, d as u32)
}

fn civil_to_unix(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> i64 {
    days_from_civil(y, m, d) * 86_400
        + i64::from(hh) * 3600
        + i64::from(mm) * 60
        + i64::from(ss)
}

fn unix_to_local_parts(unix_utc: i64, tz_offset_secs: i64) -> (i32, u32, u32, u32, u32) {
    let local = unix_utc + tz_offset_secs;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, secs / 3600, (secs % 3600) / 60)
}

fn parse_u32(s: &str) -> Option<u32> {
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Parses either `YYYY-MM-DD` (as local midnight) or RFC3339 (`YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM]`).
fn parse_timestamp(s: &str, local_tz_offset: i64) -> Option<i64> {
    let s = s.trim();
    if s.len() < 10 {
        return None;
    }
    let y: i32 = s.get(0..4)?.parse().ok()?;
    if s.as_bytes().get(4) != Some(&b'-') || s.as_bytes().get(7) != Some(&b'-') {
        return None;
    }
    let m = parse_u32(s.get(5..7)?)?;
    let d = parse_u32(s.get(8..10)?)?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    if s.len() == 10 {
        return Some(civil_to_unix(y, m, d, 0, 0, 0) - local_tz_offset);
    }
    let sep = *s.as_bytes().get(10)?;
    if sep != b'T' && sep != b't' && sep != b' ' {
        return None;
    }
    let rest = s.get(11..)?;
    if rest.len() < 5 {
        return None;
    }
    let hh = parse_u32(rest.get(0..2)?)?;
    if rest.as_bytes().get(2) != Some(&b':') {
        return None;
    }
    let mm = parse_u32(rest.get(3..5)?)?;
    let (ss, after_time) = if rest.as_bytes().get(5) == Some(&b':') && rest.len() >= 8 {
        (parse_u32(rest.get(6..8)?)?, &rest[8..])
    } else {
        (0, &rest[5..])
    };
    let after_frac = if let Some(stripped) = after_time.strip_prefix('.') {
        let digits = stripped.bytes().take_while(u8::is_ascii_digit).count();
        &stripped[digits..]
    } else {
        after_time
    };
    let offset = if after_frac.is_empty() {
        local_tz_offset
    } else if after_frac == "Z" || after_frac == "z" {
        0
    } else if after_frac.len() >= 6
        && (after_frac.starts_with('+') || after_frac.starts_with('-'))
        && after_frac.as_bytes().get(3) == Some(&b':')
    {
        let sign = if after_frac.starts_with('-') { -1 } else { 1 };
        let oh = parse_u32(after_frac.get(1..3)?)? as i64;
        let om = parse_u32(after_frac.get(4..6)?)? as i64;
        sign * (oh * 3600 + om * 60)
    } else {
        return None;
    };
    Some(civil_to_unix(y, m, d, hh, mm, ss) - offset)
}

#[derive(Clone, Copy, Debug)]
struct NowContext {
    utc_secs: i64,
    tz_offset: i64,
    today: (i32, u32, u32),
}

fn current_now() -> NowContext {
    let t = local_time(Every::Minute);
    let utc_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let local_as_utc = civil_to_unix(
        t.year,
        t.month as u32,
        t.day as u32,
        t.hour as u32,
        t.minute as u32,
        0,
    );
    let raw_diff = local_as_utc - (utc_secs - utc_secs.rem_euclid(60));
    let tz_offset = ((raw_diff + if raw_diff >= 0 { 450 } else { -450 }) / 900) * 900;
    NowContext {
        utc_secs,
        tz_offset,
        today: (t.year, t.month as u32, t.day as u32),
    }
}

fn is_fresh(cache: &EventsCache, now: NowContext) -> bool {
    cache
        .updated_at
        .as_deref()
        .and_then(|s| parse_timestamp(s, now.tz_offset))
        .is_some_and(|ts| now.utc_secs - ts <= FRESHNESS_SECS)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NextEventDisplay {
    text: String,
    calendar_id: Option<String>,
}

fn compute_next_event(state: &ScheduleState, now: NowContext) -> NextEventDisplay {
    if state.events.status == "parse_error" {
        return NextEventDisplay {
            text: "予定を読み込めません".into(),
            calendar_id: None,
        };
    }
    if state.events.status == "auth_required" {
        return NextEventDisplay {
            text: "カレンダー連携待ち".into(),
            calendar_id: None,
        };
    }
    if !is_fresh(&state.events, now) {
        return NextEventDisplay {
            text: "予定を更新できません".into(),
            calendar_id: None,
        };
    }

    let mut upcoming: Vec<(&CalEvent, i64)> = Vec::new();
    for ev in &state.events.events {
        if !state.genres.is_visible(&ev.calendar_id) {
            continue;
        }
        let Some(start) = parse_timestamp(&ev.start_raw, now.tz_offset) else {
            continue;
        };
        let end = ev
            .end_raw
            .as_deref()
            .and_then(|s| parse_timestamp(s, now.tz_offset));
        let (sy, sm, sd, _, _) = unix_to_local_parts(start, now.tz_offset);
        let matches = start >= now.utc_secs
            || (ev.all_day
                && start <= now.utc_secs
                && match end {
                    Some(e) => e > now.utc_secs,
                    None => (sy, sm, sd) == now.today,
                });
        if matches {
            upcoming.push((ev, start));
        }
    }

    if upcoming.is_empty() {
        return NextEventDisplay {
            text: if state.events.status == "error" {
                "予定を更新できません".into()
            } else {
                "予定なし".into()
            },
            calendar_id: None,
        };
    }

    let (ev, start) = upcoming
        .iter()
        .find(|(e, _)| !e.all_day)
        .copied()
        .unwrap_or(upcoming[0]);
    let (sy, sm, sd, hh, mm) = unix_to_local_parts(start, now.tz_offset);
    let date_prefix = if (sy, sm, sd) == now.today {
        String::new()
    } else {
        format!("{sm:02}/{sd:02} ")
    };
    let time_part = if ev.all_day {
        "終日".to_owned()
    } else {
        format!("{hh:02}:{mm:02}")
    };
    NextEventDisplay {
        text: format!("{date_prefix}{time_part} {}", ev.title),
        calendar_id: Some(ev.calendar_id.clone()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgendaItem {
    key: String,
    time_label: String,
    title: String,
    calendar_id: String,
    calendar_name: String,
    upcoming: bool,
    past: bool,
}

fn compute_day_events(state: &ScheduleState, now: NowContext, day_offset: i64) -> Vec<AgendaItem> {
    let base_days = days_from_civil(now.today.0, now.today.1, now.today.2) + day_offset;
    let day_start = base_days * 86_400 - now.tz_offset;
    let day_end = day_start + 86_400;

    let mut matched: Vec<(&CalEvent, i64, Option<i64>)> = Vec::new();
    for ev in &state.events.events {
        if !state.genres.is_visible(&ev.calendar_id) {
            continue;
        }
        let Some(from) = parse_timestamp(&ev.start_raw, now.tz_offset) else {
            continue;
        };
        let to = ev
            .end_raw
            .as_deref()
            .and_then(|s| parse_timestamp(s, now.tz_offset));
        let eff_to = to.unwrap_or(from);
        if from < day_end && (from >= day_start || eff_to > day_start) {
            matched.push((ev, from, to));
        }
    }

    let next_index = matched
        .iter()
        .position(|(ev, from, _)| !ev.all_day && *from >= now.utc_secs);

    matched
        .into_iter()
        .enumerate()
        .map(|(i, (ev, from, to))| {
            let (_, _, _, hh, mm) = unix_to_local_parts(from, now.tz_offset);
            let time_label = if ev.all_day {
                "終日".into()
            } else {
                format!("{hh:02}:{mm:02}")
            };
            let past = to.is_some_and(|e| e <= now.utc_secs);
            AgendaItem {
                key: format!("{i}:{}:{}", ev.start_raw, ev.title),
                time_label,
                title: ev.title.clone(),
                calendar_id: ev.calendar_id.clone(),
                calendar_name: ev.calendar_name.clone(),
                upcoming: next_index == Some(i),
                past,
            }
        })
        .collect()
}

/// Top-bar floating pill showing the next upcoming event; clicking toggles the calendar panel.
pub fn next_event_island(output: Output, at: BarPlacement) -> Show {
    let vertical = at.is_vertical();
    show(
        move || !vertical,
        move || {
            let sig = state_signal();
            let open = memo({
                let output = output.clone();
                move || is_open(&output, Panel::Calendar)
            });
            let display = memo(move || {
                let now = current_now();
                sig.with(|s| compute_next_event(s, now))
            });
            let dot_color = memo(move || {
                let dark = is_dark();
                let cal_id = display.with(|d| d.calendar_id.clone());
                sig.with(|s| {
                    if s.genres.enabled {
                        cal_id.map(|id| s.genres.accent(&id, dark))
                    } else {
                        None
                    }
                })
            });
            let text_style = TextStyle {
                size: 13.0,
                line_height: 18.0,
                letter_spacing: 0.0,
                weight: 500,
            };
            let content = island()
                .px(12.0_f32)
                .gap(7.0_f32)
                .max_w(260.0)
                .overflow_hidden()
                .on_click({
                    let output = output.clone();
                    move |_| toggle(&output, Panel::Calendar)
                })
                .dyn_style(move |s| {
                    let c = colors();
                    if open.get() {
                        s.bg(c.primary_container).text_color(c.on_primary_container)
                    } else {
                        s
                    }
                })
                .child(show(
                    move || dot_color.get().is_some(),
                    move || {
                        div()
                            .size(6.0)
                            .rounded(3.0_f32)
                            .flex_none()
                            .dyn_style(move |s| {
                                s.bg(dot_color.get().unwrap_or_else(|| colors().primary))
                            })
                    },
                ))
                .child(
                    txt(text_style, move || display.with(|d| d.text.clone()))
                        .max_w(232.0)
                        .overflow_hidden(),
                );
            pressable(content, |c| c.on_surface)
        },
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AgendaTab {
    Today,
    Tomorrow,
    Calendars,
}

/// Schedule section inside the calendar panel: Today / Tomorrow agenda + calendar visibility filter.
pub fn agenda_section() -> Div {
    let sig = state_signal();
    let tab = signal(AgendaTab::Today);

    let status_message = memo(move || {
        let now = current_now();
        sig.with(|s| {
            if s.events.status == "auth_required" {
                Some("カレンダー連携待ち".to_owned())
            } else if !is_fresh(&s.events, now) {
                Some("予定を更新できません".to_owned())
            } else {
                None
            }
        })
    });

    let tab_btn = move |target: AgendaTab, label: &'static str| {
        let active = memo(move || tab.get() == target);
        pressable(
            div()
                .flex_1()
                .h(32.0)
                .rounded(12.0_f32)
                .flex()
                .items_center()
                .justify_center()
                .on_click(move |_| {
                    if target == AgendaTab::Calendars && untrack(|| tab.get()) == AgendaTab::Calendars {
                        tab.set(AgendaTab::Today);
                    } else {
                        tab.set(target);
                    }
                })
                .dyn_style(move |s| {
                    let c = colors();
                    if active.get() {
                        s.bg(c.primary_container).text_color(c.on_primary_container)
                    } else {
                        s.bg(c.surface_container_high)
                            .text_color(c.on_surface_variant)
                    }
                })
                .child(static_txt(TextStyle::LABEL_LARGE, label)),
            |c| c.on_surface,
        )
    };

    div()
        .w(AGENDA_WIDTH)
        .flex()
        .flex_col()
        .gap(8.0_f32)
        .child(
            div()
                .h(1.0)
                .dyn_style(|s| s.bg(colors().outline_variant.with_alpha(0.5))),
        )
        .child(dyn_view(move || {
            if let Some(msg) = status_message.get() {
                return div()
                    .px(12.0_f32)
                    .py(10.0_f32)
                    .rounded(14.0_f32)
                    .dyn_style(|s| {
                        let c = colors();
                        s.bg(c.surface_container_high)
                            .text_color(c.on_surface_variant)
                    })
                    .child(static_txt(TextStyle::BODY_MEDIUM, msg))
                    .into_view();
            }
            div()
                .flex()
                .flex_col()
                .gap(8.0_f32)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(6.0_f32)
                        .child(tab_btn(AgendaTab::Today, "今日"))
                        .child(tab_btn(AgendaTab::Tomorrow, "明日"))
                        .child(tab_btn(AgendaTab::Calendars, "カレンダー")),
                )
                .child(dyn_view(move || match tab.get() {
                    AgendaTab::Today => day_events_list(sig, 0).into_view(),
                    AgendaTab::Tomorrow => day_events_list(sig, 1).into_view(),
                    AgendaTab::Calendars => calendar_filters_list(sig).into_view(),
                }))
                .into_view()
        }))
}

fn day_events_list(sig: Signal<ScheduleState>, day_offset: i64) -> Div {
    let items = memo(move || {
        let now = current_now();
        sig.with(|s| compute_day_events(s, now, day_offset))
    });
    div()
        .flex()
        .flex_col()
        .gap(6.0_f32)
        .max_h(AGENDA_MAX_H)
        .overflow_y_scroll()
        .child(dyn_view(move || {
            let list = items.get();
            if list.is_empty() {
                return div()
                    .h(38.0)
                    .rounded(12.0_f32)
                    .flex()
                    .items_center()
                    .justify_center()
                    .dyn_style(|s| {
                        let c = colors();
                        s.bg(c.surface_container_high)
                            .text_color(c.on_surface_variant)
                    })
                    .child(static_txt(TextStyle::BODY_MEDIUM, "予定なし"))
                    .into_view();
            }
            div()
                .flex()
                .flex_col()
                .gap(6.0_f32)
                .child(for_each(
                    move || items.get(),
                    |ev| ev.key.clone(),
                    move |ev| event_card(sig, ev),
                ))
                .into_view()
        }))
}

fn event_card(sig: Signal<ScheduleState>, ev: AgendaItem) -> Div {
    let cal_id = ev.calendar_id.clone();
    let cal_id_bar = cal_id.clone();
    let cal_id_time = cal_id.clone();
    let cal_id_label = cal_id.clone();
    let upcoming = ev.upcoming;
    let past = ev.past;
    let cal_name = ev.calendar_name.clone();

    let mut card = div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(8.0_f32)
        .px(10.0_f32)
        .py(8.0_f32)
        .rounded(12.0_f32);
    if upcoming {
        card = card.border(1.0_f32);
    }
    card.dyn_style(move |s| {
        let c = colors();
        let dark = is_dark();
        let (enabled, accent, tint) = sig.with(|st| {
            (
                st.genres.enabled,
                st.genres.accent(&cal_id, dark),
                st.genres.tint(&cal_id, dark),
            )
        });
        let s = if upcoming && enabled {
            s.bg(tint)
        } else {
            s.bg(c.surface_container_high)
        };
        let s = if upcoming {
            s.border_color(if enabled { accent } else { c.primary })
        } else {
            s
        };
        if past { s.opacity(0.68_f32) } else { s }
    })
    .child(show(
        move || sig.with(|st| st.genres.enabled),
        move || {
            let cal_id = cal_id_bar.clone();
            div()
                .w(3.0)
                .h(20.0)
                .rounded(2.0_f32)
                .flex_none()
                .dyn_style(move |s| {
                    let dark = is_dark();
                    s.bg(sig.with(|st| st.genres.accent(&cal_id, dark)))
                })
        },
    ))
    .child(
        static_txt(TextStyle::TITLE_SMALL, ev.time_label)
            .w(46.0)
            .flex_none()
            .dyn_style(move |s| {
                let c = colors();
                let dark = is_dark();
                let col = sig.with(|st| {
                    if st.genres.enabled {
                        st.genres.accent(&cal_id_time, dark)
                    } else {
                        c.primary
                    }
                });
                s.text_color(col)
            }),
    )
    .child(
        TextStyle::BODY_MEDIUM
            .apply(div().flex_1().min_w(0.0).overflow_hidden().child(ev.title))
            .dyn_style(|s| s.text_color(colors().on_surface)),
    )
    .child(show(
        move || !cal_name.is_empty() && sig.with(|st| st.genres.enabled),
        move || {
            let cal_id = cal_id_label.clone();
            let name = ev.calendar_name.clone();
            static_txt(TextStyle::LABEL_SMALL, name)
                .max_w(96.0)
                .overflow_hidden()
                .flex_none()
                .dyn_style(move |s| {
                    let dark = is_dark();
                    s.text_color(sig.with(|st| st.genres.accent(&cal_id, dark)))
                })
        },
    ))
}

fn calendar_filters_list(sig: Signal<ScheduleState>) -> Div {
    let cals = memo(move || sig.with(|s| s.genres.calendars.clone()));
    div()
        .flex()
        .flex_col()
        .gap(5.0_f32)
        .max_h(AGENDA_MAX_H)
        .overflow_y_scroll()
        .child(pressable(
            div()
                .h(28.0)
                .rounded(10.0_f32)
                .flex()
                .items_center()
                .justify_center()
                .on_click(|_| show_all_calendars())
                .dyn_style(|s| {
                    let c = colors();
                    s.bg(c.surface_container_high).text_color(c.primary)
                })
                .child(static_txt(TextStyle::LABEL_MEDIUM, "すべて表示")),
            |c| c.on_surface,
        ))
        .child(for_each(
            move || cals.get(),
            |c| c.id.clone(),
            move |meta| {
                let id_check = meta.id.clone();
                let id_color = meta.id.clone();
                let id_click = meta.id.clone();
                let checked = memo(move || sig.with(|s| s.genres.is_visible(&id_check)));
                pressable(
                    div()
                        .h(34.0)
                        .px(10.0_f32)
                        .rounded(10.0_f32)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(8.0_f32)
                        .on_click(move |_| toggle_calendar(&id_click))
                        .dyn_style(|s| s.bg(colors().surface_container_high))
                        .child(
                            div()
                                .size(18.0)
                                .rounded(5.0_f32)
                                .border(1.0_f32)
                                .flex()
                                .items_center()
                                .justify_center()
                                .flex_none()
                                .dyn_style(move |s| {
                                    let c = colors();
                                    let dark = is_dark();
                                    let accent = sig.with(|st| st.genres.accent(&id_color, dark));
                                    if checked.get() {
                                        s.bg(accent)
                                            .border_color(accent)
                                            .text_color(if dark {
                                                rgb(0x202127)
                                            } else {
                                                rgb(0xffffff)
                                            })
                                    } else {
                                        s.bg(Color::TRANSPARENT)
                                            .border_color(c.on_surface_variant)
                                    }
                                })
                                .child(show(
                                    move || checked.get(),
                                    || static_txt(TextStyle::LABEL_SMALL, "✓"),
                                )),
                        )
                        .child(
                            static_txt(TextStyle::BODY_SMALL, meta.name)
                                .flex_1()
                                .min_w(0.0)
                                .overflow_hidden()
                                .dyn_style(|s| s.text_color(colors().on_surface)),
                        ),
                    |c| c.on_surface,
                )
            },
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_and_date_only() {
        let jst = 9 * 3600;
        let ts = parse_timestamp("2026-11-26T15:30:00+09:00", jst).unwrap();
        assert_eq!(unix_to_local_parts(ts, jst), (2026, 11, 26, 15, 30));

        let ts_frac = parse_timestamp("2026-11-26T14:51:08.695367+09:00", jst).unwrap();
        assert_eq!(unix_to_local_parts(ts_frac, jst), (2026, 11, 26, 14, 51));

        let ts_day = parse_timestamp("2026-11-26", jst).unwrap();
        assert_eq!(unix_to_local_parts(ts_day, jst), (2026, 11, 26, 0, 0));
    }

    #[test]
    fn next_event_prefers_timed_over_all_day_and_respects_hidden() {
        let jst = 9 * 3600;
        let now = NowContext {
            utc_secs: parse_timestamp("2026-11-26T14:00:00+09:00", jst).unwrap(),
            tz_offset: jst,
            today: (2026, 11, 26),
        };
        let events = parse_events_json(
            r#"{
              "status": "ok",
              "updated_at": "2026-11-26T13:55:00+09:00",
              "events": [
                {"title": "All Day", "start": "2026-11-26", "all_day": true, "calendar_id": "a", "calendar_name": "A"},
                {"title": "Hidden", "start": "2026-11-26T14:30:00+09:00", "all_day": false, "calendar_id": "h", "calendar_name": "H"},
                {"title": "Lecture", "start": "2026-11-26T15:00:00+09:00", "end": "2026-11-26T16:30:00+09:00", "all_day": false, "calendar_id": "s", "calendar_name": "School"}
              ]
            }"#,
        );
        let mut genres = GenreSettings::default();
        genres.hidden_calendar_ids.insert("h".into());
        let state = ScheduleState { events, genres };

        let next = compute_next_event(&state, now);
        assert_eq!(next.text, "15:00 Lecture");
        assert_eq!(next.calendar_id.as_deref(), Some("s"));

        let today_items = compute_day_events(&state, now, 0);
        assert_eq!(today_items.len(), 2);
        assert!(!today_items[0].upcoming);
        assert!(today_items[1].upcoming);
    }

    #[test]
    fn stale_and_auth_required_messages() {
        let jst = 9 * 3600;
        let now = NowContext {
            utc_secs: parse_timestamp("2026-11-26T15:00:00+09:00", jst).unwrap(),
            tz_offset: jst,
            today: (2026, 11, 26),
        };
        let auth_state = ScheduleState::default();
        assert_eq!(compute_next_event(&auth_state, now).text, "カレンダー連携待ち");

        let stale = ScheduleState {
            events: parse_events_json(
                r#"{"status":"ok","updated_at":"2026-11-26T14:00:00+09:00","events":[]}"#,
            ),
            genres: GenreSettings::default(),
        };
        assert_eq!(compute_next_event(&stale, now).text, "予定を更新できません");
    }
}

