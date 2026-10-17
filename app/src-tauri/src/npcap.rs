//! Minimal Npcap binding, loaded at runtime so the app starts (and can tell
//! the user what to do) even when Npcap is missing.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::net::Ipv4Addr;
use std::sync::Arc;

#[repr(C)]
struct PcapIf {
    next: *mut PcapIf,
    name: *mut c_char,
    description: *mut c_char,
    addresses: *mut PcapAddr,
    flags: c_uint,
}

#[repr(C)]
struct PcapAddr {
    next: *mut PcapAddr,
    addr: *mut SockAddr,
    netmask: *mut SockAddr,
    broadaddr: *mut SockAddr,
    dstaddr: *mut SockAddr,
}

#[repr(C)]
struct SockAddr {
    family: u16,
    data: [u8; 14],
}

/// `struct timeval` on Windows uses 32-bit longs.
#[repr(C)]
struct Timeval {
    sec: i32,
    usec: i32,
}

#[repr(C)]
struct PktHdr {
    ts: Timeval,
    caplen: u32,
    len: u32,
}

#[repr(C)]
struct BpfProgram {
    len: c_uint,
    insns: *mut c_void,
}

type Handle = *mut c_void;
type FnFindAllDevs = unsafe extern "C" fn(*mut *mut PcapIf, *mut c_char) -> c_int;
type FnFreeAllDevs = unsafe extern "C" fn(*mut PcapIf);
type FnCreate = unsafe extern "C" fn(*const c_char, *mut c_char) -> Handle;
type FnSetInt = unsafe extern "C" fn(Handle, c_int) -> c_int;
type FnActivate = unsafe extern "C" fn(Handle) -> c_int;
type FnCompile = unsafe extern "C" fn(Handle, *mut BpfProgram, *const c_char, c_int, c_uint) -> c_int;
type FnSetFilter = unsafe extern "C" fn(Handle, *mut BpfProgram) -> c_int;
type FnFreeCode = unsafe extern "C" fn(*mut BpfProgram);
type FnNextEx = unsafe extern "C" fn(Handle, *mut *mut PktHdr, *mut *const u8) -> c_int;
type FnDatalink = unsafe extern "C" fn(Handle) -> c_int;
type FnGetErr = unsafe extern "C" fn(Handle) -> *mut c_char;
type FnClose = unsafe extern "C" fn(Handle);
type FnLibVersion = unsafe extern "C" fn() -> *const c_char;

pub struct Npcap {
    _lib: libloading::Library,
    findalldevs: FnFindAllDevs,
    freealldevs: FnFreeAllDevs,
    create: FnCreate,
    set_snaplen: FnSetInt,
    set_promisc: FnSetInt,
    set_timeout: FnSetInt,
    set_buffer_size: FnSetInt,
    set_immediate_mode: FnSetInt,
    activate: FnActivate,
    compile: FnCompile,
    setfilter: FnSetFilter,
    freecode: FnFreeCode,
    next_ex: FnNextEx,
    datalink: FnDatalink,
    geterr: FnGetErr,
    close: FnClose,
    lib_version: FnLibVersion,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub name: String,
    pub description: String,
    pub ipv4: Vec<Ipv4Addr>,
    pub loopback: bool,
    pub disconnected: bool,
}

const ERRBUF: usize = 512;
const PCAP_IF_LOOPBACK: c_uint = 0x1;
const PCAP_IF_CONNECTION_STATUS: c_uint = 0x30;
const PCAP_IF_CONNECTION_STATUS_DISCONNECTED: c_uint = 0x20;

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

impl Npcap {
    pub fn load() -> Result<Arc<Npcap>, String> {
        #[cfg(windows)]
        unsafe {
            use std::os::windows::ffi::OsStrExt;
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
            let dir = format!("{root}\\System32\\Npcap");
            let w: Vec<u16> = std::ffi::OsStr::new(&dir).encode_wide().chain(Some(0)).collect();
            windows_sys::Win32::System::LibraryLoader::SetDllDirectoryW(w.as_ptr());
        }
        let lib = unsafe { libloading::Library::new("wpcap.dll") }
            .map_err(|e| format!("Npcap is not installed or could not be loaded ({e}). Install it from https://npcap.com"))?;
        macro_rules! sym {
            ($t:ty, $n:literal) => {
                unsafe { *lib.get::<$t>($n).map_err(|e| format!("wpcap.dll is missing {}: {e}", String::from_utf8_lossy($n)))? }
            };
        }
        // Each symbol is copied out in its own statement so no borrow of
        // `lib` is alive when it moves into the struct.
        let findalldevs: FnFindAllDevs = sym!(FnFindAllDevs, b"pcap_findalldevs\0");
        let freealldevs: FnFreeAllDevs = sym!(FnFreeAllDevs, b"pcap_freealldevs\0");
        let create: FnCreate = sym!(FnCreate, b"pcap_create\0");
        let set_snaplen: FnSetInt = sym!(FnSetInt, b"pcap_set_snaplen\0");
        let set_promisc: FnSetInt = sym!(FnSetInt, b"pcap_set_promisc\0");
        let set_timeout: FnSetInt = sym!(FnSetInt, b"pcap_set_timeout\0");
        let set_buffer_size: FnSetInt = sym!(FnSetInt, b"pcap_set_buffer_size\0");
        let set_immediate_mode: FnSetInt = sym!(FnSetInt, b"pcap_set_immediate_mode\0");
        let activate: FnActivate = sym!(FnActivate, b"pcap_activate\0");
        let compile: FnCompile = sym!(FnCompile, b"pcap_compile\0");
        let setfilter: FnSetFilter = sym!(FnSetFilter, b"pcap_setfilter\0");
        let freecode: FnFreeCode = sym!(FnFreeCode, b"pcap_freecode\0");
        let next_ex: FnNextEx = sym!(FnNextEx, b"pcap_next_ex\0");
        let datalink: FnDatalink = sym!(FnDatalink, b"pcap_datalink\0");
        let geterr: FnGetErr = sym!(FnGetErr, b"pcap_geterr\0");
        let close: FnClose = sym!(FnClose, b"pcap_close\0");
        let lib_version: FnLibVersion = sym!(FnLibVersion, b"pcap_lib_version\0");
        let np = Npcap {
            _lib: lib,
            findalldevs,
            freealldevs,
            create,
            set_snaplen,
            set_promisc,
            set_timeout,
            set_buffer_size,
            set_immediate_mode,
            activate,
            compile,
            setfilter,
            freecode,
            next_ex,
            datalink,
            geterr,
            close,
            lib_version,
        };
        Ok(Arc::new(np))
    }

    pub fn version(&self) -> String {
        unsafe { cstr((self.lib_version)()) }
    }

    pub fn devices(&self) -> Result<Vec<Device>, String> {
        let mut err = [0 as c_char; ERRBUF];
        let mut head: *mut PcapIf = std::ptr::null_mut();
        unsafe {
            if (self.findalldevs)(&mut head, err.as_mut_ptr()) != 0 {
                return Err(cstr(err.as_ptr()));
            }
            let mut out = Vec::new();
            let mut cur = head;
            while !cur.is_null() {
                let d = &*cur;
                let mut ipv4 = Vec::new();
                let mut a = d.addresses;
                while !a.is_null() {
                    let sa = (*a).addr;
                    if !sa.is_null() && (*sa).family == 2 {
                        let b = (*sa).data;
                        ipv4.push(Ipv4Addr::new(b[2], b[3], b[4], b[5]));
                    }
                    a = (*a).next;
                }
                out.push(Device {
                    name: cstr(d.name),
                    description: cstr(d.description),
                    ipv4,
                    loopback: d.flags & PCAP_IF_LOOPBACK != 0,
                    disconnected: d.flags & PCAP_IF_CONNECTION_STATUS == PCAP_IF_CONNECTION_STATUS_DISCONNECTED,
                });
                cur = d.next;
            }
            (self.freealldevs)(head);
            Ok(out)
        }
    }
}

pub struct Capture {
    np: Arc<Npcap>,
    h: Handle,
    pub linktype: u32,
}

// The handle is only ever used from the thread that owns the Capture.
unsafe impl Send for Capture {}

impl Capture {
    pub fn open(np: Arc<Npcap>, device: &str, filter: &str) -> Result<Capture, String> {
        let dev = CString::new(device).map_err(|e| e.to_string())?;
        let mut err = [0 as c_char; ERRBUF];
        unsafe {
            let h = (np.create)(dev.as_ptr(), err.as_mut_ptr());
            if h.is_null() {
                return Err(cstr(err.as_ptr()));
            }
            (np.set_snaplen)(h, 65535);
            (np.set_promisc)(h, 0);
            (np.set_timeout)(h, 50);
            (np.set_buffer_size)(h, 16 << 20);
            (np.set_immediate_mode)(h, 1);
            let r = (np.activate)(h);
            if r < 0 {
                let e = cstr((np.geterr)(h));
                (np.close)(h);
                return Err(format!("activate failed ({r}): {e}"));
            }
            let cap = Capture { linktype: (np.datalink)(h) as u32, np: np.clone(), h };
            let f = CString::new(filter).map_err(|e| e.to_string())?;
            let mut prog = BpfProgram { len: 0, insns: std::ptr::null_mut() };
            if (np.compile)(h, &mut prog, f.as_ptr(), 1, 0xffff_ffff) != 0 {
                return Err(format!("bad filter: {}", cstr((np.geterr)(h))));
            }
            let r = (np.setfilter)(h, &mut prog);
            (np.freecode)(&mut prog);
            if r != 0 {
                return Err(format!("setfilter failed: {}", cstr((np.geterr)(h))));
            }
            Ok(cap)
        }
    }

    /// Next packet: Ok(Some) on data, Ok(None) on timeout.
    pub fn next(&mut self) -> Result<Option<(f64, &[u8])>, String> {
        let mut hdr: *mut PktHdr = std::ptr::null_mut();
        let mut data: *const u8 = std::ptr::null();
        unsafe {
            match (self.np.next_ex)(self.h, &mut hdr, &mut data) {
                1 => {
                    let h = &*hdr;
                    let ts = h.ts.sec as u32 as f64 + h.ts.usec as f64 / 1e6;
                    Ok(Some((ts, std::slice::from_raw_parts(data, h.caplen as usize))))
                }
                0 => Ok(None),
                -2 => Err("capture ended".into()),
                _ => Err(cstr((self.np.geterr)(self.h))),
            }
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe { (self.np.close)(self.h) }
    }
}
