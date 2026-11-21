//! The host's stand-in for the game's Java layer (`com.bicore.*`).
//!
//! The native library calls back into Java for everything it cannot do itself: device
//! information, audio, text rendering, timers. This module answers those calls. Anything
//! that would have reached the outside world on Android (opening URLs, payments, device
//! identifiers, network state) is answered with an inert value and logged.

use crate::font::Text;
use crate::jni::{JVal, Obj};
use crate::machine::Machine;
use crate::mem::R;
use std::cell::RefCell;
use std::rc::Rc;

pub const NATIVE: &str = "com/bicore/NativeFuntion";

#[derive(Default)]
pub struct Java {
    pub language: String,
    pub country: String,
    /// Delay requested by the guest's last `SetTimer`, consumed by the frame loop.
    pub timer_ms: Option<u32>,
    pub exit_requested: bool,
    pub verbose: bool,
    /// Things the guest asked the outside world to do, for the log.
    pub events: Vec<String>,
    pub text: Option<Text>,
    /// Pixel size of each `com.bicore.Font` the guest has created.
    pub fonts: Vec<f32>,
    pub glyphs_drawn: u64,
    /// Sound effects (`W/*.ogg`) and music tracks (`B/*.mp3`) the guest has loaded, by id - 1.
    pub sfx: Vec<String>,
    pub music: Vec<String>,
    /// Audio requests since the frontend last drained them.
    pub audio: Vec<AudioCmd>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioCmd {
    PlaySfx(String),
    PlayMusic { name: String, looped: bool },
    StopMusic(String),
    SfxVolume(f32),
    MusicVolume(f32),
}

impl Java {
    /// Record something the guest asked of the outside world (bounded).
    fn note(&mut self, what: String) {
        if self.events.len() < 256 {
            self.events.push(what.chars().take(300).collect());
        }
    }
}

const FONT: &str = "com/bicore/Font";

/// The Java charset names the game uses for string conversion.
fn charset(name: &str) -> &'static encoding_rs::Encoding {
    match name.to_ascii_uppercase().replace('-', "_").as_str() {
        "EUC_KR" | "KSC5601" | "KS_C_5601_1987" => encoding_rs::EUC_KR,
        _ => encoding_rs::UTF_8,
    }
}

fn font_size(m: &Machine, this: u32) -> R<f32> {
    match m.jni.get(this)? {
        Obj::Instance { class: FONT, id } => Ok(m.java.fonts.get(*id as usize).copied().unwrap_or(12.0)),
        o => Err(crate::mem::Fault::Other(format!("expected a Font, got {}", o.class_name()))),
    }
}

fn new_str(m: &mut Machine, s: &str) -> R<JVal> {
    Ok(JVal::Ref(m.jni.new_ref(Obj::Str(s.to_string()))?))
}

pub fn call(m: &mut Machine, class: &str, name: &str, sig: &str, this: u32, args: &[JVal]) -> R<JVal> {
    let arg_str = |m: &Machine, i: usize| m.jni.string(args.get(i).map_or(0, |a| a.int() as u32));
    if m.java.verbose {
        eprintln!("[java] {class}.{name}{sig} {args:?}");
    }
    Ok(match (class, name) {
        // java.lang.String <-> byte[] conversions, in the charset the guest names.
        ("java/lang/String", "getBytes") => {
            let s = m.jni.string(this)?;
            let cs = if args.is_empty() { encoding_rs::UTF_8 } else { charset(&arg_str(m, 0)?) };
            let bytes = cs.encode(&s).0.into_owned();
            JVal::Ref(m.jni.new_ref(Obj::Bytes(Rc::new(RefCell::new(bytes))))?)
        }
        ("java/lang/String", "<init>") => {
            let bytes = m.jni.bytes(args.first().map_or(0, |a| a.int() as u32))?;
            let cs = if args.len() < 2 { encoding_rs::UTF_8 } else { charset(&arg_str(m, 1)?) };
            let s = cs.decode_without_bom_handling(&bytes.borrow()).0.into_owned();
            new_str(m, &s)?
        }

        // new Font(size): report the font's metrics to the native side, as the constructor does.
        (FONT, "<init>") => {
            let px = (args.first().map_or(12, |a| a.int()).clamp(4, 200)) as f32;
            if m.java.fonts.len() >= 1024 {
                return Ok(JVal::Ref(0));
            }
            m.java.fonts.push(px);
            let font = m.jni.new_ref(Obj::Instance { class: FONT, id: m.java.fonts.len() as u32 - 1 })?;
            let (a, d, gap) = m.java.text.as_ref().map_or((px, 0.0, 0.0), |t| t.line_metrics(px));
            let v = [a.ceil() as u32, d.ceil() as u32, (a + d).ceil() as u32, gap.ceil() as u32];
            m.call_native("Java_com_bicore_Font_setFontData", font, &v)?;
            JVal::Ref(font)
        }
        (FONT, "getStringData") => {
            let px = font_size(m, this)?;
            let s = arg_str(m, 0)?;
            let Some(d) = m.java.text.as_ref().and_then(|t| t.string_data(&s, px)) else { return Ok(JVal::Int(0)) };
            m.java.glyphs_drawn += 1;
            let arr = m.jni.new_ref(Obj::Bytes(Rc::new(RefCell::new(d.alpha))))?;
            let v = [d.measured as u32, d.bounds_height as u32, d.advance as u32, d.width, d.height, arr];
            JVal::Int(m.call_native("Java_com_bicore_Font_setStringData", this, &v)? as i32)
        }
        (NATIVE, "GetStringWidth") => {
            let px = font_size(m, args.first().map_or(0, |a| a.int() as u32))?;
            let s = arg_str(m, 1)?;
            JVal::Int(m.java.text.as_ref().and_then(|t| t.string_data(&s, px)).map_or(0, |d| d.measured))
        }
        // Audio. The guest names an asset; the host never gives it a file handle.
        (NATIVE, "CreateWAVAudioClip" | "CreateMP3AudioClip") => {
            let clip = arg_str(m, 0)?;
            let list = if name == "CreateWAVAudioClip" { &mut m.java.sfx } else { &mut m.java.music };
            let id = match list.iter().position(|n| *n == clip) {
                Some(i) => i,
                None if list.len() < 4096 => {
                    list.push(clip);
                    list.len() - 1
                }
                None => return Ok(JVal::Int(0)),
            };
            JVal::Int(id as i32 + 1)
        }
        (NATIVE, "PlayWAVAudio" | "PlayMP3Audio" | "StopMP3Audio") => {
            let id = args.first().map_or(0, |a| a.int());
            let list = if name == "PlayWAVAudio" { &m.java.sfx } else { &m.java.music };
            if let Some(clip) = (id as usize).checked_sub(1).and_then(|i| list.get(i)).cloned() {
                let cmd = match name {
                    "PlayWAVAudio" => AudioCmd::PlaySfx(clip),
                    "PlayMP3Audio" => AudioCmd::PlayMusic { name: clip, looped: args.get(1).is_some_and(|a| a.int() != 0) },
                    _ => AudioCmd::StopMusic(clip),
                };
                if m.java.audio.len() < 1024 {
                    m.java.audio.push(cmd);
                }
            }
            if name == "StopMP3Audio" { JVal::Void } else { JVal::Int(id) }
        }
        (NATIVE, "SetWAVVolume" | "SetMP3Volume") => {
            let v = (args.get(1).map_or(100, |a| a.int()) as f32 / 100.0).clamp(0.0, 1.0);
            if m.java.audio.len() < 1024 {
                m.java.audio.push(if name == "SetWAVVolume" { AudioCmd::SfxVolume(v) } else { AudioCmd::MusicVolume(v) });
            }
            JVal::Void
        }
        (NATIVE, "StopWAVAudio" | "UnloadWAVAudioClip" | "UnloadMP3AudioClip") => JVal::Void,
        (NATIVE, "GetMP3AudioClipCurrentPosition") => JVal::Int(0),

        // The on-screen text field was only used for online login.
        (NATIVE, "GetTextViewControl") => JVal::Ref(m.jni.new_ref(Obj::Instance { class: "android/widget/TextView", id: 0 })?),
        (NATIVE, "GetEditBoxControl") => JVal::Ref(m.jni.new_ref(Obj::Instance { class: "android/widget/EditText", id: 0 })?),

        (NATIVE, "SetTimer") => {
            m.java.timer_ms = Some(args.first().map_or(0, |a| a.int()).max(0) as u32);
            JVal::Void
        }
        (NATIVE, "OnMessage") => {
            let s = arg_str(m, 0)?;
            eprintln!("[guest message] {s}");
            JVal::Void
        }
        (NATIVE, "OnExitApplication") => {
            m.java.exit_requested = true;
            JVal::Void
        }
        (NATIVE, "OnURLOpen") => {
            let s = arg_str(m, 0)?;
            m.java.note(format!("open URL (ignored): {s}"));
            JVal::Void
        }
        (NATIVE, "OnRequestPayment") => {
            let s = arg_str(m, 0)?;
            m.java.note(format!("payment request (ignored): {s}"));
            JVal::Void
        }
        // Identity: fixed, fake values. Nothing about the host machine is exposed.
        (NATIVE, "GetDeviceID") => new_str(m, "866150004500431")?,
        (NATIVE, "GetPhoneNumber") => new_str(m, "")?,
        (NATIVE, "GetModelType") => new_str(m, "dhost")?,
        (NATIVE, "GetCountryCode") => {
            let s = m.java.country.clone();
            new_str(m, &s)?
        }
        (NATIVE, "GetLanguageCode") => {
            let s = m.java.language.clone();
            new_str(m, &s)?
        }
        // Network state: always offline. Airplane mode on, no Wi-Fi, no data.
        (NATIVE, "GetAirplaneModeState") => JVal::Int(1),
        (NATIVE, "IsEnableDataConnect" | "IsEnableWifi" | "GetCheckLooting" | "isVaild") => JVal::Int(0),
        (NATIVE, "CallVibration" | "HideAdmob" | "HideBanner" | "ShowBanner") => JVal::Void,

        _ => {
            m.warn_once(format!("unhandled Java call {class}.{name}{sig}"));
            match sig.rsplit(')').next().and_then(|r| r.bytes().next()) {
                Some(b'V') => JVal::Void,
                Some(b'L' | b'[') => JVal::Ref(0),
                Some(b'J') => JVal::Long(0),
                Some(b'F') => JVal::Float(0.0),
                Some(b'D') => JVal::Double(0.0),
                _ => JVal::Int(0),
            }
        }
    })
}
