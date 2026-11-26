//! Resource monitor popup opened from the active window island on the bar.
//!
//! Shows:
//! - Active application CPU%, memory (PSS/RSS), and process tree count (sampled only while open)
//! - System-wide CPU, memory, and temperature using Material 3 Expressive wavy rings.

use crate::appinfo::app_id_icon;
use crate::placement::{AnchorPoint, BarPlacement};
use crate::theme::{TextStyle, colors, elevation};
use crate::ui::*;
use anti_webui::prelude::*;
use anti_webui::shell::prelude::*;
use anti_webui_md3e::components::circular_wavy_progress;
use anti_webui_md3e::icons::{self, Symbol};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const WIDTH: f32 = 340.0;
const EDGE: f32 = 14.0;
const CLK_TCK: f64 = 100.0;

#[derive(Clone, Debug, Default, PartialEq)]
struct AppSample {
    pid: u32,
    app_id: String,
    display_name: String,
    processes: usize,
    cpu_percent: Option<f64>,
    memory_kib: u64,
    valid: bool,
}

#[derive(Clone, Copy, Debug)]
struct ProcEntry {
    ppid: u32,
    ticks: u64,
    start_time: u64,
}

fn friendly_name(app_id: &str, fallback_title: &str) -> String {
    match app_id.to_ascii_lowercase().as_str() {
        "chatgpt" => "ChatGPT".into(),
        "vivaldi-stable" => "Vivaldi".into(),
        "firefox" => "Firefox".into(),
        "chromium" => "Chromium".into(),
        "dev.linegtk.linegtk" => "LINE GTK".into(),
        _ if !app_id.is_empty() => app_id.to_owned(),
        _ if !fallback_title.is_empty() => fallback_title.chars().take(28).collect(),
        _ => "アクティブアプリ".into(),
    }
}

fn query_hyprland_active() -> Option<(u32, String, String)> {
    let out = Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let pid = v.get("pid")?.as_u64()? as u32;
    if pid == 0 {
        return None;
    }
    let class = v
        .get("class")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some((pid, class, title))
}

fn parse_proc_stat(text: &str) -> Option<ProcEntry> {
    let rparen = text.rfind(')')?;
    let fields: Vec<&str> = text.get(rparen + 2..)?.split_whitespace().collect();
    if fields.len() < 20 {
        return None;
    }
    let ppid: u32 = fields[1].parse().ok()?;
    let utime: u64 = fields[11].parse().ok()?;
    let stime: u64 = fields[12].parse().ok()?;
    let start_time: u64 = fields[19].parse().ok()?;
    Some(ProcEntry {
        ppid,
        ticks: utime.saturating_add(stime),
        start_time,
    })
}

fn proc_snapshot() -> HashMap<u32, ProcEntry> {
    let mut map = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return map;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if let Ok(stat) = fs::read_to_string(entry.path().join("stat"))
            && let Some(parsed) = parse_proc_stat(&stat)
        {
            map.insert(pid, parsed);
        }
    }
    map
}

fn collect_family(root: u32, procs: &HashMap<u32, ProcEntry>) -> Vec<u32> {
    if root == 0 || !procs.contains_key(&root) {
        return Vec::new();
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, entry) in procs {
        children.entry(entry.ppid).or_default().push(pid);
    }
    let mut visited = HashSet::new();
    let mut stack = vec![root];
    let mut out = Vec::new();
    while let Some(pid) = stack.pop() {
        if visited.insert(pid) {
            out.push(pid);
            if let Some(kids) = children.get(&pid) {
                stack.extend_from_slice(kids);
            }
        }
    }
    out
}

fn parse_kib_field(text: &str, prefix: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn memory_kib_for_pid(pid: u32) -> u64 {
    let rollup = format!("/proc/{pid}/smaps_rollup");
    if let Ok(text) = fs::read_to_string(&rollup)
        && let Some(pss) = parse_kib_field(&text, "Pss:")
    {
        return pss;
    }
    let status = format!("/proc/{pid}/status");
    if let Ok(text) = fs::read_to_string(&status)
        && let Some(rss) = parse_kib_field(&text, "VmRSS:")
    {
        return rss;
    }
    0
}

fn format_memory_kib(kib: u64) -> String {
    let mib = kib as f64 / 1024.0;
    if mib >= 1024.0 {
        format!("{:.1} GiB", mib / 1024.0)
    } else {
        format!("{} MiB", mib.round() as u64)
    }
}

fn cpu_cores() -> f64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as f64)
        .unwrap_or(1.0)
}

pub fn panel(output: Output, anchor: Option<AnchorPoint>, at: BarPlacement) -> Div {
    let active_fallback = untrack(|| toplevels().active());
    let fallback_app_id = active_fallback
        .as_ref()
        .map(|t| t.app_id.clone())
        .unwrap_or_default();
    let fallback_title = active_fallback
        .as_ref()
        .map(|t| t.title.clone())
        .unwrap_or_default();

    let sample = signal(AppSample {
        pid: 0,
        app_id: fallback_app_id.clone(),
        display_name: friendly_name(&fallback_app_id, &fallback_title),
        processes: 0,
        cpu_percent: None,
        memory_kib: 0,
        valid: false,
    });

    let alive = Arc::new(AtomicBool::new(true));
    let alive_cleanup = alive.clone();
    on_cleanup(move || alive_cleanup.store(false, Ordering::Relaxed));

    let tx = ui_channel(move |next: AppSample| {
        if sample.is_alive() {
            sample.set(next);
        }
    });

    let fb_id = fallback_app_id;
    let fb_title = fallback_title;
    let _ = std::thread::Builder::new()
        .name("app-usage-sampler".into())
        .spawn(move || {
            let cores = cpu_cores();
            let mut prev_pid = 0u32;
            let mut prev_entries: HashMap<u32, ProcEntry> = HashMap::new();
            let mut prev_time: Option<Instant> = None;

            while alive.load(Ordering::Relaxed) {
                let (pid, app_id, title) = query_hyprland_active()
                    .unwrap_or_else(|| (0, fb_id.clone(), fb_title.clone()));
                let snap = if pid > 0 {
                    proc_snapshot()
                } else {
                    HashMap::new()
                };
                let family = collect_family(pid, &snap);
                let now = Instant::now();

                let mut current_entries = HashMap::with_capacity(family.len());
                let mut mem_kib = 0u64;
                for &p in &family {
                    if let Some(&e) = snap.get(&p) {
                        current_entries.insert(p, e);
                    }
                    mem_kib = mem_kib.saturating_add(memory_kib_for_pid(p));
                }

                let cpu_percent = if pid > 0 && pid == prev_pid {
                    prev_time.map(|last| {
                        let elapsed = now.duration_since(last).as_secs_f64().max(0.05);
                        let mut delta_ticks = 0u64;
                        for (&p, cur) in &current_entries {
                            if let Some(old) = prev_entries.get(&p)
                                && old.start_time == cur.start_time
                            {
                                delta_ticks =
                                    delta_ticks.saturating_add(cur.ticks.saturating_sub(old.ticks));
                            }
                        }
                        ((delta_ticks as f64 / CLK_TCK) / elapsed / cores * 100.0).clamp(0.0, 100.0)
                    })
                } else {
                    None
                };

                let next = AppSample {
                    pid,
                    display_name: friendly_name(&app_id, &title),
                    app_id,
                    processes: family.len(),
                    cpu_percent,
                    memory_kib: mem_kib,
                    valid: !family.is_empty(),
                };
                tx.send(next);

                prev_pid = pid;
                prev_entries = current_entries;
                prev_time = Some(now);

                for _ in 0..15 {
                    if !alive.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });

    let width = output.logical_size.width as f32;
    let height = output.logical_size.height as f32;
    let (x, y) = anchor
        .map(|a| (a.x, a.y))
        .unwrap_or((EDGE + WIDTH / 2.0, height / 2.0));
    let left = (x - WIDTH / 2.0).clamp(EDGE, (width - WIDTH - EDGE).max(EDGE));
    let top = signal(y);
    let container = div().w(WIDTH);
    let container = if at.is_vertical() {
        container
            .dyn_style(move |s| s.mt(top.get()))
            .on_resized(move |_, h| {
                top.set((y - h / 2.0).clamp(EDGE, (height - h - EDGE).max(EDGE)));
            })
    } else {
        container.ml(left)
    };

    let sys = system();
    let mem_total_kib = memo(move || sys.memory().total / 1024);

    container
        .p(16.0_f32)
        .rounded(28.0_f32)
        .shadows(elevation(3))
        .flex()
        .flex_col()
        .gap(12.0_f32)
        .dyn_style(|s| {
            let c = colors();
            s.bg(c.surface_container).text_color(c.on_surface)
        })
        .child(settle_in(0, header(sample)))
        .child(settle_in(1, app_metrics_card(sample, mem_total_kib)))
        .child(settle_in(2, system_gauges()))
}

fn header(sample: Signal<AppSample>) -> Div {
    let icon_path = memo(move || {
        sample.with(|s| {
            if s.app_id.is_empty() {
                None
            } else {
                app_id_icon(&s.app_id, 64)
            }
        })
    });
    div()
        .px(4.0_f32)
        .flex()
        .flex_row()
        .items_center()
        .gap(10.0_f32)
        .child(dyn_view(move || match icon_path.get() {
            Some(path) => img(path).size(24.0).flex_none().into_view(),
            None => div()
                .size(24.0)
                .rounded(12.0_f32)
                .flex()
                .items_center()
                .justify_center()
                .flex_none()
                .dyn_style(|s| s.bg(colors().secondary_container))
                .child(sym(icons::DESKTOP_WINDOWS, 14.0, |c| {
                    c.on_secondary_container
                }))
                .into_view(),
        }))
        .child(
            txt(TextStyle::TITLE_MEDIUM, move || {
                sample.with(|s| s.display_name.clone())
            })
            .flex_1()
            .min_w(0.0)
            .overflow_hidden(),
        )
        .child(show(
            move || sample.with(|s| s.valid && s.processes > 0),
            move || {
                div()
                    .px(8.0_f32)
                    .py(3.0_f32)
                    .rounded(10.0_f32)
                    .flex_none()
                    .dyn_style(|s| {
                        let c = colors();
                        s.bg(c.surface_container_high)
                            .text_color(c.on_surface_variant)
                    })
                    .child(txt(TextStyle::LABEL_SMALL, move || {
                        format!("{} プロセス", sample.with(|s| s.processes))
                    }))
            },
        ))
}

fn progress_track(fraction: impl Fn() -> f32 + 'static) -> Div {
    let frac = memo(move || fraction().clamp(0.0, 1.0));
    div()
        .h(4.0)
        .w_full()
        .rounded(2.0_f32)
        .overflow_hidden()
        .dyn_style(|s| s.bg(colors().surface_container_highest))
        .child(
            div()
                .h_full()
                .rounded(2.0_f32)
                .dyn_style(move |s| {
                    s.w(Length::Percent(frac.get() * 100.0))
                        .bg(colors().primary)
                }),
        )
}

fn app_metrics_card(sample: Signal<AppSample>, mem_total_kib: Memo<u64>) -> Div {
    let card = |label: &'static str,
                value: Box<dyn Fn() -> String>,
                fraction: Box<dyn Fn() -> f32>| {
        div()
            .flex_1()
            .p(12.0_f32)
            .rounded(18.0_f32)
            .flex()
            .flex_col()
            .gap(6.0_f32)
            .dyn_style(|s| s.bg(colors().surface_container_high))
            .child(
                static_txt(TextStyle::LABEL_SMALL, label)
                    .dyn_style(|s| s.text_color(colors().on_surface_variant)),
            )
            .child(
                txt(TextStyle::TITLE_LARGE, value)
                    .dyn_style(|s| s.text_color(colors().on_surface)),
            )
            .child(progress_track(fraction))
    };

    div()
        .flex()
        .flex_col()
        .gap(6.0_f32)
        .child(
            static_txt(TextStyle::LABEL_MEDIUM, "このアプリ")
                .px(4.0_f32)
                .dyn_style(|s| s.text_color(colors().primary)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(8.0_f32)
                .child(card(
                    "CPU",
                    Box::new(move || {
                        sample.with(|s| match (s.valid, s.cpu_percent) {
                            (true, Some(c)) => format!("{c:.1}%"),
                            (true, None) => "計測中".into(),
                            _ => "—".into(),
                        })
                    }),
                    Box::new(move || {
                        sample.with(|s| {
                            if s.valid {
                                (s.cpu_percent.unwrap_or(0.0) / 100.0) as f32
                            } else {
                                0.0
                            }
                        })
                    }),
                ))
                .child(card(
                    "メモリ",
                    Box::new(move || {
                        sample.with(|s| {
                            if s.valid {
                                format_memory_kib(s.memory_kib)
                            } else {
                                "—".into()
                            }
                        })
                    }),
                    Box::new(move || {
                        let total = mem_total_kib.get().max(1) as f64;
                        sample.with(|s| {
                            if s.valid {
                                (s.memory_kib as f64 / total) as f32
                            } else {
                                0.0
                            }
                        })
                    }),
                )),
        )
}

/// System-wide CPU, memory, and temperature wavy rings (moved from calendar).
fn system_gauges() -> Div {
    let sys = system();
    let sys_cpu = sys.clone();
    let sys_mem = sys.clone();
    let cpu = memo(move || sys_cpu.cpu());
    let memory = memo(move || sys_mem.memory());
    let heat = memo(move || sys.cpu_temperature());
    let gauge = |symbol: &'static Symbol,
                 label: &'static str,
                 value: Box<dyn Fn() -> f32>,
                 shown: Box<dyn Fn() -> String>| {
        let value: std::rc::Rc<dyn Fn() -> f32> = value.into();
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .gap(6.0_f32)
            .px(6.0_f32)
            .py(12.0_f32)
            .rounded(18.0_f32)
            .dyn_style(|s| s.bg(colors().surface_container_high))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(circular_wavy_progress(move || value()))
                    .child(
                        div()
                            .absolute()
                            .inset(0.0)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(sym(symbol, 18.0, |c| c.primary)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(txt(TextStyle::TITLE_SMALL, shown))
                    .child(
                        static_txt(TextStyle::LABEL_SMALL, label)
                            .dyn_style(|s| s.text_color(colors().on_surface_variant)),
                    ),
            )
    };
    let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
    div()
        .flex()
        .flex_col()
        .gap(6.0_f32)
        .child(
            static_txt(TextStyle::LABEL_MEDIUM, "PC全体")
                .px(4.0_f32)
                .dyn_style(|s| s.text_color(colors().on_surface_variant)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(8.0_f32)
                .child(gauge(
                    icons::SPEED,
                    "CPU",
                    Box::new(move || cpu.get() as f32),
                    Box::new(move || format!("{:.0}%", cpu.get() * 100.0)),
                ))
                .child(gauge(
                    icons::MEMORY,
                    "メモリ",
                    Box::new(move || {
                        let m = memory.get();
                        (m.used as f64 / m.total.max(1) as f64) as f32
                    }),
                    Box::new(move || format!("{:.1} GiB", gib(memory.get().used))),
                ))
                .child(gauge(
                    icons::THERMOSTAT,
                    "温度",
                    Box::new(move || {
                        ((heat.get().unwrap_or(0.0) - 30.0) / 70.0).clamp(0.0, 1.0) as f32
                    }),
                    Box::new(move || {
                        heat.get()
                            .map(|t| format!("{t:.0}°C"))
                            .unwrap_or_else(|| "—".into())
                    }),
                )),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_stat_with_spaces_in_comm() {
        let line = "1234 (Web Content (1)) S 100 100 100 0 -1 4194304 1 0 0 0 250 50 0 0 20 0 10 0 98765 0 0";
        let e = parse_proc_stat(line).unwrap();
        assert_eq!(e.ppid, 100);
        assert_eq!(e.ticks, 300);
        assert_eq!(e.start_time, 98765);
    }

    #[test]
    fn collects_process_family_tree() {
        let mut map = HashMap::new();
        map.insert(10, ProcEntry { ppid: 1, ticks: 0, start_time: 1 });
        map.insert(20, ProcEntry { ppid: 10, ticks: 0, start_time: 2 });
        map.insert(30, ProcEntry { ppid: 20, ticks: 0, start_time: 3 });
        map.insert(99, ProcEntry { ppid: 1, ticks: 0, start_time: 4 });
        let mut fam = collect_family(10, &map);
        fam.sort();
        assert_eq!(fam, vec![10, 20, 30]);
    }

    #[test]
    fn formats_kib_memory() {
        assert_eq!(format_memory_kib(512 * 1024), "512 MiB");
        assert_eq!(format_memory_kib(1536 * 1024), "1.5 GiB");
    }
}

