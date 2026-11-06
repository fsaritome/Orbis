//! Safe wrapper around the Fujifilm X SDK (`XAPI.dll`) for tethered capture.
//!
//! ```no_run
//! let sdk = fuji::Sdk::load()?;
//! let cam = sdk.open_first()?;
//! println!("{}", cam.device_info()?.product);
//! cam.shoot(true)?;
//! for img in cam.wait_images(std::time::Duration::from_secs(15))? {
//!     std::fs::write(format!("shot.{}", img.extension()), &img.data)?;
//! }
//! # anyhow::Ok(())
//! ```
//!
//! The SDK is not thread-safe: keep [`Sdk`] and its [`Camera`] on one
//! thread (neither is `Send`).
//!
//! The DLLs are proprietary and not in git. They are looked up in, in order:
//! `$ORBIS_SDK_DIR`, then `sdk/bin` next to the executable or in any parent
//! folder of it or of the current directory (so `cargo run` finds the repo's
//! `sdk/bin`).

pub mod ffi;
pub mod settings;

pub use ffi::FocusArea;
pub use settings::{Choices, Setting, Values};

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::ptr;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use ffi::*;

const DLL: &str = "XAPI.dll";
/// Half-press attempts before shooting without focus lock.
const AF_ATTEMPTS: u32 = 3;

/// Locate the folder holding `XAPI.dll`.
pub fn find_sdk_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("ORBIS_SDK_DIR") {
        let dir = PathBuf::from(dir);
        if dir.join(DLL).is_file() {
            return Ok(dir);
        }
        bail!("ORBIS_SDK_DIR={} has no {DLL}", dir.display());
    }
    let mut starts = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            starts.push(d.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    for start in starts {
        for dir in start.ancestors() {
            for candidate in [dir.to_path_buf(), dir.join("sdk").join("bin")] {
                if candidate.join(DLL).is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    bail!(
        "Fujifilm SDK not found: copy REDISTRIBUTABLES/Windows/64bit/*.dll into sdk/bin \
         (see README), or set ORBIS_SDK_DIR"
    )
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}

struct Fns {
    init: FnInit,
    exit: FnExit,
    detect: FnDetect,
    open_ex: FnOpenEx,
    close: FnClose,
    get_error_number: FnGetErrorNumber,
    /// Why the last call failed (bit flags); missing in older SDKs.
    get_error_details: Option<FnGetLong>,
    get_version_string: FnGetVersionString,
    get_device_info: FnGetDeviceInfo,
    get_lens_info: FnGetLensInfo,
    set_priority_mode: FnSetLong,
    get_priority_mode: FnGetLong,
    release: FnRelease,
    get_buffer_capacity: FnGetBufferCapacity,
    read_image_info: FnReadImageInfo,
    read_image: FnReadImage,
    delete_image: FnDeleteImage,
    get_ae_mode: FnGetLong,
    get_aperture: FnGetLong,
    get_shutter_speed: FnGetShutterSpeed,
    get_sensitivity: FnGetLong,
    get_media_record: FnGetLong,
    set_media_record: FnSetLong,
    cap_ae_mode: FnCapList,
    set_ae_mode: FnSetLong,
    cap_shutter: FnCapShutter,
    set_shutter: FnSetShutterSpeed,
    cap_sensitivity: FnCapList,
    set_sensitivity: FnSetLong,
    cap_aperture: FnCapAperture,
    set_aperture: FnSetLong,
    get_lens_zoom_pos: FnGetLong,
    cap_wb: FnCapList,
    set_wb: FnSetLong,
    get_wb: FnGetLong,
    set_prop: FnSetProp1,
    set_prop0: FnSetProp0,
    focus_area_set: FnFocusArea,
    focus_area_get: FnFocusArea,
    focus_area_cap: FnCapFocusArea,
    get_prop: FnGetProp1,
    get_prop6: FnGetProp6,
    cap_prop: FnCapProp2,
}

/// The loaded SDK. Calls `XSDK_Exit` when dropped.
pub struct Sdk {
    f: Fns,
    dir: PathBuf,
    _lib: libloading::Library,
}

impl Sdk {
    /// Load `XAPI.dll` from [`find_sdk_dir`] and initialise it.
    pub fn load() -> Result<Self> {
        Self::load_from(&find_sdk_dir()?)
    }

    pub fn load_from(dir: &Path) -> Result<Self> {
        let path = dir.join(DLL);
        #[cfg(windows)]
        let lib = {
            use std::os::windows::ffi::OsStrExt;
            use libloading::os::windows::{Library, LOAD_WITH_ALTERED_SEARCH_PATH};
            // XAPI.dll loads FTLPTP*.dll and the per-model FF00xxAPI.dll by
            // name, so make its folder part of the DLL search path.
            let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
            unsafe { windows_sys::Win32::System::LibraryLoader::SetDllDirectoryW(wide.as_ptr()) };
            let lib = unsafe { Library::load_with_flags(&path, LOAD_WITH_ALTERED_SEARCH_PATH) }
                .with_context(|| format!("loading {}", path.display()))?;
            libloading::Library::from(lib)
        };
        #[cfg(not(windows))]
        let lib = unsafe { libloading::Library::new(&path) }.with_context(|| format!("loading {}", path.display()))?;

        macro_rules! sym {
            ($name:literal) => {
                *unsafe { lib.get(concat!($name, "\0").as_bytes()) }.with_context(|| format!("{} not in {DLL}", $name))?
            };
        }
        let f = Fns {
            init: sym!("XSDK_Init"),
            exit: sym!("XSDK_Exit"),
            detect: sym!("XSDK_Detect"),
            open_ex: sym!("XSDK_OpenEx"),
            close: sym!("XSDK_Close"),
            get_error_number: sym!("XSDK_GetErrorNumber"),
            get_error_details: unsafe { lib.get::<FnGetLong>(b"XSDK_GetErrorDetails\0") }.ok().map(|s| *s),
            get_version_string: sym!("XSDK_GetVersionString"),
            get_device_info: sym!("XSDK_GetDeviceInfo"),
            get_lens_info: sym!("XSDK_GetLensInfo"),
            set_priority_mode: sym!("XSDK_SetPriorityMode"),
            get_priority_mode: sym!("XSDK_GetPriorityMode"),
            release: sym!("XSDK_Release"),
            get_buffer_capacity: sym!("XSDK_GetBufferCapacity"),
            read_image_info: sym!("XSDK_ReadImageInfo"),
            read_image: sym!("XSDK_ReadImage"),
            delete_image: sym!("XSDK_DeleteImage"),
            get_ae_mode: sym!("XSDK_GetAEMode"),
            get_aperture: sym!("XSDK_GetAperture"),
            get_shutter_speed: sym!("XSDK_GetShutterSpeed"),
            get_sensitivity: sym!("XSDK_GetSensitivity"),
            get_media_record: sym!("XSDK_GetMediaRecord"),
            set_media_record: sym!("XSDK_SetMediaRecord"),
            cap_ae_mode: sym!("XSDK_CapAEMode"),
            set_ae_mode: sym!("XSDK_SetAEMode"),
            cap_shutter: sym!("XSDK_CapShutterSpeed"),
            set_shutter: sym!("XSDK_SetShutterSpeed"),
            cap_sensitivity: sym!("XSDK_CapSensitivity"),
            set_sensitivity: sym!("XSDK_SetSensitivity"),
            cap_aperture: sym!("XSDK_CapAperture"),
            set_aperture: sym!("XSDK_SetAperture"),
            get_lens_zoom_pos: sym!("XSDK_GetLensZoomPos"),
            cap_wb: sym!("XSDK_CapWBMode"),
            set_wb: sym!("XSDK_SetWBMode"),
            get_wb: sym!("XSDK_GetWBMode"),
            set_prop: sym!("XSDK_SetProp"),
            set_prop0: sym!("XSDK_SetProp"),
            focus_area_set: sym!("XSDK_SetProp"),
            focus_area_get: sym!("XSDK_GetProp"),
            focus_area_cap: sym!("XSDK_CapProp"),
            get_prop: sym!("XSDK_GetProp"),
            get_prop6: sym!("XSDK_GetProp"),
            cap_prop: sym!("XSDK_CapProp"),
        };
        // Like Fujifilm's ReleaseButton sample, pass NULL: the SDK finds its
        // model modules itself.
        let ret = unsafe { (f.init)(ptr::null_mut()) };
        if ret != XSDK_COMPLETE {
            bail!("XSDK_Init failed ({ret})");
        }
        Ok(Self { f, dir: dir.to_owned(), _lib: lib })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn version(&self) -> String {
        let mut buf = [0i8; 256];
        unsafe { (self.f.get_version_string)(buf.as_mut_ptr()) };
        cstr(&buf.map(|c| c as u8))
    }

    /// Number of cameras on USB.
    /// Cameras on USB.
    pub fn detect(&self) -> Result<u32> {
        self.detect_on(&Link::Usb)
    }

    /// Cameras reachable over `link` (the list `ENUM:<n>` opens from).
    pub fn detect_on(&self, link: &Link) -> Result<u32> {
        let mut n: Long = 0;
        let addr = match link {
            Link::Wifi(ip) => Some(CString::new(ip.trim()).context("bad IP address")?),
            _ => None,
        };
        let iface = match link {
            Link::Usb => XSDK_DSC_IF_USB,
            Link::Wifi(_) => XSDK_DSC_IF_WIFI_IP,
            Link::WifiSearch => XSDK_DSC_IF_WIFI_LOCAL,
        };
        let p = addr.as_ref().map_or(ptr::null_mut(), |a| a.as_ptr() as *mut i8);
        let ret = unsafe { (self.f.detect)(iface, p, ptr::null_mut(), &mut n) };
        if ret != XSDK_COMPLETE {
            bail!("XSDK_Detect failed: {}", self.last_error(ptr::null_mut()));
        }
        Ok(n.max(0) as u32)
    }

    /// Open the first USB camera in tether mode and take control (PC priority).
    pub fn open_first(&self) -> Result<Camera<'_>> {
        self.open(&Link::Usb)
    }

    /// Open the first camera on `link` in tether mode and take control.
    pub fn open(&self, link: &Link) -> Result<Camera<'_>> {
        if self.detect_on(link)? == 0 {
            match link {
                Link::Usb => bail!(
                    "no Fujifilm camera found on USB. Is it on, connected, and set to CONNECTION MODE → USB TETHER SHOOTING AUTO?"
                ),
                Link::Wifi(ip) => bail!(
                    "no camera answering at {ip}. Is it on, in PC CONNECTION MODE → WIRELESS TETHER SHOOTING FIXED, and connected to the Wi-Fi (lamp blinking orange or green)?"
                ),
                Link::WifiSearch => bail!("no camera found on the local network"),
            }
        }
        let dev = CString::new("ENUM:0").unwrap();
        let mut h: XSDK_HANDLE = ptr::null_mut();
        let mut mode: Long = 0;
        let ret = unsafe { (self.f.open_ex)(dev.as_ptr(), &mut h, &mut mode, ptr::null_mut()) };
        if ret != XSDK_COMPLETE {
            bail!("XSDK_OpenEx failed: {}", self.last_error(h));
        }
        let cam = Camera { sdk: self, h, mode };
        if mode & XSDK_DSC_MODE_TETHER == 0 {
            bail!(
                "camera is connected but not in tether mode (mode flags {mode:#x}). Set CONNECTION MODE → USB TETHER SHOOTING AUTO (USB) or WIRELESS TETHER SHOOTING FIXED (Wi-Fi) in the camera menu"
            );
        }
        let take_control = || cam.call("XSDK_SetPriorityMode", unsafe { (self.f.set_priority_mode)(h, XSDK_PRIORITY_PC) });
        if let Err(first) = take_control() {
            // A session that ended without cleaning up (crash, killed
            // process) can leave live view streaming, and the camera then
            // answers BUSY. Stop it and try again.
            unsafe { (self.f.set_prop0)(h, API_CODE_STOP_LIVE_VIEW, 0) };
            sleep(Duration::from_millis(500));
            while matches!(cam.read_next(), Ok(Some(ref i)) if i.is_live()) {}
            take_control().map_err(|_| first)?;
        }
        Ok(cam)
    }

    fn last_error(&self, h: XSDK_HANDLE) -> String {
        let (mut api, mut code) = (0, 0);
        unsafe { (self.f.get_error_number)(h, &mut api, &mut code) };
        let mut msg = format!("{} ({code:#06x}, API {api:#06x})", errcode_name(code));
        let why = self.error_details(h);
        if !why.is_empty() {
            msg.push_str(&format!(": {}", why.join(", ")));
        }
        msg
    }

    /// The camera's reasons for the last failure, in words.
    fn error_details(&self, h: XSDK_HANDLE) -> Vec<&'static str> {
        let Some(f) = self.f.get_error_details else { return Vec::new() };
        let mut bits = 0;
        if unsafe { f(h, &mut bits) } != XSDK_COMPLETE {
            return Vec::new();
        }
        ERROR_DETAILS.iter().filter(|(b, _)| bits & b != 0).map(|(_, s)| *s).collect()
    }
}

impl Drop for Sdk {
    fn drop(&mut self) {
        unsafe { (self.f.exit)() };
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub vendor: String,
    pub product: String,
    pub firmware: String,
    pub serial: String,
    pub framework: String,
}

/// Exposure settings as the camera reports them.
#[derive(Debug, Clone, Copy)]
pub struct Exposure {
    pub ae_mode: i32,
    /// Raw SDK aperture value (F-number × 100), `0xFFFF` = auto.
    pub aperture: i32,
    /// Raw SDK shutter value (≈ microseconds), `-1` bulb, `0` unknown.
    pub shutter: i32,
    /// ISO, negative = auto variants.
    pub iso: i32,
}

impl Exposure {
    pub fn ae_mode_name(&self) -> &'static str {
        match self.ae_mode {
            XSDK_AE_OFF => "M (manual)",
            XSDK_AE_APERTURE_PRIORITY => "A (aperture priority)",
            XSDK_AE_SHUTTER_PRIORITY => "S (shutter priority)",
            XSDK_AE_PROGRAM => "P (program)",
            _ => "?",
        }
    }

    pub fn aperture_text(&self) -> String {
        match self.aperture {
            XSDK_IRIS_AUTO => "auto".into(),
            XSDK_IRIS_NONE => "—".into(),
            v => format!("f/{}", v as f64 / 100.0),
        }
    }

    pub fn shutter_text(&self) -> String {
        shutter_text(self.shutter)
    }

    pub fn iso_text(&self) -> String {
        match self.iso {
            v if v > 0 => format!("ISO {v}"),
            v if v < -10 => format!("auto (max {})", -v),
            _ => "auto".into(),
        }
    }
}

/// Human shutter speed from the SDK's microsecond-ish code.
pub fn shutter_text(us: i32) -> String {
    // Marked third-stop speeds as printed on Fujifilm dials and menus.
    const FRACTIONS: [&str; 45] = [
        "32000", "25000", "20000", "16000", "13000", "10000", "8000", "6400", "5000", "4000", "3200", "2500",
        "2000", "1600", "1250", "1000", "800", "640", "500", "400", "320", "250", "200", "160", "125", "100", "80",
        "60", "50", "40", "30", "25", "20", "15", "13", "10", "8", "6", "5", "4", "3", "2.5", "2", "1.6", "1.3",
    ];
    const SECONDS: [&str; 16] =
        ["1", "1.3", "1.6", "2", "2.5", "3", "4", "5", "6", "8", "10", "13", "15", "20", "25", "30"];
    let nearest = |names: &[&str], x: f64| -> String {
        names
            .iter()
            .min_by(|a, b| {
                let da = (a.parse::<f64>().unwrap().ln() - x.ln()).abs();
                let db = (b.parse::<f64>().unwrap().ln() - x.ln()).abs();
                da.total_cmp(&db)
            })
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    match us {
        XSDK_SHUTTER_BULB => "bulb".into(),
        XSDK_SHUTTER_UNKNOWN => "—".into(),
        // The SDK uses powers of two internally (1/125 = 7812 µs = 1/128), so
        // map to the nearest marked speed in log space.
        v if v >= 900_000 => format!("{}\"", nearest(&SECONDS, v as f64 / 1e6)),
        v => format!("1/{}", nearest(&FRACTIONS, 1e6 / v as f64)),
    }
}

/// A picture transferred from the camera.
#[derive(Debug, Clone)]
pub struct Image {
    pub name: String,
    pub format: i32,
    pub width: i32,
    pub height: i32,
    pub data: Vec<u8>,
}

impl Image {
    /// A live view frame rather than a photo.
    pub fn is_live(&self) -> bool {
        self.format & 0xFF == XSDK_IMAGEFORMAT_LIVE
    }

    pub fn extension(&self) -> &'static str {
        match self.format & 0xFF {
            XSDK_IMAGEFORMAT_RAW => "RAF",
            XSDK_IMAGEFORMAT_JPEG => "JPG",
            XSDK_IMAGEFORMAT_HEIF => "HIF",
            _ => "bin",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Shot {
    /// Autofocus reported success (or wasn't used).
    pub focus_ok: bool,
}

/// An open camera. Gives control back to the camera and closes on drop.
/// How to reach the camera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Usb,
    /// Wi-Fi tethering, camera at this IPv4 address.
    Wifi(String),
    /// Wi-Fi tethering, search the local network.
    WifiSearch,
}

impl std::fmt::Display for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Link::Usb => write!(f, "USB"),
            Link::Wifi(ip) => write!(f, "Wi-Fi {ip}"),
            Link::WifiSearch => write!(f, "Wi-Fi"),
        }
    }
}

pub struct Camera<'a> {
    sdk: &'a Sdk,
    h: XSDK_HANDLE,
    mode: Long,
}

impl Camera<'_> {
    fn call(&self, api: &str, ret: Long) -> Result<()> {
        if ret == XSDK_COMPLETE {
            Ok(())
        } else {
            Err(anyhow!("{api} failed: {}", self.sdk.last_error(self.h)))
        }
    }

    fn get_long(&self, api: &str, f: FnGetLong) -> Result<i32> {
        let mut v = 0;
        self.call(api, unsafe { f(self.h, &mut v) })?;
        Ok(v)
    }

    /// Capability flags from `XSDK_OpenEx` (`XSDK_DSC_MODE_*`).
    pub fn mode_flags(&self) -> i32 {
        self.mode
    }

    pub fn device_info(&self) -> Result<DeviceInfo> {
        let mut d: XSDK_DeviceInformation = unsafe { std::mem::zeroed() };
        self.call("XSDK_GetDeviceInfo", unsafe { (self.sdk.f.get_device_info)(self.h, &mut d) })?;
        Ok(DeviceInfo {
            vendor: cstr(&d.strVendor),
            product: cstr(&d.strProduct),
            firmware: cstr(&d.strFirmware),
            serial: cstr(&d.strSerialNo),
            framework: cstr(&d.strFramework),
        })
    }

    pub fn lens(&self) -> Result<String> {
        let mut l: XSDK_LensInformation = unsafe { std::mem::zeroed() };
        self.call("XSDK_GetLensInfo", unsafe { (self.sdk.f.get_lens_info)(self.h, &mut l) })?;
        let name = cstr(&l.strProductName);
        Ok(if name.is_empty() { cstr(&l.strModel) } else { name })
    }

    pub fn exposure(&self) -> Result<Exposure> {
        let f = &self.sdk.f;
        let mut shutter = 0;
        let mut bulb = 0;
        self.call("XSDK_GetShutterSpeed", unsafe { (f.get_shutter_speed)(self.h, &mut shutter, &mut bulb) })?;
        Ok(Exposure {
            ae_mode: self.get_long("XSDK_GetAEMode", f.get_ae_mode)?,
            aperture: self.get_long("XSDK_GetAperture", f.get_aperture)?,
            shutter,
            iso: self.get_long("XSDK_GetSensitivity", f.get_sensitivity)?,
        })
    }

    /// What the camera stores (`XSDK_MEDIAREC_*`).
    pub fn media_record(&self) -> Result<i32> {
        self.get_long("XSDK_GetMediaRecord", self.sdk.f.get_media_record)
    }

    pub fn set_media_record(&self, v: i32) -> Result<()> {
        self.call("XSDK_SetMediaRecord", unsafe { (self.sdk.f.set_media_record)(self.h, v) })
    }

    /// Raw state for troubleshooting refused releases: buffer, recording
    /// medium, priority, auto power-off, and the last error's details.
    pub fn diagnostics(&self) -> Vec<(String, String)> {
        let mut d = Vec::new();
        let (mut shot, mut total) = (0, 0);
        let r = unsafe { (self.sdk.f.get_buffer_capacity)(self.h, &mut shot, &mut total) };
        d.push(("buffer (shots / total frames)".into(), if r == XSDK_COMPLETE { format!("{shot} / {total}") } else { self.sdk.last_error(self.h) }));
        d.push((
            "media record".into(),
            match self.media_record() {
                Ok(1) => "RAW+JPEG to card".into(),
                Ok(2) => "RAW to card".into(),
                Ok(3) => "JPEG to card".into(),
                Ok(4) => "off (PC only)".into(),
                Ok(v) => format!("{v}"),
                Err(e) => format!("{e:#}"),
            },
        ));
        let mut prio = 0;
        let r = unsafe { (self.sdk.f.get_priority_mode)(self.h, &mut prio) };
        d.push(("priority".into(), if r == XSDK_COMPLETE { (if prio == XSDK_PRIORITY_PC { "PC" } else { "camera" }).into() } else { self.sdk.last_error(self.h) }));
        d.push(("custom auto power-off".into(), self.get_prop(API_CODE_GET_CUSTOM_AUTO_POWER_OFF).map(|v| v.to_string()).unwrap_or_else(|e| format!("{e:#}"))));
        d.push(("battery raw [body, grip1, grip2, body %, grip1 %, grip2 %]".into(), self.battery_raw().map(|v| format!("{v:?}")).unwrap_or_else(|e| format!("{e:#}"))));
        d.push(("last error".into(), self.sdk.last_error(self.h)));
        d
    }

    /// Images waiting in the camera's transfer buffer.
    pub fn buffered(&self) -> Result<u32> {
        let (mut shot, mut total) = (0, 0);
        self.call("XSDK_GetBufferCapacity", unsafe {
            (self.sdk.f.get_buffer_capacity)(self.h, &mut shot, &mut total)
        })?;
        Ok(shot.max(0) as u32)
    }

    /// Take a picture. `autofocus` = half-press first (S1 → S2); without it
    /// the camera shoots with the current focus (use with MF).
    pub fn shoot(&self, autofocus: bool) -> Result<Shot> {
        // The X-T4 sometimes refuses a release right after the previous
        // photo (SHOOT_ERROR while it is still busy): wait and try once more.
        match self.shoot_once(autofocus) {
            Err(e) if !e.to_string().starts_with("autofocus failed") => {
                sleep(Duration::from_millis(1500));
                self.shoot_once(autofocus).map_err(|e2| anyhow!("{e2} (also on the first try: {e})"))
            }
            r => r,
        }
    }

    fn shoot_once(&self, autofocus: bool) -> Result<Shot> {
        // Like Fujifilm's ReleaseButton sample: half-press (S1) first so the
        // camera focuses, then full press + release (S2, S1 off). `shot_opt`
        // is the number of frames and must be 1.
        let release = |mode: Long, api: &str| -> Result<Long> {
            let (mut shot_opt, mut status) = (1, XSDK_RELEASE_OK);
            let ret = unsafe { (self.sdk.f.release)(self.h, mode, &mut shot_opt, &mut status) };
            if ret != XSDK_COMPLETE {
                let err = anyhow!("{api} failed: {}", self.sdk.last_error(self.h));
                // Never leave the shutter button half-pressed.
                let (mut o, mut s) = (1, 0);
                unsafe { (self.sdk.f.release)(self.h, XSDK_RELEASE_CANCEL, &mut o, &mut s) };
                return Err(err);
            }
            Ok(status)
        };

        if !autofocus {
            release(XSDK_RELEASE_SHOOT_S1OFF, "XSDK_Release(SHOOT)")?;
            return Ok(Shot { focus_ok: true });
        }
        // Autofocus can miss in dim light; try a few times before giving up.
        let mut focus_ok = false;
        for attempt in 0..AF_ATTEMPTS {
            let af = release(XSDK_RELEASE_S1ON, "XSDK_Release(S1 focus)")?;
            if af != XSDK_RELEASE_AF_FAILURE {
                focus_ok = true;
                break;
            }
            if attempt + 1 < AF_ATTEMPTS {
                let (mut o, mut s) = (1, 0);
                unsafe { (self.sdk.f.release)(self.h, XSDK_RELEASE_N_S1OFF, &mut o, &mut s) };
                sleep(Duration::from_millis(300));
            }
        }
        if !focus_ok {
            // In AF-S with focus priority (the X-T4 default) the camera
            // silently refuses to fire without focus lock, so don't try.
            let (mut o, mut s) = (1, 0);
            unsafe { (self.sdk.f.release)(self.h, XSDK_RELEASE_N_S1OFF, &mut o, &mut s) };
            bail!(
                "autofocus failed {AF_ATTEMPTS}× (too dark, no contrast, or too close). Add light, \
                 or focus once, set the camera to MF and switch off \"Autofocus before every photo\""
            );
        }
        release(XSDK_RELEASE_S2_S1OFF, "XSDK_Release(S2 shoot)")?;
        Ok(Shot { focus_ok })
    }

    /// Read the head of the transfer queue: a photo, a live view frame, or
    /// nothing.
    fn read_next(&self) -> Result<Option<Image>> {
        let mut info: XSDK_ImageInformation = unsafe { std::mem::zeroed() };
        info.lFormat = XSDK_IMAGEFORMAT_NONE;
        let ret = unsafe { (self.sdk.f.read_image_info)(self.h, &mut info) };
        if ret != XSDK_COMPLETE {
            // "Nothing there" also comes back as an error on some bodies.
            return Ok(None);
        }
        let format = info.lFormat;
        let size = info.lDataSize;
        let known = matches!(
            format & 0xFF,
            XSDK_IMAGEFORMAT_RAW | XSDK_IMAGEFORMAT_JPEG | XSDK_IMAGEFORMAT_HEIF | XSDK_IMAGEFORMAT_LIVE
        );
        if !known || size <= 0 {
            return Ok(None);
        }
        let mut data = vec![0u8; size as usize];
        self.call("XSDK_ReadImage", unsafe { (self.sdk.f.read_image)(self.h, data.as_mut_ptr(), size as u32) })?;
        let (width, height) = (info.lImagePixWidth, info.lImagePixHeight);
        Ok(Some(Image { name: cstr(&info.strInternalName), format, width, height, data }))
    }

    /// Fetch the next photo from the transfer buffer, if any. Live view
    /// frames queued in front of it are skipped.
    pub fn next_image(&self) -> Result<Option<Image>> {
        loop {
            match self.read_next()? {
                Some(img) if img.is_live() => continue,
                other => return Ok(other),
            }
        }
    }

    /// Start streaming live view frames (JPEG, 1280 px) into the transfer
    /// queue; read them with [`Camera::next_live_frame`].
    pub fn start_live_view(&self) -> Result<()> {
        let f = &self.sdk.f;
        // Quality, size and mode are best effort: not every body has them.
        unsafe {
            (f.set_prop)(self.h, API_CODE_SET_LIVE_VIEW_QUALITY, 1, SDK_LIVEVIEW_QUALITY_FINE);
            (f.set_prop)(self.h, API_CODE_SET_LIVE_VIEW_SIZE, 1, SDK_LIVEVIEW_SIZE_L);
            (f.set_prop)(self.h, API_CODE_SET_LIVE_VIEW_MODE, 1, SDK_LIVEVIEW_MODE1);
        }
        self.call("StartLiveView", unsafe { (f.set_prop0)(self.h, API_CODE_START_LIVE_VIEW, 0) })
    }

    pub fn stop_live_view(&self) -> Result<()> {
        let r = self.call("StopLiveView", unsafe { (self.sdk.f.set_prop0)(self.h, API_CODE_STOP_LIVE_VIEW, 0) });
        // Throw away frames still queued.
        while matches!(self.read_next(), Ok(Some(ref i)) if i.is_live()) {}
        r
    }

    /// Newest live view frame (JPEG bytes), dropping older ones. Photos in
    /// the queue are left for [`Camera::next_image`].
    pub fn next_live_frame(&self) -> Result<Option<Vec<u8>>> {
        let mut newest = None;
        // Peek-and-read: stop at the first non-live item.
        for _ in 0..8 {
            let mut info: XSDK_ImageInformation = unsafe { std::mem::zeroed() };
            info.lFormat = XSDK_IMAGEFORMAT_NONE;
            if unsafe { (self.sdk.f.read_image_info)(self.h, &mut info) } != XSDK_COMPLETE {
                break;
            }
            let (format, size) = (info.lFormat, info.lDataSize);
            if format & 0xFF != XSDK_IMAGEFORMAT_LIVE || size <= 0 {
                break;
            }
            let mut data = vec![0u8; size as usize];
            self.call("XSDK_ReadImage", unsafe { (self.sdk.f.read_image)(self.h, data.as_mut_ptr(), size as u32) })?;
            newest = Some(data);
        }
        Ok(newest)
    }

    /// Current focus point (grid position and size).
    pub fn focus_area(&self) -> Result<FocusArea> {
        let mut a = FocusArea::default();
        let ret = unsafe { (self.sdk.f.focus_area_get)(self.h, API_CODE_GET_FOCUS_AREA, 2, SDK_ITEM_DIRECTION_0, &mut a) };
        self.call("GetFocusArea", ret)?;
        Ok(a)
    }

    /// Smallest and largest focus point position/size the camera accepts.
    pub fn focus_area_range(&self) -> Result<(FocusArea, FocusArea)> {
        let (mut min, mut max) = (FocusArea::default(), FocusArea::default());
        let ret = unsafe {
            (self.sdk.f.focus_area_cap)(self.h, API_CODE_CAP_FOCUS_AREA, 3, SDK_ITEM_DIRECTION_0, &mut min, &mut max)
        };
        self.call("CapFocusArea", ret)?;
        Ok((min, max))
    }

    /// Half-extents of the focus point grid: positions run from `-h..=h`
    /// and `-v..=v` with (0, 0) in the centre. Depends on the camera's
    /// NUMBER OF FOCUS POINTS setting (X-T4: 13×9 or 25×17).
    pub fn focus_grid(&self) -> (i32, i32) {
        match self.get_prop(API_CODE_GET_FOCUS_POINTS) {
            Ok(1) => (6, 3),  // 13×7
            Ok(2) => (12, 6), // 25×13
            Ok(4) => (12, 8), // 25×17
            _ => (6, 4),      // 13×9, measured on the X-T4
        }
    }

    pub fn set_focus_area(&self, mut a: FocusArea) -> Result<()> {
        let ret = unsafe { (self.sdk.f.focus_area_set)(self.h, API_CODE_SET_FOCUS_AREA, 2, SDK_ITEM_DIRECTION_0, &mut a) };
        let (h, v, size) = (a.h, a.v, a.size);
        self.call("SetFocusArea", ret).with_context(|| format!("focus point h={h} v={v} size={size}"))
    }

    /// Run autofocus once without taking a picture. In MF this uses the
    /// AF-ON style instant AF. Returns whether focus locked.
    pub fn focus_now(&self) -> Result<bool> {
        let f = &self.sdk.f;
        let manual = self.get(Setting::Focus).ok() == Some(SDK_FOCUS_MANUAL);
        let (on, off) = if manual {
            (XSDK_RELEASE_INSTANTAF, XSDK_RELEASE_N_INSTANTAF)
        } else {
            (XSDK_RELEASE_S1ON, XSDK_RELEASE_N_S1OFF)
        };
        let (mut opt, mut status) = (1, XSDK_RELEASE_OK);
        let ret = unsafe { (f.release)(self.h, on, &mut opt, &mut status) };
        let (mut o, mut s) = (1, 0);
        unsafe { (f.release)(self.h, off, &mut o, &mut s) };
        self.call("XSDK_Release(focus)", ret)?;
        Ok(status != XSDK_RELEASE_AF_FAILURE)
    }

    /// Wait until the camera has delivered at least one image, then return
    /// everything in the buffer (RAW+JPEG arrives as two images).
    pub fn wait_images(&self, timeout: Duration) -> Result<Vec<Image>> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        loop {
            match self.next_image()? {
                Some(img) => out.push(img),
                None if !out.is_empty() => {
                    // Give a second file (RAW+JPEG) a moment to arrive.
                    sleep(Duration::from_millis(300));
                    match self.next_image()? {
                        Some(img) => out.push(img),
                        None => return Ok(out),
                    }
                }
                None if Instant::now() >= deadline => bail!("no image arrived within {timeout:?}"),
                None => sleep(Duration::from_millis(100)),
            }
        }
    }

    pub fn delete_current_image(&self) -> Result<()> {
        self.call("XSDK_DeleteImage", unsafe { (self.sdk.f.delete_image)(self.h) })
    }

    /// Check the camera is still there.
    pub fn alive(&self) -> bool {
        self.device_info().is_ok()
    }

    /// Two-pass list query: count first, then the values.
    fn list(&self, api: &str, mut f: impl FnMut(*mut Long, *mut Long) -> Long) -> Result<Vec<i32>> {
        let mut n: Long = 0;
        self.call(api, f(&mut n, ptr::null_mut()))?;
        if n <= 0 {
            return Ok(Vec::new());
        }
        let mut v = vec![0; n as usize];
        self.call(api, f(&mut n, v.as_mut_ptr()))?;
        v.truncate(n.max(0) as usize);
        Ok(v)
    }

    /// Body battery charge in percent (`None` on mains power / unknown).
    /// A weak battery makes the X-T4 refuse releases (SHOOT_ERROR) and then
    /// switch off, so the app shows this and warns early.
    pub fn battery(&self) -> Result<Option<u8>> {
        let mut v = [0 as Long; 6];
        let [a, b, c, d, e, f] = &mut v;
        self.call("XSDK_GetProp(CheckBatteryInfo)", unsafe {
            (self.sdk.f.get_prop6)(self.h, API_CODE_CHECK_BATTERY_INFO, 6, a, b, c, d, e, f)
        })?;
        // [body level, grip 1 level, grip 2 level, body %, grip 1 %, grip 2 %]
        let (level, pct) = (v[0], v[3]);
        Ok(match level {
            0x00FF | 0x000D => None, // DC power / charging
            _ if (0..=100).contains(&pct) => Some(pct as u8),
            _ => None,
        })
    }

    /// Raw battery values, for diagnostics.
    pub fn battery_raw(&self) -> Result<[i32; 6]> {
        let mut v = [0 as Long; 6];
        let [a, b, c, d, e, f] = &mut v;
        self.call("XSDK_GetProp(CheckBatteryInfo)", unsafe {
            (self.sdk.f.get_prop6)(self.h, API_CODE_CHECK_BATTERY_INFO, 6, a, b, c, d, e, f)
        })?;
        Ok(v)
    }

    /// The custom length of the camera's "2 MIN" auto power-off as stored
    /// in the camera (the SDK manual: 1–254 = 10–263 s, 255 = never; 0 seen
    /// on an X-T4 that was never customised).
    pub fn custom_power_off(&self) -> Result<i32> {
        self.get_prop(API_CODE_GET_CUSTOM_AUTO_POWER_OFF)
    }

    /// Writes that value. Orbis doesn't call this on its own any more: on
    /// the X-T4 writing it made the camera switch itself off.
    pub fn set_custom_power_off(&self, v: i32) -> Result<()> {
        self.call("XSDK_SetProp(CustomAutoPowerOff)", unsafe {
            (self.sdk.f.set_prop)(self.h, API_CODE_SET_CUSTOM_AUTO_POWER_OFF, 1, v)
        })
    }

    fn get_prop(&self, code: Long) -> Result<i32> {
        let mut v = 0;
        self.call("XSDK_GetProp", unsafe { (self.sdk.f.get_prop)(self.h, code, 1, &mut v) })?;
        Ok(v)
    }

    /// Current value of one setting.
    pub fn get(&self, s: Setting) -> Result<i32> {
        let f = &self.sdk.f;
        match s {
            Setting::Mode => self.get_long("XSDK_GetAEMode", f.get_ae_mode),
            Setting::Aperture => self.get_long("XSDK_GetAperture", f.get_aperture),
            Setting::Shutter => {
                let (mut v, mut bulb) = (0, 0);
                self.call("XSDK_GetShutterSpeed", unsafe { (f.get_shutter_speed)(self.h, &mut v, &mut bulb) })?;
                Ok(v)
            }
            Setting::Iso => self.get_long("XSDK_GetSensitivity", f.get_sensitivity),
            Setting::WhiteBalance => self.get_long("XSDK_GetWBMode", f.get_wb),
            Setting::Focus => self.get_prop(API_CODE_GET_FOCUS_MODE),
            Setting::Quality => self.get_prop(API_CODE_GET_IMAGE_QUALITY),
            Setting::FilmSimulation => self.get_prop(API_CODE_GET_FILM_SIMULATION),
        }
    }

    /// Change one setting. The camera must be in PC priority (it is after
    /// [`Sdk::open_first`]); some values need a matching mode (e.g. shutter
    /// speeds only in M and S).
    pub fn set(&self, s: Setting, v: i32) -> Result<()> {
        let f = &self.sdk.f;
        let h = self.h;
        let (api, ret) = unsafe {
            match s {
                Setting::Mode => ("XSDK_SetAEMode", (f.set_ae_mode)(h, v)),
                Setting::Aperture => ("XSDK_SetAperture", (f.set_aperture)(h, v)),
                Setting::Shutter => ("XSDK_SetShutterSpeed", (f.set_shutter)(h, v, 0)),
                Setting::Iso => ("XSDK_SetSensitivity", (f.set_sensitivity)(h, v)),
                Setting::WhiteBalance => ("XSDK_SetWBMode", (f.set_wb)(h, v)),
                Setting::Focus => ("SetFocusMode", (f.set_prop)(h, API_CODE_SET_FOCUS_MODE, 1, v)),
                Setting::Quality => ("SetImageQuality", (f.set_prop)(h, API_CODE_SET_IMAGE_QUALITY, 1, v)),
                Setting::FilmSimulation => {
                    ("SetFilmSimulationMode", (f.set_prop)(h, API_CODE_SET_FILM_SIMULATION, 1, v))
                }
            }
        };
        self.call(api, ret).with_context(|| format!("setting {} to {}", s.label(), s.value_name(v)))
    }

    /// Values each setting accepts right now. Settings whose list can't be
    /// queried are left out (their current value is still shown).
    pub fn choices(&self) -> Choices {
        let f = &self.sdk.f;
        let h = self.h;
        let mut out = Vec::new();
        let mut add = |s: Setting, r: Result<Vec<i32>>| {
            if let Ok(v) = r {
                if !v.is_empty() {
                    out.push((s, v));
                }
            }
        };
        add(Setting::Mode, self.list("XSDK_CapAEMode", |n, v| unsafe { (f.cap_ae_mode)(h, n, v) }));
        let zoom = self.get_long("XSDK_GetLensZoomPos", f.get_lens_zoom_pos).unwrap_or(0);
        add(Setting::Aperture, self.list("XSDK_CapAperture", |n, v| unsafe { (f.cap_aperture)(h, zoom, n, v) }));
        let mut bulb = 0;
        add(Setting::Shutter, self.list("XSDK_CapShutterSpeed", |n, v| unsafe { (f.cap_shutter)(h, n, v, &mut bulb) }));
        add(Setting::Iso, self.list("XSDK_CapSensitivity", |n, v| unsafe { (f.cap_sensitivity)(h, n, v) }));
        add(Setting::WhiteBalance, self.list("XSDK_CapWBMode", |n, v| unsafe { (f.cap_wb)(h, n, v) }));
        add(
            Setting::Focus,
            self.list("CapFocusMode", |n, v| unsafe { (f.cap_prop)(h, API_CODE_CAP_FOCUS_MODE, 2, n, v) }),
        );
        add(Setting::Quality, Ok(settings::QUALITIES.to_vec()));
        add(Setting::FilmSimulation, Ok(settings::FILM_SIMULATIONS.to_vec()));
        Choices(out)
    }

    /// Current value of every setting.
    pub fn values(&self) -> Values {
        Values(Setting::ALL.iter().map(|&s| (s, self.get(s).ok())).collect())
    }

    pub fn priority_is_pc(&self) -> Result<bool> {
        Ok(self.get_long("XSDK_GetPriorityMode", self.sdk.f.get_priority_mode)? == XSDK_PRIORITY_PC)
    }
}

impl Drop for Camera<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.sdk.f.set_priority_mode)(self.h, XSDK_PRIORITY_CAMERA);
            (self.sdk.f.close)(self.h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutter_codes_read_like_the_dial() {
        assert_eq!(shutter_text(7812), "1/125"); // XSDK_SHUTTER_1_125
        assert_eq!(shutter_text(244), "1/4000"); // XSDK_SHUTTER_1_4000
        assert_eq!(shutter_text(775), "1/1250");
        assert_eq!(shutter_text(1_000_000), "1\"");
        assert_eq!(shutter_text(2_000_000), "2\"");
        assert_eq!(shutter_text(-1), "bulb");
    }

    #[test]
    fn exposure_texts() {
        let e = Exposure { ae_mode: XSDK_AE_OFF, aperture: 800, shutter: 7812, iso: 160 };
        assert_eq!(e.aperture_text(), "f/8");
        assert_eq!(e.iso_text(), "ISO 160");
        assert_eq!(e.ae_mode_name(), "M (manual)");
        assert_eq!(Exposure { iso: -3200, ..e }.iso_text(), "auto (max 3200)");
    }
}
