//! Interactive frontend: a resizable window, the mouse as a finger, the keyboard, sound.
//!
//! The game always draws a fixed 480x320 picture. The window can be any size, or
//! fullscreen; the picture is scaled up to fit and centred, with black bars where the
//! shapes differ.

use crate::audio::Audio;
use crate::mem::{Fault, R};
use crate::session::{Input, Session, KEY_BUTTON_C, KEY_DOWN, KEY_JUMP, KEY_LEFT, KEY_MAP, KEY_MENU, KEY_OK, KEY_RIGHT, KEY_SLOT_1, KEY_SLOT_2, KEY_UP};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

/// PC keyboard to the engine's key codes. Arrows move (up jumps); Z, Space and Enter are
/// the A button; X, C, A and S are the B button, the C button and the two quick slots.
/// The numeric keypad plus `-` (for `*`) and `=` (for `#`) stand in for a phone keypad.
fn key_code(k: KeyCode) -> Option<i32> {
    Some(match k {
        KeyCode::ArrowUp => KEY_UP,
        KeyCode::ArrowDown => KEY_DOWN,
        KeyCode::ArrowLeft => KEY_LEFT,
        KeyCode::ArrowRight => KEY_RIGHT,
        KeyCode::Enter | KeyCode::Space | KeyCode::KeyZ => KEY_OK,
        KeyCode::KeyX => KEY_JUMP,
        KeyCode::KeyC => KEY_BUTTON_C,
        KeyCode::KeyA => KEY_SLOT_1,
        KeyCode::KeyS => KEY_SLOT_2,
        KeyCode::KeyM => KEY_MAP,
        KeyCode::Tab => KEY_MENU,
        KeyCode::Numpad0 => b'0' as i32,
        KeyCode::Numpad1 => b'1' as i32,
        KeyCode::Numpad2 => b'2' as i32,
        KeyCode::Numpad3 => b'3' as i32,
        KeyCode::Numpad4 => b'4' as i32,
        KeyCode::Numpad5 => b'5' as i32,
        KeyCode::Numpad6 => b'6' as i32,
        KeyCode::Numpad7 => b'7' as i32,
        KeyCode::Numpad8 => b'8' as i32,
        KeyCode::Numpad9 => b'9' as i32,
        KeyCode::Minus => b'*' as i32,
        KeyCode::Equal => b'#' as i32,
        _ => return None,
    })
}

/// The skill icons down both sides of the play screen have no key codes in the engine:
/// only a touch on the icon uses the skill. So the number row presses the icon instead.
/// 1-5 are the left column from the top, 6-0 the right column from the top.
fn skill_icon(k: KeyCode) -> Option<(i32, i32)> {
    let slot = match k {
        KeyCode::Digit1 => 0,
        KeyCode::Digit2 => 1,
        KeyCode::Digit3 => 2,
        KeyCode::Digit4 => 3,
        KeyCode::Digit5 => 4,
        KeyCode::Digit6 => 5,
        KeyCode::Digit7 => 6,
        KeyCode::Digit8 => 7,
        KeyCode::Digit9 => 8,
        KeyCode::Digit0 => 9,
        _ => return None,
    };
    // Icon centres on the 480x320 game surface: two columns of five, 40 pixels apart.
    Some((if slot < 5 { 20 } else { 460 }, 25 + 40 * (slot % 5)))
}

/// Where the game picture sits inside a window: scaled by `scale` with its top-left
/// corner at (`x`, `y`). All values are in window pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub scale: f64,
    pub x: f64,
    pub y: f64,
}

impl Viewport {
    /// Fit a `game` picture into a `window`, keeping its shape and centring it. With
    /// `whole` the scale is rounded down to a whole number when the window is big enough,
    /// which keeps every game pixel the same size at the cost of wider bars.
    pub fn fit(game: (u32, u32), window: (u32, u32), whole: bool) -> Viewport {
        let (gw, gh) = (game.0.max(1) as f64, game.1.max(1) as f64);
        let (ww, wh) = (window.0 as f64, window.1 as f64);
        let exact = (ww / gw).min(wh / gh).max(1e-6);
        let scale = if whole && exact >= 1.0 { exact.floor() } else { exact };
        Viewport { scale, x: ((ww - gw * scale) / 2.0).floor(), y: ((wh - gh * scale) / 2.0).floor() }
    }

    /// The game pixel under a window position, clamped to the picture's edges.
    pub fn to_game(&self, game: (u32, u32), pos: (f64, f64)) -> (i32, i32) {
        let gx = ((pos.0 - self.x) / self.scale).floor() as i64;
        let gy = ((pos.1 - self.y) / self.scale).floor() as i64;
        (gx.clamp(0, game.0 as i64 - 1) as i32, gy.clamp(0, game.1 as i64 - 1) as i32)
    }

    /// Draw the `game` picture (0RGB pixels) into a `window` buffer, black elsewhere.
    pub fn blit(&self, game: (u32, u32), src: &[u32], window: (u32, u32), dst: &mut [u32]) {
        let (gw, gh) = (game.0 as usize, game.1 as usize);
        let (ww, wh) = (window.0 as usize, window.1 as usize);
        if ww == 0 || wh == 0 || src.len() < gw * gh || dst.len() < ww * wh {
            return;
        }
        // Which source column each window column shows (None = black bar).
        let column = |i: usize, off: f64, n: usize| -> Option<usize> {
            let g = ((i as f64 + 0.5 - off) / self.scale).floor();
            (g >= 0.0 && (g as usize) < n).then_some(g as usize)
        };
        let columns: Vec<Option<usize>> = (0..ww).map(|x| column(x, self.x, gw)).collect();
        for (y, row) in dst.chunks_exact_mut(ww).take(wh).enumerate() {
            match column(y, self.y, gh) {
                Some(gy) => {
                    let line = &src[gy * gw..(gy + 1) * gw];
                    for (d, c) in row.iter_mut().zip(&columns) {
                        *d = c.map_or(0, |gx| line[gx]);
                    }
                }
                None => row.fill(0),
            }
        }
    }
}

type Shown = (Rc<Window>, Surface<Rc<Window>, Rc<Window>>);

struct App<'a> {
    s: &'a mut Session,
    audio: Audio,
    shown: Option<Shown>,
    /// The last game frame as 0RGB pixels.
    frame: Vec<u32>,
    start: Instant,
    next_frame: Instant,
    default_step_ms: u32,
    initial_scale: u32,
    start_fullscreen: bool,
    whole_scale: bool,
    /// Mouse position in window pixels, and where the finger is while the button is held.
    cursor: (f64, f64),
    finger: Option<(i32, i32)>,
    modifiers: ModifiersState,
    /// The first thing that went wrong; ends the loop.
    failed: Option<Fault>,
}

impl App<'_> {
    fn game(&self) -> (u32, u32) {
        (self.s.width, self.s.height)
    }

    fn viewport(&self) -> Option<(Viewport, (u32, u32))> {
        let (window, _) = self.shown.as_ref()?;
        let size = window.inner_size();
        (size.width > 0 && size.height > 0).then(|| (Viewport::fit(self.game(), (size.width, size.height), self.whole_scale), (size.width, size.height)))
    }

    fn cursor_in_game(&self) -> Option<(i32, i32)> {
        self.viewport().map(|(v, _)| v.to_game(self.game(), self.cursor))
    }

    fn toggle_fullscreen(&self) {
        if let Some((window, _)) = &self.shown {
            let next = if window.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) };
            window.set_fullscreen(next);
        }
    }

    /// Run a game frame if one is due, and say when the next is.
    fn tick(&mut self) -> R<()> {
        let now = Instant::now();
        if now < self.next_frame {
            return Ok(());
        }
        self.s.render(self.start.elapsed().as_millis() as u64)?;
        for (dst, px) in self.frame.iter_mut().zip(self.s.pixels().chunks_exact(2)) {
            let v = u16::from_le_bytes([px[0], px[1]]) as u32;
            let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
            *dst = (r << 3 | r >> 2) << 16 | (g << 2 | g >> 4) << 8 | (b << 3 | b >> 2);
        }
        for cmd in self.s.take_audio() {
            self.audio.apply(cmd);
        }
        // The game schedules its own next frame with SetTimer.
        let delay = self.s.take_timer().unwrap_or(self.default_step_ms).clamp(1, 1000);
        self.next_frame = now + Duration::from_millis(delay as u64);
        if let Some((window, _)) = &self.shown {
            window.request_redraw();
        }
        Ok(())
    }

    fn draw(&mut self) -> R<()> {
        let Some((view, size)) = self.viewport() else { return Ok(()) };
        let game = self.game();
        let Some((_, surface)) = self.shown.as_mut() else { return Ok(()) };
        let bad = |e: softbuffer::SoftBufferError| Fault::Other(format!("window: {e}"));
        let (Some(w), Some(h)) = (NonZeroU32::new(size.0), NonZeroU32::new(size.1)) else { return Ok(()) };
        surface.resize(w, h).map_err(bad)?;
        let mut buffer = surface.buffer_mut().map_err(bad)?;
        view.blit(game, &self.frame, size, &mut buffer);
        buffer.present().map_err(bad)
    }

    fn on_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) -> R<()> {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw()?,
            WindowEvent::Resized(_) => {
                if let Some((window, _)) = &self.shown {
                    window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                // The Java layer only reported a move when both coordinates had changed.
                if let (Some((lx, ly)), Some((x, y))) = (self.finger, self.cursor_in_game()) {
                    if x != lx && y != ly {
                        self.s.input(Input::TouchMove(x, y, lx, ly))?;
                        self.finger = Some((x, y));
                    }
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match (state, self.finger, self.cursor_in_game()) {
                (ElementState::Pressed, None, Some((x, y))) => {
                    self.s.input(Input::TouchDown(x, y))?;
                    self.finger = Some((x, y));
                }
                (ElementState::Released, Some((lx, ly)), pos) => {
                    let (x, y) = pos.unwrap_or((lx, ly));
                    self.s.input(Input::TouchUp(x, y, lx, ly))?;
                    self.finger = None;
                }
                _ => {}
            },
            WindowEvent::KeyboardInput { event, .. } if !event.repeat => {
                let PhysicalKey::Code(key) = event.physical_key else { return Ok(()) };
                let pressed = event.state == ElementState::Pressed;
                let alt_enter = key == KeyCode::Enter && self.modifiers.alt_key();
                if key == KeyCode::F11 || alt_enter {
                    if pressed {
                        self.toggle_fullscreen();
                    }
                } else if matches!(key, KeyCode::Escape | KeyCode::Backspace) {
                    if pressed {
                        self.s.input(Input::Back)?;
                    }
                } else if let Some(code) = key_code(key) {
                    self.s.input(if pressed { Input::KeyDown(code) } else { Input::KeyUp(code) })?;
                } else if let Some((x, y)) = skill_icon(key) {
                    self.s.input(if pressed { Input::TouchDown(x, y) } else { Input::TouchUp(x, y, x, y) })?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn open(&mut self, event_loop: &ActiveEventLoop) -> R<()> {
        let (w, h) = self.game();
        let mut attrs = Window::default_attributes()
            .with_title("Demon Hunter (dhost)")
            .with_inner_size(LogicalSize::new(w * self.initial_scale, h * self.initial_scale))
            .with_min_inner_size(LogicalSize::new(w / 2, h / 2));
        if self.start_fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let bad = |what: &str, e: String| Fault::Other(format!("cannot open a window ({what}: {e}); use --headless to run without one"));
        let window = Rc::new(event_loop.create_window(attrs).map_err(|e| bad("window", e.to_string()))?);
        let context = Context::new(window.clone()).map_err(|e| bad("display", e.to_string()))?;
        let surface = Surface::new(&context, window.clone()).map_err(|e| bad("surface", e.to_string()))?;
        self.shown = Some((window, surface));
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, r: R<()>) {
        if let Err(e) = r {
            self.failed.get_or_insert(e);
            event_loop.exit();
        }
    }
}

impl ApplicationHandler for App<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shown.is_none() {
            let r = self.open(event_loop);
            self.fail(event_loop, r);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let r = self.on_event(event_loop, event);
        self.fail(event_loop, r);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let r = self.tick();
        self.fail(event_loop, r);
        if self.s.exit_requested() {
            event_loop.exit();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

pub struct WindowOptions {
    /// Starting size as a multiple of the game picture.
    pub scale: u32,
    pub fullscreen: bool,
    /// Only scale by whole numbers.
    pub whole_scale: bool,
}

pub fn run(s: &mut Session, assets: &Path, opts: WindowOptions, default_step_ms: u32) -> R<()> {
    let WindowOptions { scale, fullscreen, whole_scale } = opts;
    if !(1..=8).contains(&scale) {
        return Err(Fault::Other("--scale must be between 1 and 8".into()));
    }
    // The window library loads system libraries at run time and panics if one is
    // missing (on Linux: libxkbcommon-x11). Report that instead of crashing.
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let made = std::panic::catch_unwind(EventLoop::new);
    std::panic::set_hook(quiet);
    let no_window = |why: String| Fault::Other(format!("cannot open a window: {why} (use --headless to run without one)"));
    let event_loop = made
        .map_err(|_| no_window("a system library the window needs is missing; on Linux install libxkbcommon-x11".into()))?
        .map_err(|e| no_window(e.to_string()))?;
    let now = Instant::now();
    let mut app = App {
        frame: vec![0; (s.width * s.height) as usize],
        s,
        audio: Audio::new(assets),
        shown: None,
        start: now,
        next_frame: now,
        default_step_ms,
        initial_scale: scale,
        start_fullscreen: fullscreen,
        whole_scale,
        cursor: (0.0, 0.0),
        finger: None,
        modifiers: ModifiersState::empty(),
        failed: None,
    };
    let ran = event_loop.run_app(&mut app);
    match app.failed.take() {
        Some(e) => Err(e),
        None => ran.map_err(|e| Fault::Other(format!("window: {e}"))),
    }
}
