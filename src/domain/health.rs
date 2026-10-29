//! Health-script parsing and derived metrics.
//!
//! The remote script prints `key=value` lines. This module turns that text into
//! a [`RawSample`], then into a [`HealthReport`] with percentages and rates.
//! CPU % and network bytes/sec need a previous sample; without one those fields
//! stay empty. Status levels use the warn/crit thresholds from config.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Disk, memory, and load-per-CPU cutoffs used by [`classify`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Disk used percent.
    pub disk: f64,
    /// Memory used percent.
    pub mem: f64,
    /// Load average divided by CPU count.
    pub load_per_cpu: f64,
}

/// Raw counters from one poll. Unknown keys are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawSample {
    /// Wall time of the sample, Unix seconds.
    pub taken_at: f64,
    /// `hostname` output.
    pub hostname: Option<String>,
    /// `uname -r`.
    pub kernel: Option<String>,
    /// `/etc/os-release` `PRETTY_NAME`.
    pub os: Option<String>,
    /// Seconds from `/proc/uptime`.
    pub uptime_secs: Option<f64>,
    /// 1-minute load average.
    pub load1: Option<f64>,
    /// 5-minute load average.
    pub load5: Option<f64>,
    /// 15-minute load average.
    pub load15: Option<f64>,
    /// Logical CPU count.
    pub cpus: Option<u32>,
    /// Sum of user/nice/system/irq/softirq/steal since boot.
    pub cpu_busy: Option<u64>,
    /// Sum of all CPU fields since boot, including idle and iowait.
    pub cpu_total: Option<u64>,
    /// `MemTotal` kibibytes.
    pub mem_total_kb: Option<u64>,
    /// `MemAvailable` kibibytes.
    pub mem_available_kb: Option<u64>,
    /// `SwapTotal` kibibytes.
    pub swap_total_kb: Option<u64>,
    /// `SwapFree` kibibytes.
    pub swap_free_kb: Option<u64>,
    /// Mounted filesystems from `df -P`.
    pub disks: Vec<DiskSample>,
    /// Summed receive bytes excluding `lo`.
    pub net_rx_bytes: Option<u64>,
    /// Summed transmit bytes excluding `lo`.
    pub net_tx_bytes: Option<u64>,
    /// `systemctl --failed` line count.
    pub failed_units: Option<u32>,
    /// `/var/run/reboot-required` present.
    pub reboot_required: Option<bool>,
}

/// One `df -P` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskSample {
    /// Mount point, for example `/`.
    pub mount: String,
    /// Size in kibibytes.
    pub total_kb: u64,
    /// Used kibibytes.
    pub used_kb: u64,
    /// Available kibibytes.
    pub avail_kb: u64,
    /// Used percent from `df`.
    pub used_pct: f64,
}

/// Derived metrics ready for the UI and CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthReport {
    /// When the sample was taken.
    pub checked_at: f64,
    /// Whether ssh returned the script output.
    pub reachable: bool,
    /// Failure text when unreachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Overall level from thresholds.
    pub status: HealthStatus,
    /// True when the data is older than twice the refresh interval.
    #[serde(default)]
    pub stale: bool,
    /// Remote hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Kernel release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// Pretty OS name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// Compact uptime such as `41d`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime: Option<String>,
    /// Uptime in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<f64>,
    /// 1-minute load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load1: Option<f64>,
    /// Load divided by CPU count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_per_cpu: Option<f64>,
    /// CPU count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// Busy CPU percent between this sample and the previous one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<f64>,
    /// Memory used percent from MemTotal − MemAvailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_pct: Option<f64>,
    /// Swap used percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_pct: Option<f64>,
    /// Root (`/`) disk used percent, else the fullest mount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_pct: Option<f64>,
    /// All filesystems.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<DiskSample>,
    /// Receive bytes per second since the previous sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_rx_bps: Option<f64>,
    /// Transmit bytes per second since the previous sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_tx_bps: Option<f64>,
    /// Failed systemd units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_units: Option<u32>,
    /// Reboot-required flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reboot_required: Option<bool>,
}

/// Traffic-light level for a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// Under every warn threshold.
    Ok,
    /// At least one warn threshold, none critical.
    Warn,
    /// At least one crit threshold.
    Crit,
    /// ssh failed or produced no usable sample.
    Unreachable,
}

/// One sparkline / chart point kept in the ring buffer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HistoryPoint {
    /// Sample time, Unix seconds.
    pub at: f64,
    /// CPU percent when known.
    pub cpu_pct: Option<f64>,
    /// Memory percent when known.
    pub mem_pct: Option<f64>,
    /// 1-minute load when known.
    pub load1: Option<f64>,
    /// Receive bytes/sec when known.
    pub net_rx_bps: Option<f64>,
    /// Transmit bytes/sec when known.
    pub net_tx_bps: Option<f64>,
}

impl HealthStatus {
    /// Symbol used when colour is off: ✔ ⚠ ✖ —.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Ok => "✔",
            Self::Warn => "⚠",
            Self::Crit | Self::Unreachable => "✖",
        }
    }
}

/// Parse `key=value` text into a [`RawSample`]. Unknown keys are skipped.
pub fn parse_sample(text: &str, taken_at: f64) -> RawSample {
    let mut sample = RawSample {
        taken_at,
        hostname: None,
        kernel: None,
        os: None,
        uptime_secs: None,
        load1: None,
        load5: None,
        load15: None,
        cpus: None,
        cpu_busy: None,
        cpu_total: None,
        mem_total_kb: None,
        mem_available_kb: None,
        swap_total_kb: None,
        swap_free_kb: None,
        disks: Vec::new(),
        net_rx_bytes: None,
        net_tx_bytes: None,
        failed_units: None,
        reboot_required: None,
    };
    let mut cpu_user = 0u64;
    let mut cpu_nice = 0u64;
    let mut cpu_system = 0u64;
    let mut cpu_idle = 0u64;
    let mut cpu_iowait = 0u64;
    let mut cpu_irq = 0u64;
    let mut cpu_softirq = 0u64;
    let mut cpu_steal = 0u64;
    let mut saw_cpu = false;
    let mut pending_mount: Option<String> = None;
    let mut pending_total = 0u64;
    let mut pending_used = 0u64;
    let mut pending_avail = 0u64;
    let mut pending_pct = 0.0;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "hostname" => sample.hostname = Some(value.to_string()),
            "kernel" => sample.kernel = Some(value.to_string()),
            "os" => sample.os = Some(value.to_string()),
            "uptime_secs" => sample.uptime_secs = parse_f64(value),
            "load1" => sample.load1 = parse_f64(value),
            "load5" => sample.load5 = parse_f64(value),
            "load15" => sample.load15 = parse_f64(value),
            "cpus" => sample.cpus = parse_u32(value),
            "cpu_user" => {
                saw_cpu = true;
                cpu_user = parse_u64(value).unwrap_or(0);
            }
            "cpu_nice" => cpu_nice = parse_u64(value).unwrap_or(0),
            "cpu_system" => cpu_system = parse_u64(value).unwrap_or(0),
            "cpu_idle" => cpu_idle = parse_u64(value).unwrap_or(0),
            "cpu_iowait" => cpu_iowait = parse_u64(value).unwrap_or(0),
            "cpu_irq" => cpu_irq = parse_u64(value).unwrap_or(0),
            "cpu_softirq" => cpu_softirq = parse_u64(value).unwrap_or(0),
            "cpu_steal" => cpu_steal = parse_u64(value).unwrap_or(0),
            "mem_total_kb" => sample.mem_total_kb = parse_u64(value),
            "mem_available_kb" => sample.mem_available_kb = parse_u64(value),
            "swap_total_kb" => sample.swap_total_kb = parse_u64(value),
            "swap_free_kb" => sample.swap_free_kb = parse_u64(value),
            "disk_mount" => {
                flush_disk(
                    &mut sample.disks,
                    &mut pending_mount,
                    pending_total,
                    pending_used,
                    pending_avail,
                    pending_pct,
                );
                pending_mount = Some(value.to_string());
                pending_total = 0;
                pending_used = 0;
                pending_avail = 0;
                pending_pct = 0.0;
            }
            "disk_total_kb" => pending_total = parse_u64(value).unwrap_or(0),
            "disk_used_kb" => pending_used = parse_u64(value).unwrap_or(0),
            "disk_avail_kb" => pending_avail = parse_u64(value).unwrap_or(0),
            "disk_used_pct" => pending_pct = parse_f64(value).unwrap_or(0.0),
            "net_rx_bytes" => sample.net_rx_bytes = parse_u64(value),
            "net_tx_bytes" => sample.net_tx_bytes = parse_u64(value),
            "failed_units" => sample.failed_units = parse_u32(value),
            "reboot_required" => {
                sample.reboot_required = Some(value == "1" || value.eq_ignore_ascii_case("true"))
            }
            _ => {}
        }
    }
    flush_disk(
        &mut sample.disks,
        &mut pending_mount,
        pending_total,
        pending_used,
        pending_avail,
        pending_pct,
    );
    if saw_cpu {
        let busy = cpu_user
            .saturating_add(cpu_nice)
            .saturating_add(cpu_system)
            .saturating_add(cpu_irq)
            .saturating_add(cpu_softirq)
            .saturating_add(cpu_steal);
        let total = busy.saturating_add(cpu_idle).saturating_add(cpu_iowait);
        sample.cpu_busy = Some(busy);
        sample.cpu_total = Some(total);
    }
    sample
}

/// Build a report from a raw sample, optional previous sample, and thresholds.
pub fn build_report(
    sample: &RawSample,
    previous: Option<&RawSample>,
    warn: &Thresholds,
    crit: &Thresholds,
) -> HealthReport {
    let mem_pct = match (sample.mem_total_kb, sample.mem_available_kb) {
        (Some(total), Some(available)) if total > 0 => {
            let used = total.saturating_sub(available) as f64;
            Some((used / total as f64) * 100.0)
        }
        _ => None,
    };
    let swap_pct = match (sample.swap_total_kb, sample.swap_free_kb) {
        (Some(total), Some(free)) if total > 0 => {
            let used = total.saturating_sub(free) as f64;
            Some((used / total as f64) * 100.0)
        }
        _ => None,
    };
    let disk_pct = primary_disk_pct(&sample.disks);
    let load_per_cpu = match (sample.load1, sample.cpus) {
        (Some(load), Some(cpus)) if cpus > 0 => Some(load / f64::from(cpus)),
        (Some(load), None) => Some(load),
        _ => None,
    };
    let cpu_pct = previous.and_then(|prev| {
        cpu_percent(
            prev.cpu_busy?,
            prev.cpu_total?,
            sample.cpu_busy?,
            sample.cpu_total?,
        )
    });
    let (net_rx_bps, net_tx_bps) = previous
        .map(|prev| {
            let dt = sample.taken_at - prev.taken_at;
            (
                rate(prev.net_rx_bytes, sample.net_rx_bytes, dt),
                rate(prev.net_tx_bytes, sample.net_tx_bytes, dt),
            )
        })
        .unwrap_or((None, None));
    let status = classify(
        true,
        mem_pct,
        disk_pct,
        load_per_cpu,
        sample.failed_units,
        warn,
        crit,
    );
    HealthReport {
        checked_at: sample.taken_at,
        reachable: true,
        error: None,
        status,
        stale: false,
        hostname: sample.hostname.clone(),
        kernel: sample.kernel.clone(),
        os: sample.os.clone(),
        uptime: sample.uptime_secs.map(format_uptime),
        uptime_secs: sample.uptime_secs,
        load1: sample.load1,
        load_per_cpu,
        cpus: sample.cpus,
        cpu_pct,
        mem_pct,
        swap_pct,
        disk_pct,
        disks: sample.disks.clone(),
        net_rx_bps,
        net_tx_bps,
        failed_units: sample.failed_units,
        reboot_required: sample.reboot_required,
    }
}

/// Report for a host that did not return a sample.
pub fn unreachable_report(checked_at: f64, error: impl Into<String>) -> HealthReport {
    HealthReport {
        checked_at,
        reachable: false,
        error: Some(error.into()),
        status: HealthStatus::Unreachable,
        stale: false,
        hostname: None,
        kernel: None,
        os: None,
        uptime: None,
        uptime_secs: None,
        load1: None,
        load_per_cpu: None,
        cpus: None,
        cpu_pct: None,
        mem_pct: None,
        swap_pct: None,
        disk_pct: None,
        disks: Vec::new(),
        net_rx_bps: None,
        net_tx_bps: None,
        failed_units: None,
        reboot_required: None,
    }
}

/// Pick ok / warn / crit from the derived metrics.
pub fn classify(
    reachable: bool,
    mem_pct: Option<f64>,
    disk_pct: Option<f64>,
    load_per_cpu: Option<f64>,
    failed_units: Option<u32>,
    warn: &Thresholds,
    crit: &Thresholds,
) -> HealthStatus {
    if !reachable {
        return HealthStatus::Unreachable;
    }
    if crosses(mem_pct, crit.mem)
        || crosses(disk_pct, crit.disk)
        || crosses(load_per_cpu, crit.load_per_cpu)
    {
        return HealthStatus::Crit;
    }
    if crosses(mem_pct, warn.mem)
        || crosses(disk_pct, warn.disk)
        || crosses(load_per_cpu, warn.load_per_cpu)
        || failed_units.is_some_and(|count| count > 0)
    {
        return HealthStatus::Warn;
    }
    HealthStatus::Ok
}

/// Compact uptime: `45s`, `12m`, `3h`, `41d`.
pub fn format_uptime(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 48 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

/// Human network rate: `40K`, `1.2M`, `3.4G`.
pub fn format_rate(bps: f64) -> String {
    let abs = bps.abs();
    if abs < 1000.0 {
        return format!("{bps:.0}");
    }
    if abs < 1_000_000.0 {
        return format!("{:.1}K", bps / 1000.0);
    }
    if abs < 1_000_000_000.0 {
        return format!("{:.1}M", bps / 1_000_000.0);
    }
    format!("{:.1}G", bps / 1_000_000_000.0)
}

/// History point taken from a finished report.
pub fn history_point(report: &HealthReport) -> HistoryPoint {
    HistoryPoint {
        at: report.checked_at,
        cpu_pct: report.cpu_pct,
        mem_pct: report.mem_pct,
        load1: report.load1,
        net_rx_bps: report.net_rx_bps,
        net_tx_bps: report.net_tx_bps,
    }
}

/// Current Unix time as a float seconds value.
pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
}

fn cpu_percent(prev_busy: u64, prev_total: u64, busy: u64, total: u64) -> Option<f64> {
    if busy < prev_busy || total < prev_total {
        return None;
    }
    let delta_busy = busy - prev_busy;
    let delta_total = total - prev_total;
    if delta_total == 0 {
        return None;
    }
    Some((delta_busy as f64 / delta_total as f64) * 100.0)
}

fn rate(prev: Option<u64>, next: Option<u64>, dt: f64) -> Option<f64> {
    if dt <= 0.0 {
        return None;
    }
    let prev = prev?;
    let next = next?;
    if next < prev {
        return None;
    }
    Some((next - prev) as f64 / dt)
}

fn primary_disk_pct(disks: &[DiskSample]) -> Option<f64> {
    if disks.is_empty() {
        return None;
    }
    if let Some(root) = disks.iter().find(|disk| disk.mount == "/") {
        return Some(root.used_pct);
    }
    disks
        .iter()
        .map(|disk| disk.used_pct)
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal))
}

fn crosses(value: Option<f64>, threshold: f64) -> bool {
    value.is_some_and(|value| value >= threshold)
}

fn flush_disk(
    disks: &mut Vec<DiskSample>,
    mount: &mut Option<String>,
    total_kb: u64,
    used_kb: u64,
    avail_kb: u64,
    used_pct: f64,
) {
    if let Some(mount) = mount.take() {
        disks.push(DiskSample {
            mount,
            total_kb,
            used_kb,
            avail_kb,
            used_pct,
        });
    }
}

fn parse_f64(value: &str) -> Option<f64> {
    value.parse().ok()
}

fn parse_u64(value: &str) -> Option<u64> {
    value.parse().ok()
}

fn parse_u32(value: &str) -> Option<u32> {
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warn() -> Thresholds {
        Thresholds {
            disk: 80.0,
            mem: 85.0,
            load_per_cpu: 1.0,
        }
    }

    fn crit() -> Thresholds {
        Thresholds {
            disk: 90.0,
            mem: 95.0,
            load_per_cpu: 2.0,
        }
    }

    const UBUNTU: &str = r#"
hostname=web-1
kernel=6.8.0-40-generic
os=Ubuntu 24.04.1 LTS
uptime_secs=3544320.15
load1=0.42
load5=0.35
load15=0.30
cpus=4
cpu_user=1000
cpu_nice=0
cpu_system=200
cpu_idle=8800
cpu_iowait=0
cpu_irq=0
cpu_softirq=0
cpu_steal=0
mem_total_kb=16384000
mem_available_kb=10158080
swap_total_kb=2097152
swap_free_kb=2097152
disk_mount=/
disk_total_kb=102400000
disk_used_kb=62464000
disk_avail_kb=39936000
disk_used_pct=61
disk_mount=/var
disk_total_kb=51200000
disk_used_kb=10240000
disk_avail_kb=40960000
disk_used_pct=20
net_rx_bytes=1000000
net_tx_bytes=500000
failed_units=0
reboot_required=0
"#;

    const DEBIAN: &str = r#"
hostname=db-1
kernel=6.1.0-18-amd64
os=Debian GNU/Linux 12 (bookworm)
uptime_secs=8820000
load1=7.20
load5=6.80
load15=6.10
cpus=4
cpu_user=5000
cpu_nice=0
cpu_system=1000
cpu_idle=2000
cpu_iowait=500
cpu_irq=0
cpu_softirq=0
cpu_steal=0
mem_total_kb=8192000
mem_available_kb=409600
swap_total_kb=0
swap_free_kb=0
disk_mount=/
disk_total_kb=204800000
disk_used_kb=188416000
disk_avail_kb=16384000
disk_used_pct=92
net_rx_bytes=9000000
net_tx_bytes=1000000
failed_units=2
reboot_required=1
"#;

    const ROCKY: &str = r#"
hostname=cache-1
kernel=5.14.0-362.el9.x86_64
os=Rocky Linux 9.3 (Blue Onyx)
uptime_secs=120.5
load1=0.05
load5=0.03
load15=0.01
cpus=2
cpu_user=100
cpu_nice=0
cpu_system=50
cpu_idle=9850
cpu_iowait=0
cpu_irq=0
cpu_softirq=0
cpu_steal=0
mem_total_kb=4096000
mem_available_kb=3072000
swap_total_kb=1048576
swap_free_kb=1048576
disk_mount=/
disk_total_kb=40960000
disk_used_kb=8192000
disk_avail_kb=32768000
disk_used_pct=20
net_rx_bytes=100
net_tx_bytes=50
failed_units=0
reboot_required=0
"#;

    const ALPINE: &str = r#"
hostname=edge-1
kernel=6.6.28-0-lts
os=Alpine Linux v3.19
uptime_secs=3600
load1=0.10
load5=0.08
load15=0.05
cpus=1
cpu_user=200
cpu_nice=0
cpu_system=50
cpu_idle=7750
cpu_iowait=0
cpu_irq=0
cpu_softirq=0
cpu_steal=0
mem_total_kb=1024000
mem_available_kb=512000
swap_total_kb=0
swap_free_kb=0
disk_mount=/
disk_total_kb=8192000
disk_used_kb=3276800
disk_avail_kb=4915200
disk_used_pct=40
net_rx_bytes=250000
net_tx_bytes=125000
reboot_required=0
"#;

    #[test]
    fn parses_ubuntu_and_derives_ok_status() {
        let sample = parse_sample(UBUNTU, 1_000.0);
        assert_eq!(sample.hostname.as_deref(), Some("web-1"));
        assert_eq!(sample.cpus, Some(4));
        assert_eq!(sample.disks.len(), 2);
        assert_eq!(sample.disks[0].mount, "/");
        assert_eq!(sample.cpu_busy, Some(1200));
        assert_eq!(sample.cpu_total, Some(10_000));
        let report = build_report(&sample, None, &warn(), &crit());
        assert_eq!(report.status, HealthStatus::Ok);
        assert!((report.mem_pct.unwrap() - 38.0).abs() < 0.1);
        assert_eq!(report.disk_pct, Some(61.0));
        assert_eq!(report.cpu_pct, None);
        assert_eq!(report.uptime.as_deref(), Some("41d"));
    }

    #[test]
    fn debian_disk_and_failed_units_go_critical_then_warn() {
        let sample = parse_sample(DEBIAN, 2_000.0);
        let report = build_report(&sample, None, &warn(), &crit());
        assert_eq!(report.status, HealthStatus::Crit);
        assert_eq!(report.disk_pct, Some(92.0));
        assert_eq!(report.failed_units, Some(2));
        assert_eq!(report.reboot_required, Some(true));
        assert!((report.load_per_cpu.unwrap() - 1.8).abs() < 0.01);
    }

    #[test]
    fn rocky_and_alpine_parse() {
        let rocky = parse_sample(ROCKY, 100.0);
        assert!(rocky.os.as_deref().unwrap().contains("Rocky"));
        assert_eq!(rocky.cpus, Some(2));
        let alpine = parse_sample(ALPINE, 100.0);
        assert!(alpine.os.as_deref().unwrap().contains("Alpine"));
        assert_eq!(alpine.failed_units, None);
        let report = build_report(&alpine, None, &warn(), &crit());
        assert_eq!(report.status, HealthStatus::Ok);
        assert!((report.mem_pct.unwrap() - 50.0).abs() < 0.1);
    }

    #[test]
    fn cpu_and_net_deltas_ignore_counter_resets() {
        let first = parse_sample(UBUNTU, 1_000.0);
        let mut second_text = UBUNTU.to_string();
        second_text = second_text.replace("cpu_user=1000", "cpu_user=1500");
        second_text = second_text.replace("cpu_idle=8800", "cpu_idle=9100");
        second_text = second_text.replace("net_rx_bytes=1000000", "net_rx_bytes=1300000");
        second_text = second_text.replace("net_tx_bytes=500000", "net_tx_bytes=560000");
        let second = parse_sample(&second_text, 1_030.0);
        let report = build_report(&second, Some(&first), &warn(), &crit());
        // busy +500, total +800 → 62.5%
        assert!((report.cpu_pct.unwrap() - 62.5).abs() < 0.1);
        assert!((report.net_rx_bps.unwrap() - 10_000.0).abs() < 0.1);
        assert!((report.net_tx_bps.unwrap() - 2_000.0).abs() < 0.1);

        let mut reset = second.clone();
        reset.cpu_busy = Some(10);
        reset.cpu_total = Some(100);
        reset.net_rx_bytes = Some(1);
        let after_reboot = build_report(&reset, Some(&second), &warn(), &crit());
        assert_eq!(after_reboot.cpu_pct, None);
        assert_eq!(after_reboot.net_rx_bps, None);
    }

    #[test]
    fn classify_and_formatting_helpers() {
        assert_eq!(
            classify(false, None, None, None, None, &warn(), &crit()),
            HealthStatus::Unreachable
        );
        assert_eq!(
            classify(
                true,
                Some(90.0),
                Some(50.0),
                Some(0.2),
                None,
                &warn(),
                &crit()
            ),
            HealthStatus::Warn
        );
        assert_eq!(format_uptime(45.0), "45s");
        assert_eq!(format_uptime(3600.0), "1h");
        assert_eq!(format_rate(40.0), "40");
        assert_eq!(format_rate(12_300.0), "12.3K");
        assert_eq!(format_rate(1_200_000.0), "1.2M");
        assert_eq!(HealthStatus::Ok.symbol(), "✔");
    }
}
