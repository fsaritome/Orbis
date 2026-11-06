//! Raw declarations from the Fujifilm X SDK 1.33 `XAPI.H` (the subset we use).
//!
//! Windows `long` is 32-bit, so every `long` here is `i32`. All structs are
//! `#pragma pack(1)`; C `bool` is one byte. Functions use the C calling
//! convention (`XSDK_APIENTRY` = `__declspec(dllimport) long`).

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::c_void;

pub type LIB_HANDLE = *mut c_void;
pub type XSDK_HANDLE = *mut c_void;
pub type Long = i32;

pub const XSDK_COMPLETE: Long = 0;
pub const XSDK_ERROR: Long = -1;

pub const XSDK_DSC_IF_USB: Long = 0x0000_0001;
pub const XSDK_DSC_IF_WIFI_LOCAL: Long = 0x0000_0010;

pub const XSDK_DSC_MODE_TETHER: Long = 0x0001;
pub const XSDK_DSC_MODE_RAW: Long = 0x0002;
pub const XSDK_DSC_MODE_BR: Long = 0x0004;
pub const XSDK_DSC_MODE_WEBCAM: Long = 0x0008;

pub const XSDK_PRIORITY_CAMERA: Long = 0x0001;
pub const XSDK_PRIORITY_PC: Long = 0x0002;

pub const XSDK_RELEASE_S1ON: Long = 0x0200;
pub const XSDK_RELEASE_N_S1OFF: Long = 0x0004;
pub const XSDK_RELEASE_CANCEL: Long = 0x000F;
/// Full press with autofocus, then release everything.
pub const XSDK_RELEASE_S2_S1OFF: Long = 0x0300 | 0x0004;
/// Shoot without autofocus (manual focus / focus already locked).
pub const XSDK_RELEASE_SHOOT_S1OFF: Long = 0x0100 | 0x0004;

pub const XSDK_RELEASE_OK: Long = 1;
pub const XSDK_RELEASE_AF_FAILURE: Long = 0;

pub const XSDK_IMAGEFORMAT_RAW: Long = 1;
pub const XSDK_IMAGEFORMAT_LIVE: Long = 4;
pub const XSDK_IMAGEFORMAT_NONE: Long = 5;
pub const XSDK_IMAGEFORMAT_JPEG: Long = 7;
pub const XSDK_IMAGEFORMAT_HEIF: Long = 0x0012;

pub const XSDK_MEDIAREC_RAWJPEG: Long = 0x0001;
pub const XSDK_MEDIAREC_RAW: Long = 0x0002;
pub const XSDK_MEDIAREC_JPEG: Long = 0x0003;
pub const XSDK_MEDIAREC_OFF: Long = 0x0004;

pub const XSDK_AE_OFF: Long = 0x0001;
pub const XSDK_AE_APERTURE_PRIORITY: Long = 0x0003;
pub const XSDK_AE_SHUTTER_PRIORITY: Long = 0x0004;
pub const XSDK_AE_PROGRAM: Long = 0x0006;

pub const XSDK_IRIS_NONE: Long = 0;
pub const XSDK_IRIS_AUTO: Long = 0xFFFF;
pub const XSDK_SHUTTER_UNKNOWN: Long = 0;
pub const XSDK_SHUTTER_BULB: Long = -1;

pub const XSDK_ERRCODE_NOERR: Long = 0x0000_0000;
pub const XSDK_ERRCODE_BUSY: Long = 0x0000_1006;
pub const XSDK_ERRCODE_FORCEMODE_BUSY: Long = 0x0000_1016;
pub const XSDK_ERRCODE_RUNNING_OTHER_FUNCTION: Long = 0x0000_1017;

/// Name of an `XSDK_ERRCODE_*` value.
pub fn errcode_name(code: Long) -> &'static str {
    match code {
        0x0000_0000 => "NOERR",
        0x0000_1001 => "SEQUENCE",
        0x0000_1002 => "PARAM",
        0x0000_1003 => "INVALID_CAMERA",
        0x0000_1004 => "LOADLIB",
        0x0000_1005 => "UNSUPPORTED",
        0x0000_1006 => "BUSY",
        0x0000_1007 => "AF_TIMEOUT",
        0x0000_1008 => "SHOOT_ERROR",
        0x0000_1009 => "FRAME_FULL",
        0x0000_1010 => "STANDBY",
        0x0000_1011 => "NODRIVER",
        0x0000_1012 => "NO_MODEL_MODULE",
        0x0000_1013 => "API_NOTFOUND",
        0x0000_1014 => "API_MISMATCH",
        0x0000_1015 => "INVALID_USBMODE",
        0x0000_1016 => "FORCEMODE_BUSY",
        0x0000_1017 => "RUNNING_OTHER_FUNCTION",
        0x0000_2001 => "COMMUNICATION",
        0x0000_2002 => "TIMEOUT",
        0x0000_2003 => "COMBINATION",
        0x0000_2004 => "WRITEERROR",
        0x0000_2005 => "CARDFULL",
        0x0000_3001 => "HARDWARE",
        0x0000_9001 => "INTERNAL",
        0x0000_9002 => "MEMFULL",
        0x0000_9100 => "UNKNOWN",
        _ => "?",
    }
}

#[repr(C, packed)]
pub struct XSDK_DeviceInformation {
    pub strVendor: [u8; 256],
    pub strManufacturer: [u8; 256],
    pub strProduct: [u8; 256],
    pub strFirmware: [u8; 256],
    pub strDeviceType: [u8; 256],
    pub strSerialNo: [u8; 256],
    pub strFramework: [u8; 256],
    pub bDeviceId: u8,
    pub strDeviceName: [u8; 32],
    pub strYNo: [u8; 32],
}

#[repr(C, packed)]
pub struct XSDK_ImageInformation {
    pub strInternalName: [u8; 32],
    pub lFormat: Long,
    pub lDataSize: Long,
    pub lImagePixHeight: Long,
    pub lImagePixWidth: Long,
    pub lImageBitDepth: Long,
    pub lPreviewSize: Long,
    pub hImage: XSDK_HANDLE,
}

#[repr(C, packed)]
pub struct XSDK_LensInformation {
    pub strModel: [u8; 20],
    pub strProductName: [u8; 100],
    pub strSerialNo: [u8; 20],
    pub lISCapability: Long,
    pub lMFCapability: Long,
    pub lZoomPosCapability: Long,
}

// Compile-time layout checks against the C header.
const _: () = assert!(std::mem::size_of::<XSDK_DeviceInformation>() == 256 * 7 + 1 + 32 + 32);
const _: () = assert!(std::mem::size_of::<XSDK_ImageInformation>() == 32 + 6 * 4 + std::mem::size_of::<usize>());
const _: () = assert!(std::mem::size_of::<XSDK_LensInformation>() == 20 + 100 + 20 + 3 * 4);

pub type FnInit = unsafe extern "C" fn(LIB_HANDLE) -> Long;
pub type FnExit = unsafe extern "C" fn() -> Long;
pub type FnDetect = unsafe extern "C" fn(Long, *mut i8, *mut i8, *mut Long) -> Long;
pub type FnOpenEx = unsafe extern "C" fn(*const i8, *mut XSDK_HANDLE, *mut Long, *mut c_void) -> Long;
pub type FnClose = unsafe extern "C" fn(XSDK_HANDLE) -> Long;
pub type FnGetErrorNumber = unsafe extern "C" fn(XSDK_HANDLE, *mut Long, *mut Long) -> Long;
pub type FnGetVersionString = unsafe extern "C" fn(*mut i8) -> Long;
pub type FnGetDeviceInfo = unsafe extern "C" fn(XSDK_HANDLE, *mut XSDK_DeviceInformation) -> Long;
pub type FnGetLensInfo = unsafe extern "C" fn(XSDK_HANDLE, *mut XSDK_LensInformation) -> Long;
pub type FnSetLong = unsafe extern "C" fn(XSDK_HANDLE, Long) -> Long;
pub type FnGetLong = unsafe extern "C" fn(XSDK_HANDLE, *mut Long) -> Long;
pub type FnRelease = unsafe extern "C" fn(XSDK_HANDLE, Long, *mut Long, *mut Long) -> Long;
pub type FnGetBufferCapacity = unsafe extern "C" fn(XSDK_HANDLE, *mut Long, *mut Long) -> Long;
pub type FnReadImageInfo = unsafe extern "C" fn(XSDK_HANDLE, *mut XSDK_ImageInformation) -> Long;
pub type FnReadImage = unsafe extern "C" fn(XSDK_HANDLE, *mut u8, u32) -> Long;
pub type FnDeleteImage = unsafe extern "C" fn(XSDK_HANDLE) -> Long;
pub type FnGetShutterSpeed = unsafe extern "C" fn(XSDK_HANDLE, *mut Long, *mut Long) -> Long;
pub type FnSetShutterSpeed = unsafe extern "C" fn(XSDK_HANDLE, Long, Long) -> Long;
/// `XSDK_Cap*`-style list query: (handle, count out, values out or null).
pub type FnCapList = unsafe extern "C" fn(XSDK_HANDLE, *mut Long, *mut Long) -> Long;
/// `XSDK_CapShutterSpeed`: (handle, count, values, bulb capable).
pub type FnCapShutter = unsafe extern "C" fn(XSDK_HANDLE, *mut Long, *mut Long, *mut Long) -> Long;
/// `XSDK_CapAperture`: (handle, zoom position, count, values).
pub type FnCapAperture = unsafe extern "C" fn(XSDK_HANDLE, Long, *mut Long, *mut Long) -> Long;

// The *Prop functions are C-variadic. On x64 Windows, variadic integer and
// pointer arguments are passed exactly like fixed ones, so each use gets a
// fixed-arity signature. `lAPIParam` is the number of following arguments.
pub type FnSetProp0 = unsafe extern "C" fn(XSDK_HANDLE, Long, Long) -> Long;
pub type FnSetProp1 = unsafe extern "C" fn(XSDK_HANDLE, Long, Long, Long) -> Long;
pub type FnGetProp1 = unsafe extern "C" fn(XSDK_HANDLE, Long, Long, *mut Long) -> Long;
pub type FnCapProp2 = unsafe extern "C" fn(XSDK_HANDLE, Long, Long, *mut Long, *mut Long) -> Long;

// Model-dependent property codes (XAPIOpt.H), valid for the X-T4.
pub const API_CODE_SET_FILM_SIMULATION: Long = 0x2121;
pub const API_CODE_GET_FILM_SIMULATION: Long = 0x2122;
pub const API_CODE_SET_IMAGE_QUALITY: Long = 0x2129;
pub const API_CODE_GET_IMAGE_QUALITY: Long = 0x2130;
pub const API_CODE_SET_FOCUS_MODE: Long = 0x2201;
pub const API_CODE_GET_FOCUS_MODE: Long = 0x2202;
pub const API_CODE_CAP_FOCUS_MODE: Long = 0x2209;

pub const SDK_FOCUS_MANUAL: Long = 0x0001;
pub const SDK_FOCUS_AFS: Long = 0x8001;
pub const SDK_FOCUS_AFC: Long = 0x8002;

pub const SDK_IMAGEQUALITY_RAW: Long = 1;
pub const SDK_IMAGEQUALITY_FINE: Long = 2;
pub const SDK_IMAGEQUALITY_NORMAL: Long = 3;
pub const SDK_IMAGEQUALITY_RAW_FINE: Long = 4;
pub const SDK_IMAGEQUALITY_RAW_NORMAL: Long = 5;
pub const SDK_IMAGEQUALITY_SUPERFINE: Long = 6;
pub const SDK_IMAGEQUALITY_RAW_SUPERFINE: Long = 7;

pub const SDK_FILMSIMULATION_PROVIA: Long = 1;

pub const API_CODE_START_LIVE_VIEW: Long = 0x3301;
pub const API_CODE_STOP_LIVE_VIEW: Long = 0x3302;
pub const API_CODE_SET_LIVE_VIEW_QUALITY: Long = 0x3323;
pub const API_CODE_SET_LIVE_VIEW_SIZE: Long = 0x3325;
pub const API_CODE_SET_LIVE_VIEW_MODE: Long = 0x332F;
pub const SDK_LIVEVIEW_QUALITY_FINE: Long = 1;
pub const SDK_LIVEVIEW_SIZE_L: Long = 1; // 1280 px
pub const SDK_LIVEVIEW_MODE1: Long = 1;

/// Focus point position on the camera's focus grid (`SDK_FocusArea`).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FocusArea {
    pub h: Long,
    pub v: Long,
    pub size: Long,
}

pub const API_CODE_SET_FOCUS_AREA: Long = 0x2205;
pub const API_CODE_GET_FOCUS_AREA: Long = 0x2206;
pub const API_CODE_CAP_FOCUS_AREA: Long = 0x2235;
pub const API_CODE_GET_FOCUS_POINTS: Long = 0x2258;
pub const SDK_ITEM_DIRECTION_0: Long = 1;
/// SetProp/GetProp(code, 2, angle, &FocusArea).
pub type FnFocusArea = unsafe extern "C" fn(XSDK_HANDLE, Long, Long, Long, *mut FocusArea) -> Long;
/// CapProp(code, 3, angle, &min, &max).
pub type FnCapFocusArea = unsafe extern "C" fn(XSDK_HANDLE, Long, Long, Long, *mut FocusArea, *mut FocusArea) -> Long;

/// One-shot autofocus while in MF (the AF-ON button).
pub const XSDK_RELEASE_INSTANTAF: Long = 0xA000;
pub const XSDK_RELEASE_N_INSTANTAF: Long = 0x0010;
pub const SDK_WB_DAYLIGHT: Long = 0x0004;

/// XSDK_GetErrorDetails bits (why a release was refused).
pub const ERROR_DETAILS: [(Long, &str); 10] = [
    (0x0001, "shutter button half-pressed (S1)"),
    (0x0002, "AE lock active"),
    (0x0004, "AF lock active"),
    (0x0008, "instant AF running"),
    (0x0010, "AF-ON pressed"),
    (0x0020, "still shooting"),
    (0x0040, "self-timer countdown"),
    (0x0080, "recording video"),
    (0x0100, "live view running"),
    (0x0200, "previous photo not transferred yet"),
];

/// Custom length of the "2 MIN" auto power-off (X-T4 and others).
pub const API_CODE_SET_CUSTOM_AUTO_POWER_OFF: Long = 0x4229;
pub const API_CODE_GET_CUSTOM_AUTO_POWER_OFF: Long = 0x4230;
/// 1–254 = 10–263 s; 255 = never power off.
pub const AUTO_POWER_OFF_NEVER: Long = 255;
/// 120 s (value = seconds - 9): the stock "2 MIN".
pub const AUTO_POWER_OFF_2_MIN: Long = 111;
