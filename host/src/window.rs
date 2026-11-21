//! Interactive frontend: a window, the mouse as a finger, and sound.

use crate::audio::Audio;
use crate::mem::{Fault, R};
use crate::session::{Input, Session, KEY_DOWN, KEY_LEFT, KEY_OK, KEY_RIGHT, KEY_UP};
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Scale, Window, WindowOptions};
use std::path::Path;
use std::time::{Duration, Instant};

/// PC keyboard to the engine's key codes: arrows move, Enter/Space/Z is OK, and the
/// digit row plus `-` (for `*`) and `=` (for `#`) stand in for a phone keypad.
fn key_code(k: Key) -> Option<i32> {
    Some(match k {
        Key::Up => KEY_UP,
        Key::Down => KEY_DOWN,
        Key::Left => KEY_LEFT,
        Key::Right => KEY_RIGHT,
        Key::Enter | Key::Space | Key::Z => KEY_OK,
        Key::Key0 | Key::NumPad0 => b'0' as i32,
        Key::Key1 | Key::NumPad1 => b'1' as i32,
        Key::Key2 | Key::NumPad2 => b'2' as i32,
        Key::Key3 | Key::NumPad3 => b'3' as i32,
        Key::Key4 | Key::NumPad4 => b'4' as i32,
        Key::Key5 | Key::NumPad5 => b'5' as i32,
        Key::Key6 | Key::NumPad6 => b'6' as i32,
        Key::Key7 | Key::NumPad7 => b'7' as i32,
        Key::Key8 | Key::NumPad8 => b'8' as i32,
        Key::Key9 | Key::NumPad9 => b'9' as i32,
        Key::Minus => b'*' as i32,
        Key::Equal => b'#' as i32,
        _ => return None,
    })
}

pub fn run(s: &mut Session, assets: &Path, scale: u32, default_step_ms: u32) -> R<()> {
    let (w, h) = (s.width as usize, s.height as usize);
    let scale = match scale {
        1 => Scale::X1,
        2 => Scale::X2,
        4 => Scale::X4,
        _ => return Err(Fault::Other("--scale must be 1, 2 or 4".into())),
    };
    let mut win = Window::new("Demon Hunter (dhost)", w, h, WindowOptions { scale, ..WindowOptions::default() })
        .map_err(|e| Fault::Other(format!("cannot open a window: {e} (use --headless to run without one)")))?;
    win.set_target_fps(120);
    let mut audio = Audio::new(assets);
    let mut rgb = vec![0u32; w * h];
    let start = Instant::now();
    let mut next_frame = start;
    // Where the finger is, while the left button is held.
    let mut finger: Option<(i32, i32)> = None;

    while win.is_open() && !s.exit_requested() {
        let pos = win.get_mouse_pos(MouseMode::Clamp).map(|(x, y)| (x as i32, y as i32));
        let held = win.get_mouse_down(MouseButton::Left);
        match (finger, pos, held) {
            (None, Some((x, y)), true) => {
                s.input(Input::TouchDown(x, y))?;
                finger = Some((x, y));
            }
            // The Java layer only reported a move when both coordinates had changed.
            (Some((lx, ly)), Some((x, y)), true) if x != lx && y != ly => {
                s.input(Input::TouchMove(x, y, lx, ly))?;
                finger = Some((x, y));
            }
            (Some((lx, ly)), p, false) => {
                let (x, y) = p.unwrap_or((lx, ly));
                s.input(Input::TouchUp(x, y, lx, ly))?;
                finger = None;
            }
            _ => {}
        }
        for k in win.get_keys_pressed(KeyRepeat::No) {
            match k {
                Key::Escape | Key::Backspace => s.input(Input::Back)?,
                k => {
                    if let Some(code) = key_code(k) {
                        s.input(Input::KeyDown(code))?;
                    }
                }
            }
        }
        for k in win.get_keys_released() {
            if let Some(code) = key_code(k) {
                s.input(Input::KeyUp(code))?;
            }
        }

        let now = Instant::now();
        if now >= next_frame {
            s.render(start.elapsed().as_millis() as u64)?;
            for (dst, px) in rgb.iter_mut().zip(s.pixels().chunks_exact(2)) {
                let v = u16::from_le_bytes([px[0], px[1]]) as u32;
                let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
                *dst = (r << 3 | r >> 2) << 16 | (g << 2 | g >> 4) << 8 | (b << 3 | b >> 2);
            }
            for cmd in s.take_audio() {
                audio.apply(cmd);
            }
            // The game schedules its own next frame with SetTimer.
            let delay = s.take_timer().unwrap_or(default_step_ms).clamp(1, 1000);
            next_frame = now + Duration::from_millis(delay as u64);
            win.update_with_buffer(&rgb, w, h).map_err(|e| Fault::Other(format!("window: {e}")))?;
        } else {
            win.update();
        }
    }
    Ok(())
}
