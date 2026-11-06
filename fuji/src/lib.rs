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

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::ptr;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use ffi::*;

const DLL: &str = "XAPI.dll";

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
    pub fn detect(&self) -> Result<u32> {
        let mut n: Long = 0;
        let ret = unsafe { (self.f.detect)(XSDK_DSC_IF_USB, ptr::null_mut(), ptr::null_mut(), &mut n) };
        if ret != XSDK_COMPLETE {
            bail!("XSDK_Detect failed: {}", self.last_error(ptr::null_mut()));
        }
        Ok(n.max(0) as u32)
    }

    /// Open the first USB camera in tether mode and take control (PC priority).
    pub fn open_first(&self) -> Result<Camera<'_>> {
        if self.detect()? == 0 {
            bail!(
                "no Fujifilm camera found on USB. Is it on, connected, and set to \
                 CONNECTION MODE → USB TETHER SHOOTING AUTO?"
            );
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
                "camera is connected but not in tether mode (mode flags {mode:#x}). \
                 Set CONNECTION MODE → USB TETHER SHOOTING AUTO in the camera menu"
            );
        }
        cam.call("XSDK_SetPriorityMode", unsafe { (self.f.set_priority_mode)(h, XSDK_PRIORITY_PC) })?;
        Ok(cam)
    }

    fn last_error(&self, h: XSDK_HANDLE) -> String {
        let (mut api, mut code) = (0, 0);
        unsafe { (self.f.get_error_number)(h, &mut api, &mut code) };
        format!("{} ({code:#06x}, API {api:#06x})", errcode_name(code))
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
    const DENOMS: [f64; 40] = [
        32000.0, 16000.0, 8000.0, 6400.0, 5000.0, 4000.0, 3200.0, 2500.0, 2000.0, 1600.0, 1250.0, 1000.0, 800.0,
        640.0, 500.0, 400.0, 320.0, 250.0, 200.0, 160.0, 125.0, 100.0, 80.0, 60.0, 50.0, 40.0, 30.0, 25.0, 20.0,
        15.0, 13.0, 10.0, 8.0, 6.0, 5.0, 4.0, 3.0, 2.5, 2.0, 1.6,
    ];
    match us {
        XSDK_SHUTTER_BULB => "bulb".into(),
        XSDK_SHUTTER_UNKNOWN => "—".into(),
        v if v >= 1_000_000 => format!("{:.1}\"", v as f64 / 1e6),
        v => {
            let d = 1e6 / v as f64;
            let nearest = DENOMS
                .iter()
                .min_by(|a, b| (a.ln() - d.ln()).abs().total_cmp(&(b.ln() - d.ln()).abs()))
                .copied()
                .unwrap_or(d);
            format!("1/{nearest}")
        }
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
        let mode = if autofocus { XSDK_RELEASE_S2_S1OFF } else { XSDK_RELEASE_SHOOT_S1OFF };
        let (mut opt, mut af) = (0, XSDK_RELEASE_OK);
        let ret = unsafe { (self.sdk.f.release)(self.h, mode, &mut opt, &mut af) };
        if ret != XSDK_COMPLETE {
            // Make sure the shutter button isn't left half-pressed.
            let (mut o, mut a) = (0, 0);
            unsafe { (self.sdk.f.release)(self.h, XSDK_RELEASE_CANCEL, &mut o, &mut a) };
            self.call("XSDK_Release", ret)?;
        }
        Ok(Shot { focus_ok: af != XSDK_RELEASE_AF_FAILURE })
    }

    /// Fetch the next image from the transfer buffer, if any.
    pub fn next_image(&self) -> Result<Option<Image>> {
        let mut info: XSDK_ImageInformation = unsafe { std::mem::zeroed() };
        info.lFormat = XSDK_IMAGEFORMAT_NONE;
        let ret = unsafe { (self.sdk.f.read_image_info)(self.h, &mut info) };
        if ret != XSDK_COMPLETE {
            // "Nothing there" also comes back as an error on some bodies.
            return Ok(None);
        }
        let format = info.lFormat;
        let size = info.lDataSize;
        if !matches!(format & 0xFF, XSDK_IMAGEFORMAT_RAW | XSDK_IMAGEFORMAT_JPEG | XSDK_IMAGEFORMAT_HEIF) || size <= 0
        {
            return Ok(None);
        }
        let mut data = vec![0u8; size as usize];
        self.call("XSDK_ReadImage", unsafe { (self.sdk.f.read_image)(self.h, data.as_mut_ptr(), size as u32) })?;
        let (width, height) = (info.lImagePixWidth, info.lImagePixHeight);
        Ok(Some(Image { name: cstr(&info.strInternalName), format, width, height, data }))
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
        assert_eq!(shutter_text(1_000_000), "1.0\"");
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
