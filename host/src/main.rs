//! dhost — a sandboxed desktop emulation host for `libdhunter.so`.
//!
//! The unmodified ARMv7 Android library runs inside a CPU emulator. Everything it can
//! see or do is provided by this program: memory, files, time, the "Java layer" and the
//! frame buffer. See `host/README.md` for the design and the security model.

mod audio;
mod elf;
mod fmt;
mod font;
mod heap;
mod java;
mod jni;
mod libc;
mod machine;
mod mem;
mod session;
#[cfg(test)]
mod tests;
mod vfs;
#[cfg(feature = "window")]
mod window;

use machine::{Clock, Machine, Options};
use mem::{Fault, R};
use session::{Input, Session};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// SHA-256 of the original `libdhunter.so` from version 1.07.
const KNOWN_LIB_SHA256: &str = "69f4be20fb14ccc43297e2eb9e256e8fe1061746ffa71ec74cf1083d6f813218";

/// Languages the game ships, with its internal code for each (0 is Korean).
const LANGUAGES: &[(&str, u8)] = &[("ko", 0), ("en", b'E'), ("ja", b'J'), ("zh", b'C'), ("es", b'S'), ("fr", b'F'), ("de", b'G')];

struct Args {
    lib: PathBuf,
    assets: PathBuf,
    save: PathBuf,
    headless: bool,
    frames: u32,
    step_ms: u32,
    dump: Option<PathBuf>,
    dump_every: u32,
    scale: u32,
    screen: (u32, u32),
    /// `--lang`: (two-letter code reported to the game, the game's own language code).
    language: Option<(String, u8)>,
    country: String,
    add_cubics: Option<u32>,
    fonts: Vec<PathBuf>,
    /// Scripted input for headless runs: (frame, event).
    script: Vec<(u32, Input)>,
    opts: Options,
}

fn usage() -> ! {
    eprintln!(
        "usage: dhost [options]\n\
         \n\
         \x20 --lib PATH        libdhunter.so (default: ../app/src/main/libs/armeabi-v7a/libdhunter.so)\n\
         \x20 --assets DIR      game assets (default: ../app/src/main/assets)\n\
         \x20 --save DIR        save directory, the only place the game can write (default: ./save)\n\
         \x20 --font PATH       font file for in-game text; repeat to add fallbacks (default: system fonts)\n\
         \x20 --lang CODE       set the game language: ko, en, ja, zh, es, fr or de\n\
         \x20                   (default: whatever the save directory's Config.dat says)\n\
         \x20 --add-cubics N    give N Cubics (shop currency) to the game you load or start;\n\
         \x20                   save in the game afterwards to keep them\n\
         \x20 --country CODE    country code reported to the game (default: US)\n\
         \x20 --screen WxH      device resolution reported to the game (default: 480x320)\n\
         \x20 --scale N         window scale: 1, 2 or 4 (default: 2)\n\
         \n\
         headless mode (no window, virtual clock, reproducible):\n\
         \x20 --headless        run without a window\n\
         \x20 --frames N        number of frames to run (default: 100)\n\
         \x20 --step-ms N       virtual milliseconds per frame when the game sets no timer (default: 40)\n\
         \x20 --dump DIR        write frames as PPM images into DIR\n\
         \x20 --dump-every N    dump every Nth frame (default: 10)\n\
         \x20 --tap F:X,Y       touch at frame F, released two frames later (repeatable)\n\
         \x20 --back F          back key at frame F (repeatable)\n\
         \x20 --key F:CODE      key held from frame F for 5 frames; -1..-4 are up/down/left/right, -5 is OK\n\
         \x20 --event F:T,A,B   raw event (type, a, b) at frame F (repeatable)\n\
         \n\
         diagnostics:\n\
         \x20 --verbose         log file, audio and Java-layer activity\n\
         \x20 --trace           log every host call the game makes\n\
         \x20 --profile         count guest instructions per function (slow)"
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let mut a = Args {
        lib: "../app/src/main/libs/armeabi-v7a/libdhunter.so".into(),
        assets: "../app/src/main/assets".into(),
        save: "save".into(),
        headless: !cfg!(feature = "window"),
        frames: 100,
        step_ms: 40,
        dump: None,
        dump_every: 10,
        scale: 2,
        screen: (480, 320),
        language: None,
        country: "US".into(),
        add_cubics: None,
        fonts: Vec::new(),
        script: Vec::new(),
        opts: Options::default(),
    };
    fn num<T: std::str::FromStr>(s: &str) -> T {
        s.parse().unwrap_or_else(|_| usage())
    }
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        // "F:A,B,C" -> (F, [A, B, C])
        let frame_and = |v: String| -> (u32, Vec<i32>) {
            let (f, rest) = v.split_once(':').unwrap_or_else(|| usage());
            (num(f), rest.split(',').map(num).collect())
        };
        match k.as_str() {
            "--lib" => a.lib = val().into(),
            "--assets" => a.assets = val().into(),
            "--save" => a.save = val().into(),
            "--font" => a.fonts.push(val().into()),
            "--lang" => {
                let v = val().to_ascii_lowercase();
                let Some((_, code)) = LANGUAGES.iter().find(|(name, _)| *name == v) else {
                    eprintln!("unknown language `{v}`");
                    usage()
                };
                a.language = Some((v, *code));
            }
            "--country" => a.country = val(),
            "--add-cubics" => a.add_cubics = Some(num(&val())),
            "--screen" => {
                let v = val();
                let (w, h) = v.split_once('x').unwrap_or_else(|| usage());
                a.screen = (num(w), num(h));
            }
            "--scale" => a.scale = num(&val()),
            "--headless" => a.headless = true,
            "--frames" => a.frames = num(&val()),
            "--step-ms" => a.step_ms = num::<u32>(&val()).max(1),
            "--dump" => a.dump = Some(val().into()),
            "--dump-every" => a.dump_every = num::<u32>(&val()).max(1),
            "--tap" => {
                let (f, p) = frame_and(val());
                let [x, y] = p[..] else { usage() };
                a.script.push((f, Input::TouchDown(x, y)));
                a.script.push((f + 2, Input::TouchUp(x, y, x, y)));
            }
            "--back" => a.script.push((num(&val()), Input::Back)),
            "--key" => {
                let (f, p) = frame_and(val());
                let [code] = p[..] else { usage() };
                a.script.push((f, Input::KeyDown(code)));
                a.script.push((f + 5, Input::KeyUp(code)));
            }
            "--event" => {
                let (f, p) = frame_and(val());
                let [t, x, y] = p[..] else { usage() };
                a.script.push((f, Input::Raw(t, x, y)));
            }
            "--verbose" => a.opts.verbose = true,
            "--trace" => a.opts.trace = true,
            "--profile" => a.opts.profile = true,
            _ => usage(),
        }
    }
    a
}

/// Write an RGB565 frame as a binary PPM.
fn write_ppm(path: &std::path::Path, w: u32, h: u32, rgb565: &[u8]) -> std::io::Result<()> {
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in rgb565.chunks_exact(2) {
        let v = u16::from_le_bytes([px[0], px[1]]);
        let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
        out.extend_from_slice(&[(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8]);
    }
    std::fs::write(path, out)
}

fn headless(s: &mut Session, a: &Args) -> R<()> {
    if let Some(d) = &a.dump {
        std::fs::create_dir_all(d).map_err(|e| Fault::Other(format!("{}: {e}", d.display())))?;
    }
    let started = std::time::Instant::now();
    let mut elapsed_ms: u64 = 0;
    let mut costs: Vec<f64> = Vec::new();
    for n in 0..a.frames {
        for (_, ev) in a.script.iter().filter(|(f, _)| *f == n) {
            s.input(*ev)?;
        }
        let t0 = std::time::Instant::now();
        s.render(elapsed_ms)?;
        costs.push(t0.elapsed().as_secs_f64() * 1000.0);
        if let Some(d) = &a.dump {
            if n % a.dump_every == 0 || n + 1 == a.frames {
                let p = d.join(format!("frame{n:05}.ppm"));
                write_ppm(&p, s.width, s.height, &s.pixels()).map_err(|e| Fault::Other(format!("{}: {e}", p.display())))?;
            }
        }
        for cmd in s.take_audio() {
            if a.opts.verbose {
                eprintln!("[audio] frame {n}: {}", audio::describe(&a.assets, &cmd));
            }
        }
        if s.exit_requested() {
            eprintln!("[host] the game asked to exit at frame {n}");
            break;
        }
        let step = s.take_timer().map_or(a.step_ms, |t| t.max(1)) as u64;
        elapsed_ms += step;
        s.m.clock.advance_ms(step);
    }
    if !costs.is_empty() {
        let real = started.elapsed().as_secs_f64();
        costs.sort_by(|x, y| x.total_cmp(y));
        let pick = |q: f64| costs[((costs.len() - 1) as f64 * q) as usize];
        eprintln!(
            "[host] {} frames, {elapsed_ms} ms of game time in {real:.2} s ({:.1}x real time); frame cost median {:.1} ms, 99% {:.1} ms, worst {:.1} ms",
            costs.len(),
            elapsed_ms as f64 / 1000.0 / real.max(1e-9),
            pick(0.5),
            pick(0.99),
            pick(1.0)
        );
    }
    Ok(())
}

fn run(a: &Args) -> Result<(), String> {
    let lib = std::fs::read(&a.lib).map_err(|e| format!("{}: {e}", a.lib.display()))?;
    let digest: String = Sha256::digest(&lib).iter().map(|b| format!("{b:02x}")).collect();
    if digest != KNOWN_LIB_SHA256 {
        eprintln!("[host] note: {} is not the 1.07 library this host was tested with (sha256 {digest})", a.lib.display());
    }
    let boot = || -> Result<Session, String> {
        let mut vfs = vfs::Vfs::new(&a.assets, &a.save)?;
        vfs.verbose = a.opts.verbose;
        let java = java::Java {
            language: a.language.as_ref().map_or("en", |l| l.0.as_str()).to_string(),
            country: a.country.clone(),
            verbose: a.opts.verbose,
            text: Some(font::Text::load(&a.fonts)),
            ..Default::default()
        };
        // Headless runs use a fixed virtual start time (2012-01-01 UTC) so they are reproducible.
        let clock = if a.headless { Clock::Virtual { micros: 1_325_376_000_000_000 } } else { Clock::Real };
        if a.opts.verbose {
            eprintln!("[host] virtual APK: {} bytes; save directory: {}", vfs.apk_len(), a.save.display());
        }
        let m = Machine::new(&lib, vfs, java, clock, a.opts.clone())?;
        Session::boot(m, a.screen).map_err(|e| e.to_string())
    };
    let mut s = boot()?;
    // --lang: the language lives in the game's own config file. If it differs, change it
    // the way the in-game picker does (set, save, restart), then start over.
    if let Some((name, code)) = &a.language {
        s.render(0).map_err(|e| e.to_string())?;
        if s.language().map_err(|e| e.to_string())? != *code {
            s.save_language(*code).map_err(|e| e.to_string())?;
            eprintln!("[host] game language set to `{name}` in {}", a.save.join("Config.dat").display());
            s = boot()?;
        }
    }
    if a.add_cubics.is_some() && digest != KNOWN_LIB_SHA256 {
        return Err("--add-cubics only works with the 1.07 library this host was tested with".into());
    }
    if let Some(n) = a.add_cubics {
        s.give_cubics_in_game(n).map_err(|e| e.to_string())?;
    }
    let r = if a.headless {
        headless(&mut s, a)
    } else {
        #[cfg(feature = "window")]
        {
            window::run(&mut s, &a.assets, a.scale, a.step_ms)
        }
        #[cfg(not(feature = "window"))]
        {
            Err(Fault::Other("built without the `window` feature; use --headless".into()))
        }
    };

    let m = &s.m;
    if a.opts.verbose || a.opts.profile {
        let mut used: Vec<_> = m.imports.iter().filter(|i| i.calls > 0).collect();
        used.sort_by_key(|i| std::cmp::Reverse(i.calls));
        eprintln!("[host] guest heap: {} KiB in use, {} KiB peak", m.heap.in_use >> 10, m.heap.peak >> 10);
        eprintln!("[host] host calls: {}", used.iter().map(|i| format!("{}×{}", i.name, i.calls)).collect::<Vec<_>>().join(" "));
    }
    if a.opts.profile {
        let rep = m.profile_report();
        let total: u64 = rep.iter().map(|e| e.1).sum();
        eprintln!("[host] guest instructions: {total} total; hottest functions:");
        let shown = std::env::var("DHOST_PROFILE_ALL").map_or(25, |_| usize::MAX);
        for (name, n) in rep.iter().take(shown) {
            eprintln!("  {:5.1}%  {n:>12}  {name}", *n as f64 * 100.0 / total.max(1) as f64);
        }
    }
    for e in &m.java.events {
        eprintln!("[host] {e}");
    }
    match r {
        Ok(()) | Err(Fault::Exit(0)) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn main() {
    let a = parse_args();
    if let Err(e) = run(&a) {
        eprintln!("[host] stopped: {e}");
        std::process::exit(1);
    }
}
