# Schedule

ggoled can change its settings by time of day, for example a dark screen at
night and the clock during the day.

## Setting it up

1. Tray menu → **Schedule → Edit config file**. If you have no schedule yet, an
   example is added to the bottom of the file (commented out).
2. Edit the entries, save, then **Schedule → Reload config**.
3. Turn on **Schedule → Follow schedule**.

The tray's top line shows what's in effect, e.g. `Now: Night (until Fri 08:00)`.

If the file has a mistake, ggoled says where and keeps the previous settings.
It won't overwrite a file it couldn't read.

> Changing a setting from the tray rewrites the config file, which drops any
> comments you added. The schedule entries themselves are kept.

## Entries

Each entry starts at `start` on its `days` and lasts until the next entry
starts. Anything an entry leaves out uses the normal tray settings.

```toml
schedule_enabled = true

[[schedule]]
name = "Night"
days = ["daily"]
start = "00:00"
display = "Blank"
brightness = 1
scarlett_led_timeout = 5

[[schedule]]
name = "Day"
days = ["weekdays"]
start = "08:00"
display = "Normal"
brightness = 5
scarlett_led_timeout = 1800

[[schedule]]
name = "Day"
days = ["weekends"]
start = "10:00"
display = "Normal"
brightness = 5
scarlett_led_timeout = 1800
```

| Key | Values |
|---|---|
| `name` | Shown in the tray |
| `days` | `"daily"`, `"weekdays"`, `"weekends"`, day names `"mon"` … `"sun"`, or ranges like `"fri-sun"`; a list, combinable. Empty = daily |
| `start` | `"HH:MM"`, 24-hour |
| `display` | `"Normal"` (time/media as enabled), `"Blank"` (dark; volume changes still show briefly), `"Stock"` (SteelSeries UI) |
| `time_mode` | `"Off"`, `"H24"`, `"H12"` |
| `show_media` | `true` / `false` |
| `screensaver` | `true` / `false` |
| `screensaver_secs` | Seconds of no keyboard/mouse input before blanking |
| `brightness` | OLED brightness, 1–10. Stays at the last value set if a later entry doesn't specify one |
| `scarlett_led_timeout` | Seconds; see below |

## Weekends

**Schedule → Weekend days** sets what `"weekends"` means: Sat–Sun or Fri–Sun.
`"weekdays"` is everything else (Mon–Thu with a Fri–Sun weekend). For other
splits, set `weekend_days` in the config file, e.g. `weekend_days = "fri,sat,sun"`.

When entries share a name, the tray adds their days, e.g. "Day (Mon–Thu)".

## Holding an entry

**Schedule → Hold: <name>** keeps that entry in effect until the next entry
starts (e.g. hold "Day" for a late session). **End hold** returns to whatever
the schedule says right now.

## Scarlett LEDs (optional)

Each entry's `scarlett_led_timeout` can set a Focusrite Scarlett 2i2 4th Gen's
front-panel LED sleep time. This needs the one-time setup in
[`scripts/scarlett`](../scripts/scarlett/README.md), then:

```toml
[scarlett]
enabled = true
command = ["schtasks", "/run", "/tn", "ggoled Scarlett LEDs"]
```

ggoled writes the value to `ggoled_scarlett_led.txt` next to the config file and
runs `command` (`{seconds}` in an argument is replaced with the value). Changing
the timeout interrupts the Scarlett's audio for a few seconds, so ggoled waits
while media is playing, and only runs again when the value changes.
**Schedule → Re-apply Scarlett LEDs** forces it (e.g. after Focusrite Control 2
reset the timeout).
