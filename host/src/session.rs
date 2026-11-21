//! One running game: boots the library the way the Android runtime and the Java layer
//! did, then exposes the three things a frontend needs — render a frame, deliver an input
//! event, and see what the game asked for (timer, audio, exit).

// Some inputs are only produced by the windowed frontend.
#![cfg_attr(not(feature = "window"), allow(dead_code))]

use crate::java::{self, AudioCmd};
use crate::jni::Obj;
use crate::machine::Machine;
use crate::mem::{Fault, R};
use crate::vfs;
use std::cell::RefCell;
use std::rc::Rc;

const SURFACE_VIEW: &str = "Java_com_bicore_graphic_BitmapSurfaceView_";
const NATIVE: &str = "Java_com_bicore_NativeFuntion_";

// Event types and key codes, from `handleCletEvent` in the library and the Java layer.
const EV_KEY_DOWN: i32 = 2;
const EV_KEY_UP: i32 = 3;
pub const KEY_UP: i32 = -1;
pub const KEY_DOWN: i32 = -2;
pub const KEY_LEFT: i32 = -3;
pub const KEY_RIGHT: i32 = -4;
pub const KEY_OK: i32 = -5;
pub const KEY_BACK: i32 = -22;

/// Input events understood by the library's `nativeHandleEvent(type, a, b, c, d)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// Finger down at (x, y), in game-surface pixels.
    TouchDown(i32, i32),
    /// Finger moved to (x, y) from (last x, last y).
    TouchMove(i32, i32, i32, i32),
    /// Finger lifted at (x, y); the last two values are where it was last seen.
    TouchUp(i32, i32, i32, i32),
    /// The Android back key.
    Back,
    /// A key went down or up. Codes are the feature-phone ones the engine was written
    /// for: see the `KEY_*` constants.
    KeyDown(i32),
    KeyUp(i32),
    /// A raw event, for experiments: (type, a, b).
    Raw(i32, i32, i32),
}

pub struct Session {
    pub m: Machine,
    class: u32,
    view: u32,
    frame: Rc<RefCell<Vec<u8>>>,
    frame_ref: u32,
    pub width: u32,
    pub height: u32,
}

impl Session {
    pub fn boot(mut m: Machine, screen: (u32, u32)) -> R<Session> {
        // System.loadLibrary
        let vm = m.jni.vm;
        let on_load = m.export("JNI_OnLoad")?;
        m.call(on_load, &[vm, 0])?;

        // These references stay alive for the whole session.
        m.jni.push_frame();
        let class = m.jni.new_ref(Obj::Class(java::NATIVE.into()))?;
        let view = m.jni.new_ref(Obj::Instance { class: "com/bicore/graphic/BitmapSurfaceView", id: 0 })?;
        let apk = m.jni.new_ref(Obj::Str(vfs::APK_PATH.into()))?;
        let doc = m.jni.new_ref(Obj::Str(vfs::DOC_PATH.into()))?;

        // BicoreActivity.onCreate / onGameInit, then the BitmapSurfaceView constructor.
        let (sw, sh) = screen;
        m.call_native(&format!("{NATIVE}nativeSetDeviceResolution"), class, &[sw, sh])?;
        m.call_native(&format!("{NATIVE}nativeSetResourceDirectory"), class, &[apk])?;
        m.call_native(&format!("{NATIVE}nativeSetDocumentDirectory"), class, &[doc])?;
        m.call_native(&format!("{NATIVE}nativeSetDeviceResolution"), class, &[sw, sh])?;
        m.call_native(&format!("{SURFACE_VIEW}FirstInit"), view, &[])?;
        let width = m.call_native(&format!("{SURFACE_VIEW}GetGameWidth"), view, &[])?;
        let height = m.call_native(&format!("{SURFACE_VIEW}GetGameHeight"), view, &[])?;
        if !(1..=4096).contains(&width) || !(1..=4096).contains(&height) {
            return Err(Fault::Other(format!("implausible surface size {width}x{height}")));
        }
        let frame = Rc::new(RefCell::new(vec![0u8; (width * height * 2) as usize]));
        let frame_ref = m.jni.new_ref(Obj::Bytes(frame.clone()))?;
        Ok(Session { m, class, view, frame, frame_ref, width, height })
    }

    /// Run one game frame (`BitmapSurfaceView.onDraw`). `elapsed_ms` is time since boot.
    pub fn render(&mut self, elapsed_ms: u64) -> R<()> {
        // FillBuffer(byte[] buf, int w, int h, int stride, long elapsedMs)
        let args = [self.frame_ref, self.width, self.height, self.width * 2, elapsed_ms as u32, (elapsed_ms >> 32) as u32];
        self.m.call_native(&format!("{SURFACE_VIEW}FillBuffer"), self.view, &args)?;
        Ok(())
    }

    /// The last rendered frame: `width * height` little-endian RGB565 pixels.
    pub fn pixels(&self) -> std::cell::Ref<'_, Vec<u8>> {
        self.frame.borrow()
    }

    pub fn input(&mut self, ev: Input) -> R<()> {
        let v: [i32; 5] = match ev {
            Input::TouchDown(x, y) => [23, x, y, x, y],
            Input::TouchUp(x, y, lx, ly) => [24, x, y, lx, ly],
            Input::TouchMove(x, y, lx, ly) => [26, x, y, lx, ly],
            Input::Back => [EV_KEY_DOWN, KEY_BACK, 0, 0, 0],
            Input::KeyDown(k) => [EV_KEY_DOWN, k, 0, 0, 0],
            Input::KeyUp(k) => [EV_KEY_UP, k, 0, 0, 0],
            Input::Raw(t, a, b) => [t, a, b, 0, 0],
        };
        self.m.call_native(&format!("{NATIVE}nativeHandleEvent"), self.class, &v.map(|x| x as u32))?;
        Ok(())
    }

    /// The game's current language: 0 for Korean, otherwise an ASCII letter
    /// (`E`, `J`, `C`, `S`, `F`, `G`), as in the asset file-name suffixes.
    pub fn language(&mut self) -> R<u8> {
        let f = self.m.export("IGM_GetLanguageCode")?;
        Ok(self.m.call(f, &[])? as u8)
    }

    /// Change the language and write it to the game's config file, using the same two
    /// functions as the in-game language picker. The picker then restarts the game; the
    /// caller does that by booting a fresh session.
    pub fn save_language(&mut self, code: u8) -> R<()> {
        let set = self.m.export("IGM_SetLanguageCode")?;
        let save = self.m.export("IGOpt_SaveConfig")?;
        self.m.call(set, &[code as u32])?;
        self.m.call(save, &[])?;
        Ok(())
    }

    /// Delay in milliseconds the game asked for before its next frame, if any.
    pub fn take_timer(&mut self) -> Option<u32> {
        self.m.java.timer_ms.take()
    }

    pub fn take_audio(&mut self) -> Vec<AudioCmd> {
        std::mem::take(&mut self.m.java.audio)
    }

    pub fn exit_requested(&self) -> bool {
        self.m.java.exit_requested
    }
}
