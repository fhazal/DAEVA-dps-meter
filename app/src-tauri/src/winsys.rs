//! Windows-specific helpers: process/connection lookup and global hotkeys.

use meter_core::net::Endpoint;
#[cfg(windows)]
use std::net::Ipv4Addr;

#[derive(Debug, Clone)]
pub struct Conn {
    pub local: Endpoint,
    pub remote: Endpoint,
    pub pid: u32,
}

#[cfg(windows)]
pub fn processes() -> Vec<(u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut e) != 0 {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                out.push((e.th32ProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
                if Process32NextW(snap, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

#[cfg(not(windows))]
pub fn processes() -> Vec<(u32, String)> {
    Vec::new()
}

/// Established IPv4 TCP connections with their owning process.
#[cfg(windows)]
pub fn tcp_connections() -> Vec<Conn> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_ALL};
    use windows_sys::Win32::Networking::WinSock::AF_INET;
    let mut out = Vec::new();
    unsafe {
        let mut size: u32 = 0;
        GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_ALL, 0);
        if size == 0 {
            return out;
        }
        size += 16 * 1024;
        // u32 buffer keeps the table aligned
        let mut buf: Vec<u32> = vec![0; (size as usize + 3) / 4];
        let mut sz = (buf.len() * 4) as u32;
        let r = GetExtendedTcpTable(buf.as_mut_ptr() as *mut _, &mut sz, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_ALL, 0);
        if r != 0 {
            return out;
        }
        let n = buf[0] as usize;
        let rows = buf.as_ptr().add(1) as *const MIB_TCPROW_OWNER_PID;
        let max = (buf.len() - 1) * 4 / std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
        for i in 0..n.min(max) {
            let row = std::ptr::read_unaligned(rows.add(i));
            if row.dwState != 5 {
                continue; // ESTABLISHED only
            }
            out.push(Conn {
                local: Endpoint { ip: Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()), port: u16::from_be(row.dwLocalPort as u16) },
                remote: Endpoint { ip: Ipv4Addr::from(row.dwRemoteAddr.to_ne_bytes()), port: u16::from_be(row.dwRemotePort as u16) },
                pid: row.dwOwningPid,
            });
        }
    }
    out
}

#[cfg(not(windows))]
pub fn tcp_connections() -> Vec<Conn> {
    Vec::new()
}

// ---------------------------------------------------------------- hotkeys

/// Parses "Ctrl+Shift+F9" into (modifiers, virtual key).
pub fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    const ALT: u32 = 0x1;
    const CTRL: u32 = 0x2;
    const SHIFT: u32 = 0x4;
    const WIN: u32 = 0x8;
    let mut mods = 0;
    let mut vk = None;
    for part in s.split('+').map(|p| p.trim()) {
        let up = part.to_ascii_uppercase();
        match up.as_str() {
            "CTRL" | "CONTROL" => mods |= CTRL,
            "SHIFT" => mods |= SHIFT,
            "ALT" => mods |= ALT,
            "WIN" | "SUPER" | "META" => mods |= WIN,
            k => {
                vk = Some(match k {
                    _ if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => k.as_bytes()[0] as u32,
                    _ if k.starts_with('F') && k[1..].parse::<u32>().map_or(false, |n| (1..=24).contains(&n)) => 0x6F + k[1..].parse::<u32>().unwrap(),
                    _ if k.starts_with("NUM") && k[3..].parse::<u32>().map_or(false, |n| n <= 9) => 0x60 + k[3..].parse::<u32>().unwrap(),
                    "INSERT" | "INS" => 0x2D,
                    "DELETE" | "DEL" => 0x2E,
                    "HOME" => 0x24,
                    "END" => 0x23,
                    "PAGEUP" | "PGUP" => 0x21,
                    "PAGEDOWN" | "PGDN" => 0x22,
                    "PAUSE" => 0x13,
                    "SCROLLLOCK" => 0x91,
                    "`" | "BACKQUOTE" => 0xC0,
                    "SPACE" => 0x20,
                    _ => return None,
                });
            }
        }
    }
    vk.map(|v| (mods, v))
}

pub struct HotkeyThread {
    #[allow(dead_code)]
    tid: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl HotkeyThread {
    /// Registers the bindings on a dedicated thread and calls `cb(action)`
    /// whenever one fires. Returns the bindings that failed to register.
    #[cfg(windows)]
    pub fn start(bindings: Vec<(String, String)>, cb: impl Fn(&str) + Send + 'static) -> (HotkeyThread, Vec<String>) {
        use std::sync::atomic::Ordering;
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};
        let tid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let tid2 = tid.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<String>>();
        std::thread::spawn(move || unsafe {
            tid2.store(GetCurrentThreadId(), Ordering::SeqCst);
            let mut failed = Vec::new();
            let mut actions = Vec::new();
            for (i, (action, combo)) in bindings.iter().enumerate() {
                actions.push(action.clone());
                match parse_hotkey(combo) {
                    Some((mods, vk)) => {
                        if RegisterHotKey(0 as _, (i + 1) as i32, mods | 0x4000, vk) == 0 {
                            failed.push(format!("{action} ({combo}) is already used by another program"));
                        }
                    }
                    None if combo.trim().is_empty() => {}
                    None => failed.push(format!("{action}: cannot parse '{combo}'")),
                }
            }
            let _ = tx.send(failed);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, 0 as _, 0, 0) > 0 {
                if msg.message == WM_HOTKEY {
                    if let Some(a) = actions.get((msg.wParam as usize).wrapping_sub(1)) {
                        cb(a);
                    }
                }
            }
            for i in 0..actions.len() {
                UnregisterHotKey(0 as _, (i + 1) as i32);
            }
        });
        let failed = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or_default();
        (HotkeyThread { tid }, failed)
    }

    #[cfg(not(windows))]
    pub fn start(_bindings: Vec<(String, String)>, _cb: impl Fn(&str) + Send + 'static) -> (HotkeyThread, Vec<String>) {
        (HotkeyThread { tid: Default::default() }, vec![])
    }

    pub fn stop(&self) {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
            let t = self.tid.load(std::sync::atomic::Ordering::SeqCst);
            if t != 0 {
                PostThreadMessageW(t, WM_QUIT, 0, 0);
            }
        }
    }
}

// ---------------------------------------------------------------- clipboard

#[cfg(windows)]
pub fn set_clipboard(text: &str) -> Result<(), String> {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x2;
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(0 as _) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            return Err("clipboard is busy".into());
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        if h.is_null() {
            CloseClipboard();
            return Err("out of memory".into());
        }
        let p = GlobalLock(h) as *mut u16;
        std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
        GlobalUnlock(h);
        if SetClipboardData(CF_UNICODETEXT, h as _).is_null() {
            CloseClipboard();
            return Err("could not set clipboard".into());
        }
        CloseClipboard();
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_clipboard(_text: &str) -> Result<(), String> {
    Err("clipboard only supported on Windows".into())
}

/// Screen position of the mouse cursor (physical pixels).
#[cfg(windows)]
pub fn cursor_pos() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT { x: 0, y: 0 };
    unsafe {
        if GetCursorPos(&mut p) != 0 {
            Some((p.x, p.y))
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
pub fn cursor_pos() -> Option<(i32, i32)> {
    None
}

/// Plays the Windows notification sound without blocking.
#[cfg(windows)]
pub fn chime() {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};
    let alias: Vec<u16> = "SystemNotification\0".encode_utf16().collect();
    unsafe {
        PlaySoundW(alias.as_ptr(), std::ptr::null_mut(), SND_ALIAS | SND_ASYNC | SND_NODEFAULT);
    }
}

#[cfg(not(windows))]
pub fn chime() {}

/// Opens a web page in the default browser.
#[cfg(windows)]
pub fn open_url(url: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (op, file) = (w("open"), w(url));
    let r = unsafe { ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    if (r as isize) > 32 { Ok(()) } else { Err(format!("could not open the browser ({})", r as isize)) }
}

#[cfg(not(windows))]
pub fn open_url(_url: &str) -> Result<(), String> {
    Err("not supported".into())
}
