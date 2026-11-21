//! Windows: the foreground program, and RivaTuner Statistics Server's shared memory.

use crate::RtssApp;
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::{
        Memory::{FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile},
        Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW},
    },
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

/// (process id, program file name) of the window in the foreground.
pub fn foreground_program() -> Option<(u32, String)> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
        Some((pid, name))
    }
}

// RTSSSharedMemoryV2 layout (all little-endian u32):
//   header: signature 'RTSS', version, app entry size, app array offset, app count
//   app entry: pid, name (260 bytes, ANSI), flags, time0, time1, frames, frame time
const SIGNATURE: u32 = u32::from_le_bytes(*b"SSTR"); // 'RTSS' as a DWORD
const ENTRY_NAME: usize = 4;
const ENTRY_TIME0: usize = 268;
const ENTRY_TIME1: usize = 272;
const ENTRY_FRAMES: usize = 276;
const ENTRY_MIN_SIZE: usize = 280;

/// Apps RTSS is currently hooked into. None if RTSS isn't running.
pub fn rtss_apps() -> Option<Vec<RtssApp>> {
    unsafe {
        let name: Vec<u16> = "RTSSSharedMemoryV2\0".encode_utf16().collect();
        let mapping = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
        if mapping.is_null() {
            return None;
        }
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        let result = if view.Value.is_null() {
            None
        } else {
            let base = view.Value as *const u8;
            let read = |off: usize| -> u32 { std::ptr::read_unaligned(base.add(off) as *const u32) };
            let (sig, version, entry_size, arr_offset, count) = (read(0), read(4), read(8), read(12), read(16));
            // Sanity checks before trusting the offsets
            if (sig != SIGNATURE && sig != u32::from_be_bytes(*b"SSTR"))
                || version < 0x0002_0000
                || (entry_size as usize) < ENTRY_MIN_SIZE
                || entry_size > 1 << 20
                || count > 1024
                || arr_offset < 20
            {
                None
            } else {
                let mut apps = vec![];
                for i in 0..count as usize {
                    let off = arr_offset as usize + i * entry_size as usize;
                    let pid = read(off);
                    if pid == 0 {
                        continue;
                    }
                    let name_bytes = std::slice::from_raw_parts(base.add(off + ENTRY_NAME), 260);
                    let end = name_bytes.iter().position(|&c| c == 0).unwrap_or(260);
                    apps.push(RtssApp {
                        pid,
                        name: String::from_utf8_lossy(&name_bytes[..end]).to_string(),
                        time0: read(off + ENTRY_TIME0),
                        time1: read(off + ENTRY_TIME1),
                        frames: read(off + ENTRY_FRAMES),
                    });
                }
                Some(apps)
            }
        };
        if !view.Value.is_null() {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
        }
        CloseHandle(mapping);
        result
    }
}
