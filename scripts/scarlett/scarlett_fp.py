#!/usr/bin/env python3
"""
scarlett_fp.py - read/set front-panel LED sleep time and brightness on a
Focusrite Scarlett 2i2 4th Gen (firmware 2417+), from Linux userspace.

Protocol, offsets and the init sequence are taken from the Linux kernel's
sound/usb/mixer_scarlett2.c (config set "gen4_2i2_2417").

Usage (needs root, or a udev rule granting access to the device):
  sudo python3 scarlett_fp.py status
  sudo python3 scarlett_fp.py sleep 5            # seconds, 0..86400
  sudo python3 scarlett_fp.py brightness low     # high | medium | low
  sudo python3 scarlett_fp.py dump 0x2c0 0x40    # read-only hex dump
  sudo python3 scarlett_fp.py find 600           # read-only value search

While it runs, the audio driver is briefly detached from the Scarlett, so
sound through it will drop out for a second or two.

Requires: python3-usb  (sudo apt install python3-usb)
"""
import struct
import sys
import time

import usb.core
import usb.util

VID, PID = 0x1235, 0x8219          # Scarlett 2i2 4th Gen
MIN_FIRMWARE = 2417

# Commands (mixer_scarlett2.c)
CMD_INIT_0_REQ = 0                  # bRequest for the initial read
CMD_REQ, CMD_RESP = 2, 3            # bRequest values for send / receive
USB_INIT_1 = 0x00000000
USB_INIT_2 = 0x00000002
USB_GET_DATA = 0x00800000
USB_SET_DATA = 0x00800001
USB_DATA_CMD = 0x00800002

# gen4_2i2_2417 config items
PARAM_BUF_ADDR = 0xFC
FP_BRIGHTNESS = dict(offset=0x2C7, activate=37)   # 1 byte, via param buffer
FP_SLEEP_TIME = dict(offset=0x2CC, activate=39)   # 4 bytes, direct write
# Firmware 2452 moved this block 12 bytes later (found by searching for the
# default sleep time of 600 s). Offsets for 2417 are from the kernel driver.
LAYOUT_SHIFT = [(2417, 0x00), (2452, 0x0C)]
BRIGHTNESS_NAMES = ["high", "medium", "low"]
MAX_SLEEP = 86400
CHUNK = 4        # bytes per read (device rejects larger GET_DATA reads)

REQ_OUT = 0x21   # host->device | class | interface
REQ_IN = 0xA1    # device->host | class | interface
HDR = struct.Struct("<IHHII")       # cmd, size, seq, error, pad


class Scarlett:
    def __init__(self):
        self.dev = usb.core.find(idVendor=VID, idProduct=PID)
        if self.dev is None:
            sys.exit("Scarlett 2i2 4th Gen not found (USB 1235:8219).")
        self.cfg = self.dev.get_active_configuration()
        self.detached = []
        self.ifnum = None
        self.ep_in = None
        for intf in self.cfg:
            if intf.bInterfaceClass == 255 and intf.bNumEndpoints >= 1:
                self.ifnum = intf.bInterfaceNumber
                self.ep_in = intf[0].bEndpointAddress
                break
        if self.ifnum is None:
            sys.exit("Couldn't find the Focusrite control interface.")
        self.seq = 0

    # --- driver handling -------------------------------------------------
    def open(self):
        # Detach the kernel audio driver from every interface, so the card is
        # torn down cleanly and rebuilt cleanly when we hand it back.
        for intf in self.cfg:
            n = intf.bInterfaceNumber
            if n in self.detached:
                continue
            try:
                if self.dev.is_kernel_driver_active(n):
                    self.dev.detach_kernel_driver(n)
                    self.detached.append(n)
            except usb.core.USBError as e:
                print(f"warning: couldn't detach interface {n}: {e}")
        usb.util.claim_interface(self.dev, self.ifnum)

    def close(self):
        try:
            usb.util.release_interface(self.dev, self.ifnum)
        except usb.core.USBError:
            pass
        for n in sorted(self.detached):
            try:
                self.dev.attach_kernel_driver(n)
            except usb.core.USBError:
                pass   # usually already re-bound when interface 0 re-probed
        time.sleep(0.5)
        failed = []
        for n in sorted(self.detached):
            try:
                if not self.dev.is_kernel_driver_active(n):
                    failed.append(n)
            except usb.core.USBError:
                pass
        if failed:
            print("Note: couldn't hand every interface back to the audio "
                  "driver. Unplug and replug the Scarlett to restore audio.")

    # --- protocol --------------------------------------------------------
    def _drain_acks(self, timeout_ms=20):
        try:
            while True:
                self.dev.read(self.ep_in, 64, timeout=timeout_ms)
        except usb.core.USBError:
            pass   # timeout = nothing pending

    def _wait_ack(self):
        try:
            self.dev.read(self.ep_in, 64, timeout=1000)
        except usb.core.USBError:
            pass   # proceed anyway; the response read will tell us

    def cmd(self, cmd, data=b"", resp_size=0):
        seq = self.seq
        self.seq += 1
        req = HDR.pack(cmd, len(data), seq, 0, 0) + data
        sent = self.dev.ctrl_transfer(REQ_OUT, CMD_REQ, 0, self.ifnum, req)
        if sent != len(req):
            raise IOError(f"cmd {cmd:#x}: sent {sent}/{len(req)} bytes")
        self._wait_ack()
        raw = bytes(self.dev.ctrl_transfer(REQ_IN, CMD_RESP, 0, self.ifnum,
                                           HDR.size + resp_size))
        if len(raw) != HDR.size + resp_size:
            info = ""
            if len(raw) >= HDR.size:
                c, sz, sq, er, pd = HDR.unpack(raw[:HDR.size])
                info = f" [size field {sz}, error {er}, seq {sq}]"
            raise IOError(f"cmd {cmd:#x}: short response "
                          f"({len(raw)} bytes){info}")
        r_cmd, r_size, r_seq, r_err, r_pad = HDR.unpack(raw[:HDR.size])
        seq_ok = (r_seq == seq) or (seq == 1 and r_seq == 0)
        if r_cmd != cmd or not seq_ok or r_size != resp_size or r_err or r_pad:
            raise IOError(f"cmd {cmd:#x}: bad response "
                          f"(cmd {r_cmd:#x} seq {r_seq}/{seq} size {r_size} "
                          f"err {r_err} pad {r_pad})")
        return raw[HDR.size:]

    def init(self):
        self.dev.ctrl_transfer(REQ_IN, CMD_INIT_0_REQ, 0, self.ifnum, 24)
        self._drain_acks()
        self.seq = 1
        self.cmd(USB_INIT_1)
        self.seq = 1
        step2 = self.cmd(USB_INIT_2, resp_size=84)
        self.firmware = struct.unpack_from("<I", step2, 8)[0]
        self.shift = 0
        for fw, shift in LAYOUT_SHIFT:
            if self.firmware >= fw:
                self.shift = shift
        return self.firmware

    def get(self, offset, size):
        return self.cmd(USB_GET_DATA, struct.pack("<II", offset, size), size)

    def set_data(self, offset, size, value):
        payload = struct.pack("<II", offset, size) + \
            value.to_bytes(size, "little")
        self.cmd(USB_SET_DATA, payload)

    def activate(self, n):
        self.cmd(USB_DATA_CMD, struct.pack("<I", n))

    # --- front panel -----------------------------------------------------
    def read_sleep(self):
        return struct.unpack("<I", self.get(FP_SLEEP_TIME["offset"] + self.shift, 4))[0]

    def read_brightness(self):
        return self.get(FP_BRIGHTNESS["offset"] + self.shift, 1)[0]

    def write_sleep(self, seconds):
        self.set_data(FP_SLEEP_TIME["offset"] + self.shift, 4, seconds)
        self.activate(FP_SLEEP_TIME["activate"])

    def write_brightness(self, level):
        self.set_data(PARAM_BUF_ADDR + 1, 1, 0)       # index
        self.set_data(PARAM_BUF_ADDR, 1, level)       # value
        self.activate(FP_BRIGHTNESS["activate"])


def show(s):
    b = s.read_brightness()
    name = BRIGHTNESS_NAMES[b] if b < len(BRIGHTNESS_NAMES) else f"? ({b})"
    print(f"Firmware:        {s.firmware}")
    print(f"Sleep time:      {s.read_sleep()} s")
    print(f"Brightness:      {name}")


def hexdump(s, start, length):
    data = b""
    pos = start
    while pos < start + length:
        n = min(CHUNK, start + length - pos)
        try:
            data += s.get(pos, n)
        except IOError:
            data += b"\xee" * n   # unreadable
        pos += n
    for i in range(0, len(data), 16):
        row = data[i:i + 16]
        print(f"{start + i:#06x}: " + " ".join(f"{b:02x}" for b in row))
    return data


def main():
    args = sys.argv[1:]
    if not args or args[0] not in ("status", "sleep", "brightness",
                                   "dump", "find"):
        sys.exit(__doc__)

    s = Scarlett()
    s.open()
    try:
        fw = s.init()
        if fw < MIN_FIRMWARE:
            sys.exit(f"Firmware {fw} is older than {MIN_FIRMWARE}; "
                     "these settings don't exist on it.")

        if args[0] == "dump":          # read-only: dump START LEN (hex ok)
            hexdump(s, int(args[1], 0), int(args[2], 0))
            return
        if args[0] == "find":          # read-only: find a u32 value
            target = int(args[1], 0).to_bytes(4, "little")
            lo = int(args[2], 0) if len(args) > 2 else 0x200
            hi = int(args[3], 0) if len(args) > 3 else 0x340
            data = b""
            pos = lo
            while pos < hi:
                try:
                    data += s.get(pos, CHUNK)
                except IOError:
                    data += b"\xff" * CHUNK   # unreadable; skip
                pos += CHUNK
            hits = [lo + i for i in range(len(data) - 3)
                    if data[i:i + 4] == target]
            print(f"{int(args[1], 0)} found at: " +
                  (", ".join(f"{h:#05x}" for h in hits) or "nowhere"))
            return

        if args[0] == "sleep":
            secs = int(args[1])
            if not 0 <= secs <= MAX_SLEEP:
                sys.exit(f"Sleep time must be 0..{MAX_SLEEP} seconds.")
            before = s.read_sleep()
            print(f"Before -> sleep time {before} s")
            if before > MAX_SLEEP:
                sys.exit("Current value doesn't look like a sleep time, so the "
                         "offset is probably wrong for this firmware. "
                         "Not writing anything.")
            s.write_sleep(secs)
        elif args[0] == "brightness":
            level = BRIGHTNESS_NAMES.index(args[1].lower())
            s.write_brightness(level)

        time.sleep(0.1)
        show(s)
    finally:
        s.close()


if __name__ == "__main__":
    main()
