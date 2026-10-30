# Performance check on Windows: runs a release build through scripted sessions and
# reports how much CPU / memory it uses and how smoothly it draws. Needs internet
# (Home feed, one song played muted); nothing touches your real settings or library.
#   pwsh scripts/perf-windows.ps1 [-Exe path\to\music-gpui.exe] [-Settings settings.json]
#                                 [-Report out.md] [-NoChecks]
# -Settings copies a settings.json into the throw-away settings folder (e.g. your own,
# %APPDATA%\cratetune\config\settings.json, to measure your look and bar styles).
# Exits 1 when a check fails (see "Checks" at the end); -NoChecks only reports.
# What each number means: docs/TESTING.md, "Performance".
param(
  [string]$Exe = "target\release\music-gpui.exe",
  [string]$Settings,
  [string]$Report,
  [switch]$NoChecks
)
$ErrorActionPreference = "Stop"
$Root = Resolve-Path "$PSScriptRoot\.."
if (-not [IO.Path]::IsPathRooted($Exe)) { $Exe = Join-Path $Root $Exe }
if (-not (Test-Path $Exe)) { throw "no build at $Exe (cargo build --release -p music-gpui)" }
$Work = Join-Path ([IO.Path]::GetTempPath()) "cratetune-perf"
Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
New-Item -ItemType Directory $Work | Out-Null
$Cores = [Environment]::ProcessorCount

# Runs one session: a fresh settings folder, MUSIC_TIMINGS on, the given MUSIC_SCRIPT
# (which must end with @quit). Returns its log lines and one CPU / memory sample per second.
function Invoke-Session([string]$Name, [string]$Script) {
  $cfg = Join-Path $Work $Name
  New-Item -ItemType Directory $cfg | Out-Null
  if ($Settings) { Copy-Item $Settings (Join-Path $cfg "settings.json") }
  $env:MUSIC_CONFIG_DIR = $cfg
  $env:MUSIC_VOLUME = "0.0"
  $env:MUSIC_SECRET_STORE = "file"
  $env:MUSIC_TIMINGS = "1"
  $env:MUSIC_SCRIPT = $Script
  $log = Join-Path $Work "$Name.log"
  $p = Start-Process $Exe -RedirectStandardError $log -PassThru
  $samples = @()
  $prev = $null
  while (-not $p.HasExited) {
    Start-Sleep -Milliseconds 1000
    $p.Refresh()
    try { $cpu = $p.TotalProcessorTime.TotalSeconds; $at = (Get-Date) - $p.StartTime } catch { break }
    if ($prev) {
      $samples += [pscustomobject]@{
        t        = $at.TotalSeconds
        core_pct = 100 * ($cpu - $prev.cpu) / ($at - $prev.at).TotalSeconds
        private  = $p.PrivateMemorySize64 / 1MB
        ws       = $p.WorkingSet64 / 1MB
      }
    }
    $prev = @{ cpu = $cpu; at = $at }
  }
  foreach ($k in "MUSIC_CONFIG_DIR", "MUSIC_VOLUME", "MUSIC_SECRET_STORE", "MUSIC_TIMINGS", "MUSIC_SCRIPT") {
    Remove-Item "Env:$k" -ErrorAction SilentlyContinue
  }
  [pscustomobject]@{ Lines = @(Get-Content $log); Samples = $samples }
}

# The lines between the first `script @<from>` marker and the next `script @<to>` one.
function Get-Phase($Lines, [string]$From, [string]$To) {
  $on = $false
  foreach ($l in $Lines) {
    if ($l -match "script @$To\b" -and $on) { break }
    if ($l -match "script @$From\b") { $on = $true; continue }
    if ($on) { $l }
  }
}

function Get-Bench($Lines) {
  foreach ($l in $Lines) {
    if ($l -match 'bench \d+ draws: avg ([\d.]+) ms, p50 ([\d.]+), p95 ([\d.]+)') {
      [pscustomobject]@{ avg = [double]$Matches[1]; p50 = [double]$Matches[2]; p95 = [double]$Matches[3] }
    }
  }
}

# Renders per second of one view (`app` or `bar`): the median over the busy seconds of
# a phase, with the median of the per-second interval p95s. Builds before 0.2.2 print
# the app's rate without the `app` label (and no bar rate).
function Get-Rate($Lines, [string]$View) {
  $label = if ($View -eq "app") { "(?:app )?" } else { "$View " }
  $rows = foreach ($l in $Lines) {
    if ($l -match "music-gpui: $label(\d+) renders/s(, interval p50 ([\d.]+) / p95 ([\d.]+))?") {
      [pscustomobject]@{ fps = [int]$Matches[1]; p95 = if ($Matches[4]) { [double]$Matches[4] } }
    }
  }
  $busy = @($rows | Where-Object { $_.fps -ge 5 } | Sort-Object fps)
  if (-not $busy) { return $null }
  $p95 = @($busy | Where-Object p95 | Sort-Object p95)
  [pscustomobject]@{
    fps = $busy[[int](($busy.Count - 1) / 2)].fps
    p95 = if ($p95) { $p95[[int](($p95.Count - 1) / 2)].p95 }
  }
}

function Get-Usage($Samples, [double]$From, [double]$To) {
  $s = @($Samples | Where-Object { $_.t -ge $From -and $_.t -lt $To })
  if (-not $s) { return $null }
  $core = ($s | Measure-Object core_pct -Average).Average
  [pscustomobject]@{
    core    = [math]::Round($core, 1)
    machine = [math]::Round($core / $Cores, 2)
    private = [math]::Round(($s | Measure-Object private -Maximum).Maximum)
    ws      = [math]::Round(($s | Measure-Object ws -Maximum).Maximum)
  }
}

# The refresh rate of the display the window opens on (the primary one, usually).
$refresh = (Get-CimInstance Win32_VideoController | Where-Object CurrentRefreshRate |
  Sort-Object CurrentRefreshRate -Descending | Select-Object -First 1).CurrentRefreshRate
if (-not $refresh) { $refresh = 60 }
$cpuName = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name.Trim()
$gpus = (Get-CimInstance Win32_VideoController | Where-Object CurrentRefreshRate | ForEach-Object Name) -join ", "

function Wheel([double]$Start, [int]$Notches, [int]$Lines) {
  (0..($Notches - 1) | ForEach-Object { "{0}:@wheel=700;450;$Lines" -f [math]::Round($Start + $_ * 0.1, 1) }) -join ","
}

Write-Host "1/3 idle, draw cost and scrolling on Home..."
# Bench at the top (~4 s), scroll down 30 notches and back up 10, bench again, then
# sit idle. `@titlebar-status` only prints, marking the end of the scrolling.
$scroll = Invoke-Session "home" ("8:@bench=150,{0},{1},19.5:@titlebar-status,21:@bench=150,40:@quit" -f (Wheel 14 30 -3), (Wheel 17.2 10 3))
$benches = @(Get-Bench $scroll.Lines)
$scrollRate = Get-Rate (Get-Phase $scroll.Lines "wheel" "titlebar-status") "app"
$idle = Get-Usage $scroll.Samples 29 39

Write-Host "2/3 playing a song (muted) with the Equalizer bar..."
$play = Invoke-Session "playing" "4:@bar-style=equalizer,5:@search=daft punk,10:@play=0,14:@nav=home,32:@bar-style=standard,33:@quit"
$barRate = Get-Rate (Get-Phase $play.Lines "nav" "bar-style") "bar"
$playing = Get-Usage $play.Samples 18 31
$played = $play.Lines -match "script @play"

Write-Host "3/3 Now Playing open while playing..."
$np = Invoke-Session "now-playing" "5:@search=daft punk,10:@play=0,14:@open,30:@quit"
$npRate = Get-Rate (Get-Phase $np.Lines "open" "quit") "app"
$npUsage = Get-Usage $np.Samples 18 29

function Fmt($v, [string]$unit = "") { if ($null -eq $v) { "n/a" } else { "$v$unit" } }
$top = $benches | Select-Object -First 1
$deep = $benches | Select-Object -Skip 1 -First 1
$out = @(
  "# CrateTune performance ($(Get-Date -Format 'yyyy-MM-dd HH:mm'))",
  "",
  "$cpuName, $Cores threads; display $refresh Hz ($gpus)",
  "Build: $Exe$(if ($Settings) { "; settings from $Settings" })",
  "",
  "| Measure | Result |",
  "|---|---|",
  "| Idle on Home (nothing playing): CPU | $(Fmt $idle.core '%') of one core ($(Fmt $idle.machine '%') of the machine) |",
  "| Idle on Home: memory | $(Fmt $idle.private ' MB') private, $(Fmt $idle.ws ' MB') working set |",
  "| Full window draw, top of Home | p50 $(Fmt $top.p50 ' ms'), p95 $(Fmt $top.p95 ' ms') |",
  "| Full window draw, scrolled down Home | p50 $(Fmt $deep.p50 ' ms'), p95 $(Fmt $deep.p95 ' ms') |",
  "| Scrolling Home (mouse wheel) | $(Fmt $scrollRate.fps ' fps') (display $refresh Hz), interval p95 $(Fmt $scrollRate.p95 ' ms') |",
  "| Playing, Equalizer bar, Home: bar | $(Fmt $barRate.fps ' fps') (by design ~22), interval p95 $(Fmt $barRate.p95 ' ms') |",
  "| Playing, Home: CPU / memory | $(Fmt $playing.core '%') of one core, $(Fmt $playing.private ' MB') private |",
  "| Now Playing open while playing | $(Fmt $npRate.fps ' fps'), $(Fmt $npUsage.core '%') of one core, $(Fmt $npUsage.private ' MB') private |"
)

# Checks, relative to the machine where possible. Scrolling should keep up with the
# display (up to 120 Hz) on any recent laptop; before sections were culled
# (`components/cull.rs`) a 6800H managed ~100 fps on a 180 Hz panel.
$checks = @(
  @{ name = "Home loaded and benchmarked twice"; ok = $benches.Count -ge 2 },
  @{ name = "Scrolling >= 90% of min(display, 120 Hz)"; ok = $scrollRate -and $scrollRate.fps -ge 0.9 * [math]::Min($refresh, 120) },
  @{ name = "Scrolled-down draw p50 <= 1.5x top-of-Home p50"; ok = $top -and $deep -and $deep.p50 -le 1.5 * $top.p50 },
  @{ name = "Idle CPU <= 5% of one core"; ok = $idle -and $idle.core -le 5 },
  @{ name = "Playback session ran (network)"; ok = [bool]$played -and $playing }
)
$out += "", "## Checks", ""
$failed = 0
foreach ($c in $checks) {
  $mark = if ($c.ok) { "pass" } else { $failed++; "FAIL" }
  $out += "- $mark - $($c.name)"
}
$out | Write-Host
if ($Report) { $out | Set-Content -Encoding utf8 $Report }
Write-Host "logs: $Work"
if ($failed -and -not $NoChecks) { exit 1 }
