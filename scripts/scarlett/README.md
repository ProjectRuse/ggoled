# Scarlett 2i2 4th Gen LED timeout

Sets the front-panel LED sleep time (0–86400 s) on a Focusrite Scarlett 2i2 4th
Gen with firmware 2417 or newer. Focusrite Control 2 only offers 10 minutes or
off; the device accepts any value, and keeps it across power cycles.

- `scarlett_fp.py`: talks to the device directly (Linux, or WSL on Windows).
  Protocol from the Linux kernel's `mixer_scarlett2.c`; firmware 2452 stores
  these settings 12 bytes later than 2417, which the script handles.
- `Reset-ScarlettLEDs.ps1`: Windows. Hands the Scarlett to WSL with `usbipd`,
  runs the script, hands it back. Audio drops for a few seconds.
- `Install-ScarlettTask.ps1`: registers a scheduled task so ggoled (or you) can
  run the reset without an admin prompt.

Toggling "Automatically turn off LEDs" in Focusrite Control 2 resets the value
to 10 minutes.

## Windows setup (once)

In an Administrator PowerShell:

```powershell
winget install usbipd                     # then reopen PowerShell
wsl -u root -- sh -c "apt update && apt install -y python3-usb"
cd <this folder>
.\Reset-ScarlettLEDs.ps1 -Status          # check it works
.\Install-ScarlettTask.ps1                # for ggoled's schedule
```

Manual use: `.\Reset-ScarlettLEDs.ps1 -Seconds 5` (admin).

## Linux

```bash
sudo apt install python3-usb
sudo python3 scarlett_fp.py status
sudo python3 scarlett_fp.py sleep 5
```
