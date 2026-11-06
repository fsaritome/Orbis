//! Camera settings the app can read and change, with readable names.

use crate::ffi::*;

/// One adjustable camera setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Setting {
    Mode,
    Aperture,
    Shutter,
    Iso,
    WhiteBalance,
    Focus,
    Quality,
    FilmSimulation,
}

impl Setting {
    pub const ALL: [Setting; 8] = [
        Setting::Mode,
        Setting::Aperture,
        Setting::Shutter,
        Setting::Iso,
        Setting::WhiteBalance,
        Setting::Focus,
        Setting::Quality,
        Setting::FilmSimulation,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Setting::Mode => "Mode",
            Setting::Aperture => "Aperture",
            Setting::Shutter => "Shutter",
            Setting::Iso => "ISO",
            Setting::WhiteBalance => "White balance",
            Setting::Focus => "Focus",
            Setting::Quality => "Image quality",
            Setting::FilmSimulation => "Film simulation",
        }
    }

    /// Display text for an SDK value of this setting.
    pub fn value_name(self, v: i32) -> String {
        match self {
            Setting::Mode => match v {
                XSDK_AE_OFF => "M (manual)".into(),
                XSDK_AE_APERTURE_PRIORITY => "A (aperture priority)".into(),
                XSDK_AE_SHUTTER_PRIORITY => "S (shutter priority)".into(),
                XSDK_AE_PROGRAM => "P (program)".into(),
                v => format!("mode {v:#x}"),
            },
            Setting::Aperture => match v {
                XSDK_IRIS_AUTO => "auto".into(),
                XSDK_IRIS_NONE => "—".into(),
                v => format!("f/{}", v as f64 / 100.0),
            },
            Setting::Shutter => crate::shutter_text(v),
            Setting::Iso => match v {
                v if v > 0 => format!("ISO {v}"),
                -10 => "auto".into(),
                v if v < -10 => format!("auto (max {})", -v),
                v => format!("auto {}", -v),
            },
            Setting::WhiteBalance => wb_name(v),
            Setting::Focus => match v {
                SDK_FOCUS_MANUAL => "MF (manual)".into(),
                SDK_FOCUS_AFS => "AF-S (single)".into(),
                SDK_FOCUS_AFC => "AF-C (continuous)".into(),
                v => format!("{v:#x}"),
            },
            Setting::Quality => match v {
                SDK_IMAGEQUALITY_RAW => "RAW".into(),
                SDK_IMAGEQUALITY_FINE => "JPEG Fine".into(),
                SDK_IMAGEQUALITY_NORMAL => "JPEG Normal".into(),
                SDK_IMAGEQUALITY_RAW_FINE => "RAW + JPEG Fine".into(),
                SDK_IMAGEQUALITY_RAW_NORMAL => "RAW + JPEG Normal".into(),
                SDK_IMAGEQUALITY_SUPERFINE => "JPEG Super Fine".into(),
                SDK_IMAGEQUALITY_RAW_SUPERFINE => "RAW + JPEG Super Fine".into(),
                v => format!("{v}"),
            },
            Setting::FilmSimulation => film_name(v).into(),
        }
    }
}

fn wb_name(v: i32) -> String {
    match v {
        0x0002 => "Auto".into(),
        0x8020 => "Auto (white priority)".into(),
        0x8021 => "Auto (ambience priority)".into(),
        0x0004 => "Daylight".into(),
        0x0006 => "Incandescent".into(),
        0x0008 => "Underwater".into(),
        0x8001 => "Fluorescent 1".into(),
        0x8002 => "Fluorescent 2".into(),
        0x8003 => "Fluorescent 3".into(),
        0x8006 => "Shade".into(),
        0x8007 => "Color temperature".into(),
        0x8008..=0x800C => format!("Custom {}", v - 0x8007),
        v if (2000..=12000).contains(&v) => format!("{v} K"),
        v => format!("{v:#x}"),
    }
}

/// X-T4 film simulations (no capability query exists for these).
pub const FILM_SIMULATIONS: [i32; 19] = [1, 2, 3, 11, 4, 5, 17, 18, 16, 6, 7, 8, 9, 12, 13, 14, 15, 10, 0x8000];

fn film_name(v: i32) -> &'static str {
    match v {
        1 => "Provia / Standard",
        2 => "Velvia / Vivid",
        3 => "Astia / Soft",
        4 => "Pro Neg. Hi",
        5 => "Pro Neg. Std",
        6 => "Monochrome",
        7 => "Monochrome + Ye",
        8 => "Monochrome + R",
        9 => "Monochrome + G",
        10 => "Sepia",
        11 => "Classic Chrome",
        12 => "Acros",
        13 => "Acros + Ye",
        14 => "Acros + R",
        15 => "Acros + G",
        16 => "Eterna",
        17 => "Classic Neg.",
        18 => "Eterna Bleach Bypass",
        0x8000 => "Auto",
        _ => "?",
    }
}

/// X-T4 image quality choices.
pub const QUALITIES: [i32; 7] = [
    SDK_IMAGEQUALITY_RAW_FINE,
    SDK_IMAGEQUALITY_RAW_SUPERFINE,
    SDK_IMAGEQUALITY_RAW_NORMAL,
    SDK_IMAGEQUALITY_FINE,
    SDK_IMAGEQUALITY_SUPERFINE,
    SDK_IMAGEQUALITY_NORMAL,
    SDK_IMAGEQUALITY_RAW,
];

/// Current value of every setting (`None` = couldn't be read).
#[derive(Debug, Clone, Default)]
pub struct Values(pub Vec<(Setting, Option<i32>)>);

impl Values {
    pub fn get(&self, s: Setting) -> Option<i32> {
        self.0.iter().find(|(k, _)| *k == s).and_then(|(_, v)| *v)
    }
}

/// Values each setting accepts right now (depends on mode and lens).
#[derive(Debug, Clone, Default)]
pub struct Choices(pub Vec<(Setting, Vec<i32>)>);

impl Choices {
    pub fn get(&self, s: Setting) -> &[i32] {
        self.0.iter().find(|(k, _)| *k == s).map(|(_, v)| v.as_slice()).unwrap_or(&[])
    }
}

/// Recommended settings for photogrammetry: identical exposure, colour
/// and focus behaviour for every photo, maximum detail.
pub fn photogrammetry_preset() -> Vec<(Setting, i32)> {
    vec![
        (Setting::Mode, XSDK_AE_OFF),
        (Setting::Iso, 160),
        (Setting::WhiteBalance, SDK_WB_DAYLIGHT),
        (Setting::FilmSimulation, SDK_FILMSIMULATION_PROVIA),
        (Setting::Quality, SDK_IMAGEQUALITY_RAW_FINE),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(Setting::Focus.value_name(SDK_FOCUS_MANUAL), "MF (manual)");
        assert_eq!(Setting::Quality.value_name(4), "RAW + JPEG Fine");
        assert_eq!(Setting::FilmSimulation.value_name(12), "Acros");
        assert_eq!(Setting::WhiteBalance.value_name(5600), "5600 K");
        assert_eq!(Setting::Aperture.value_name(560), "f/5.6");
        assert_eq!(Setting::Iso.value_name(-3200), "auto (max 3200)");
        for f in FILM_SIMULATIONS {
            assert_ne!(film_name(f), "?");
        }
    }
}
