# System info

Shows CPU and GPU load and temperatures, memory, and FPS in games. Pick
**Display → System info** in the tray, use `display = "SystemInfo"` in a
schedule entry, or let it switch on automatically while you play.

## Layouts

- **In a game** (when RTSS is measuring it): large FPS with GPU power draw,
  CPU and GPU load and temperature, and VRAM/RAM used out of total in small
  text (e.g. `VRAM 7/16  RAM 19/64`).
- **Otherwise:** CPU, GPU, VRAM and RAM rows.

## Where the numbers come from

| Data | Source | Setup |
|---|---|---|
| CPU load, RAM | Windows | none |
| GPU load, temperature, power, VRAM | NVIDIA's NVML library, part of the driver | none (NVIDIA GPUs) |
| CPU temperature | LibreHardwareMonitor's web server | run LibreHardwareMonitor with Options > Remote Web Server > Run (default address `http://localhost:8085/data.json`) |
| FPS, game detection | RivaTuner Statistics Server (RTSS) | keep RTSS running; its own on-screen display can be off |

Memory is in the same units as Task Manager (1 GB = 1024³ bytes); RAM "used"
matches Task Manager's "In use". Anything unavailable is shown as `--`. **Settings → System info**
shows which sources are working and live readings.

## Games

With "Show system info while a game is in front" on (default), the display
switches to system info while a game is the foreground window, and stays for
`linger_secs` (default 10) after you switch away, so alt-tabbing doesn't make it
flicker. This takes priority over the schedule and holds, but not over the
lock-screen entry. The tray status line shows the game's name.

A game is:
- a foreground program RTSS is currently measuring frames for, unless it's in
  the "never treat as games" list (browsers, Discord, etc. by default, since
  RTSS can hook those too), or
- a program in the "always treat as games" list (for games RTSS doesn't hook,
  or if you don't use RTSS). Use the name from Task Manager's Details tab.

```toml
[sysinfo]
auto_when_gaming = true
game_programs = ["Warframe.x64.exe"]
ignore_programs = ["chrome.exe", "discord.exe"]
linger_secs = 10
lhm_url = "http://localhost:8085/data.json"
```
