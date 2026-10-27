# vcon health sample — POSIX sh, key=value lines.
# Missing tools omit keys; nothing here is fatal.

hostname=$(hostname 2>/dev/null) || true
[ -n "$hostname" ] && printf 'hostname=%s\n' "$hostname"

kernel=$(uname -r 2>/dev/null) || true
[ -n "$kernel" ] && printf 'kernel=%s\n' "$kernel"

if [ -r /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release 2>/dev/null || true
  [ -n "$PRETTY_NAME" ] && printf 'os=%s\n' "$PRETTY_NAME"
fi

if [ -r /proc/uptime ]; then
  read -r up _ </proc/uptime 2>/dev/null || true
  [ -n "$up" ] && printf 'uptime_secs=%s\n' "$up"
fi

if [ -r /proc/loadavg ]; then
  read -r l1 l5 l15 _ </proc/loadavg 2>/dev/null || true
  [ -n "$l1" ] && printf 'load1=%s\n' "$l1"
  [ -n "$l5" ] && printf 'load5=%s\n' "$l5"
  [ -n "$l15" ] && printf 'load15=%s\n' "$l15"
fi

cpus=$(nproc 2>/dev/null) || true
if [ -z "$cpus" ] && [ -r /proc/cpuinfo ]; then
  cpus=$(grep -c '^processor' /proc/cpuinfo 2>/dev/null) || true
fi
[ -n "$cpus" ] && printf 'cpus=%s\n' "$cpus"

if [ -r /proc/stat ]; then
  read -r label user nice system idle iowait irq softirq steal _ </proc/stat 2>/dev/null || true
  if [ "$label" = "cpu" ]; then
    printf 'cpu_user=%s\n' "${user:-0}"
    printf 'cpu_nice=%s\n' "${nice:-0}"
    printf 'cpu_system=%s\n' "${system:-0}"
    printf 'cpu_idle=%s\n' "${idle:-0}"
    printf 'cpu_iowait=%s\n' "${iowait:-0}"
    printf 'cpu_irq=%s\n' "${irq:-0}"
    printf 'cpu_softirq=%s\n' "${softirq:-0}"
    printf 'cpu_steal=%s\n' "${steal:-0}"
  fi
fi

if [ -r /proc/meminfo ]; then
  while read -r key value unit; do
    case "$key" in
      MemTotal:) printf 'mem_total_kb=%s\n' "$value" ;;
      MemAvailable:) printf 'mem_available_kb=%s\n' "$value" ;;
      SwapTotal:) printf 'swap_total_kb=%s\n' "$value" ;;
      SwapFree:) printf 'swap_free_kb=%s\n' "$value" ;;
    esac
  done </proc/meminfo
fi

if command -v df >/dev/null 2>&1; then
  df -P 2>/dev/null | awk 'NR > 1 && $1 !~ /^(tmpfs|devtmpfs|overlay|squashfs)$/ {
    gsub(/%/, "", $5)
    printf "disk_mount=%s\n", $6
    printf "disk_total_kb=%s\n", $2
    printf "disk_used_kb=%s\n", $3
    printf "disk_avail_kb=%s\n", $4
    printf "disk_used_pct=%s\n", $5
  }'
fi

if [ -r /proc/net/dev ]; then
  # Sum every interface except lo. awk keeps the arithmetic portable.
  awk -F'[: ]+' '
    NR > 2 && $1 != "lo" {
      rx += $2
      tx += $10
    }
    END {
      printf "net_rx_bytes=%s\n", rx + 0
      printf "net_tx_bytes=%s\n", tx + 0
    }
  ' /proc/net/dev 2>/dev/null
fi

if command -v systemctl >/dev/null 2>&1; then
  failed=$(systemctl --failed --no-legend 2>/dev/null | wc -l | tr -d ' ')
  printf 'failed_units=%s\n' "${failed:-0}"
fi

if [ -e /var/run/reboot-required ]; then
  printf 'reboot_required=1\n'
else
  printf 'reboot_required=0\n'
fi
