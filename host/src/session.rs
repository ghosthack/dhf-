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
// What the on-screen buttons do, found by comparing each button with each key code in a
// dungeon: A is OK (attack, talk), B is keypad 2 (jump), C is keypad 9, the two quick
// slots are keypad 0 and #. The soft keys open the map and the menu.
pub const KEY_JUMP: i32 = b'2' as i32;
pub const KEY_BUTTON_C: i32 = b'9' as i32;
pub const KEY_SLOT_1: i32 = b'0' as i32;
pub const KEY_SLOT_2: i32 = b'#' as i32;
pub const KEY_MAP: i32 = -6;
pub const KEY_MENU: i32 = -16;
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
    /// Cubics still to be handed to the player once a game is running.
    pending_cubics: Option<u32>,
    /// How often the game has loaded a map, and how often it has read a save file.
    map_loads: Option<Rc<std::cell::Cell<u32>>>,
    save_loads: Option<Rc<std::cell::Cell<u32>>>,
    /// Frames rendered since the game last loaded a map.
    frames_in_map: Option<u32>,
}

/// Item id of the Cubic, the game's premium currency (it is an inventory item).
const ITEM_CUBIC: u32 = 5632;
/// Item counts are signed 16-bit in the game.
const MAX_ITEM_COUNT: u32 = 32767;

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
        Ok(Session { m, class, view, frame, frame_ref, width, height, pending_cubics: None, map_loads: None, save_loads: None, frames_in_map: None })
    }

    /// Run one game frame (`BitmapSurfaceView.onDraw`). `elapsed_ms` is time since boot.
    pub fn render(&mut self, elapsed_ms: u64) -> R<()> {
        // FillBuffer(byte[] buf, int w, int h, int stride, long elapsedMs)
        let args = [self.frame_ref, self.width, self.height, self.width * 2, elapsed_ms as u32, (elapsed_ms >> 32) as u32];
        self.m.call_native(&format!("{SURFACE_VIEW}FillBuffer"), self.view, &args)?;
        if self.pending_cubics.is_some() {
            self.grant_pending_cubics()?;
        }
        Ok(())
    }

    /// Give `n` Cubics to the player once a game (loaded or new) is running.
    pub fn give_cubics_in_game(&mut self, n: u32) -> R<()> {
        let (map, save) = (self.m.export("IMAP_Load")?, self.m.export("IEU_SaveLoad_Load")?);
        self.map_loads = Some(self.m.watch(map)?);
        self.save_loads = Some(self.m.watch(save)?);
        self.pending_cubics = Some(n);
        Ok(())
    }

    /// The player data of the running game, or `None` on the title screen and menus.
    fn player(&mut self) -> R<Option<u32>> {
        // IGM_GetPlayerGObject(i) is `IA_Get(state->objects, i)` and does not check that
        // the list exists, so look first. The state pointer is read the way the function
        // itself reads it: two literals after its code locate a slot in the GOT.
        let f = self.m.export("IGM_GetPlayerGObject")?;
        let slot = (f + 0x14).wrapping_add(self.m.mem.r32(f + 0x1c)?).wrapping_add(self.m.mem.r32(f + 0x20)?);
        let state = self.m.mem.r32(slot)?;
        let list = self.m.mem.r32(state.wrapping_add(0x8c))?;
        if list == 0 || self.m.mem.r32(list.wrapping_add(8))? == 0 {
            return Ok(None);
        }
        let object = self.m.call(f, &[0])?;
        if object == 0 {
            return Ok(None);
        }
        // The player's data hangs off the game object (as in IEU_SaveLoad_CubicSave). It
        // only exists once a game is loaded; check its item lists before trusting it.
        let data = self.m.mem.r32(object.wrapping_add(0x40))?;
        if self.m.heap.capacity(data).map_or(true, |c| c < 0x2d4) {
            return Ok(None);
        }
        for list in [0x2b4, 0x2bc, 0x2d0] {
            let l = self.m.mem.r32(data + list)?;
            if self.m.heap.capacity(l).is_none() {
                return Ok(None);
            }
        }
        Ok(Some(data))
    }

    /// Cubics the player holds, if a game is running.
    pub fn cubics(&mut self) -> R<Option<u32>> {
        let Some(player) = self.player()? else { return Ok(None) };
        let count = self.m.export("IEO_Player_GetCountItem")?;
        Ok(Some(self.m.call(count, &[player, ITEM_CUBIC])? as u16 as u32))
    }

    /// Hand over `pending_cubics` a second after the game has entered a map, with the
    /// same three calls the game makes when it restores Cubics from a save file.
    ///
    /// Reading a save replaces the player's items, and the game also reads every save
    /// just to draw the load list, so "a save was read" is not the moment to act. A map
    /// load with no save read after it is: that only happens once play has started.
    fn grant_pending_cubics(&mut self) -> R<()> {
        let take = |c: &Option<Rc<std::cell::Cell<u32>>>| c.as_ref().map_or(0, |c| c.replace(0));
        if take(&self.save_loads) > 0 {
            self.frames_in_map = None;
        }
        if take(&self.map_loads) > 0 {
            self.frames_in_map = Some(0);
        }
        match &mut self.frames_in_map {
            Some(n) if *n >= 25 => {}
            Some(n) => {
                *n += 1;
                return Ok(());
            }
            None => return Ok(()),
        }
        let Some(player) = self.player()? else { return Ok(()) };
        let Some(want) = self.pending_cubics.take() else { return Ok(()) };
        let before = self.cubics()?.unwrap_or(0);
        let total = before.saturating_add(want).min(MAX_ITEM_COUNT);
        if total > before {
            // Adding to an existing stack is refused above 999, but a new stack may hold
            // any count. So do what the game's own load code does: drop the whole stack
            // (count -1), then acquire the new total.
            let export = |s: &Self, name: &str| s.m.export(name);
            let (drop_all, load, acquire, delete) =
                (export(self, "IEO_Player_DropItem2")?, export(self, "IItem_Load")?, export(self, "IEO_Player_AcqItem")?, export(self, "IItem_Delete")?);
            let item = self.m.call(load, &[ITEM_CUBIC])?;
            if item == 0 {
                return Err(Fault::Other("the game could not create a Cubic item".into()));
            }
            self.m.call(drop_all, &[player, 0, ITEM_CUBIC, u32::MAX])?;
            self.m.call(acquire, &[player, item, total])?;
            self.m.call(delete, &[item])?;
        }
        let after = self.cubics()?.unwrap_or(0);
        eprintln!(
            "[host] Cubics: {before} -> {after}{}. Save in the game (Menu, System, Save) to keep them.",
            if total < before.saturating_add(want) { format!(" (the game holds at most {MAX_ITEM_COUNT})") } else { String::new() }
        );
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
