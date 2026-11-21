//! Tests of the sandbox boundary and of the game actually running.
//!
//! The end-to-end tests use the library and assets from this repository.

use crate::machine::{Clock, Machine, Options, HEAP_BASE, STACK_BASE, TRAP_BASE};
use crate::mem::Fault;
use crate::session::{Input, Session};
use crate::vfs::{Target, Vfs};
use crate::{elf, heap, java, vfs};
use std::path::{Path, PathBuf};

const LIB: &str = "../app/src/main/libs/armeabi-v7a/libdhunter.so";
const ASSETS: &str = "../app/src/main/assets";

fn temp_dir(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("dhost-test-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A tiny assets directory, for tests that do not need the real game.
fn small_assets() -> PathBuf {
    let d = temp_dir("assets");
    std::fs::create_dir_all(d.join("Default/I")).unwrap();
    std::fs::write(d.join("Default/I/I0I"), b"hello").unwrap();
    std::fs::write(d.join("Config.dat"), b"config").unwrap();
    d
}

fn machine(save: &Path) -> Machine {
    let lib = std::fs::read(LIB).expect("library present in the repository");
    let vfs = Vfs::new(Path::new(ASSETS), save).unwrap();
    let java = java::Java { language: "en".into(), country: "US".into(), ..Default::default() };
    Machine::new(&lib, vfs, java, Clock::Virtual { micros: 1_325_376_000_000_000 }, Options::default()).unwrap()
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn vfs_only_exposes_the_apk_and_the_save_directory() {
    let (assets, save) = (small_assets(), temp_dir("save"));
    let v = Vfs::new(&assets, &save).unwrap();
    assert!(matches!(v.resolve(b"/apk/base.apk"), Target::Apk));
    assert!(matches!(v.resolve(b"/data/Save0.dat"), Target::Doc(_)));
    assert!(matches!(v.resolve(b"/data/sub/dir/file"), Target::Doc(_)));
    for bad in [
        &b"/etc/passwd"[..],
        b"/data/../etc/passwd",
        b"/data/a/../../b",
        b"/data/..",
        b"/dataX/file",
        b"/apk/base.apk/..",
        b"/apk/other",
        b"data/Save0.dat",
        b"",
        b"/data/a\\..\\b",
        b"/data/C:evil",
        b"/data/\xff\xfe",
    ] {
        assert!(matches!(v.resolve(bad), Target::Nothing), "{:?} must not resolve", String::from_utf8_lossy(bad));
    }
    // The bundled config is copied into the save directory on first start, as on Android.
    assert_eq!(files_in(&save), ["Config.dat"]);
}

#[cfg(unix)]
#[test]
fn vfs_refuses_symlinks_that_leave_the_save_directory() {
    let (assets, save, outside) = (small_assets(), temp_dir("save"), temp_dir("outside"));
    std::fs::write(outside.join("secret"), b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, save.join("link")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), save.join("file")).unwrap();
    let mut v = Vfs::new(&assets, &save).unwrap();
    assert!(matches!(v.resolve(b"/data/link/secret"), Target::Nothing));
    assert!(matches!(v.resolve(b"/data/link/new"), Target::Nothing));
    assert!(matches!(v.resolve(b"/data/file"), Target::Nothing));
    assert!(v.fopen(1, b"/data/file", b"r").is_err());
    assert!(v.fopen(1, b"/data/link/new", b"w").is_err());
    assert_eq!(files_in(&outside), ["secret"]);
}

#[test]
fn the_apk_cannot_be_written() {
    let (assets, save) = (small_assets(), temp_dir("save"));
    let mut v = Vfs::new(&assets, &save).unwrap();
    for mode in [&b"w"[..], b"a", b"r+", b"wb+"] {
        assert!(v.fopen(1, vfs::APK_PATH.as_bytes(), mode).is_err());
    }
    assert!(v.fopen(1, vfs::APK_PATH.as_bytes(), b"rb").is_ok());
    assert!(v.doc_path(vfs::APK_PATH.as_bytes()).is_err());
    assert!(v.doc_path(vfs::DOC_PATH.as_bytes()).is_err(), "the save directory itself cannot be removed or renamed");
}

#[test]
fn synthesised_apk_is_a_well_formed_zip() {
    let (assets, save) = (small_assets(), temp_dir("save"));
    let mut v = Vfs::new(&assets, &save).unwrap();
    v.fopen(1, vfs::APK_PATH.as_bytes(), b"rb").unwrap();
    let mut zip = vec![0u8; v.apk_len()];
    assert_eq!(v.open.get_mut(&1).unwrap().read(&mut zip), zip.len());
    let u16at = |o: usize| u16::from_le_bytes(zip[o..o + 2].try_into().unwrap()) as usize;
    let u32at = |o: usize| u32::from_le_bytes(zip[o..o + 4].try_into().unwrap()) as usize;
    let eocd = zip.len() - 22;
    assert_eq!(u32at(eocd), 0x0605_4b50);
    assert_eq!(u16at(eocd + 10), 2);
    let mut at = u32at(eocd + 16);
    let mut names = Vec::new();
    for _ in 0..2 {
        assert_eq!(u32at(at), 0x0201_4b50);
        let (crc, size, name_len, local) = (u32at(at + 16), u32at(at + 24), u16at(at + 28), u32at(at + 42));
        names.push(String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap());
        assert_eq!(u32at(local), 0x0403_4b50);
        let data = local + 30 + u16at(local + 26) + u16at(local + 28);
        assert_eq!(vfs::crc32(0, &zip[data..data + size]) as usize, crc);
        at += 46 + name_len;
    }
    assert_eq!(names, ["assets/Config.dat", "assets/Default/I/I0I"]);
    assert_eq!(vfs::crc32(0, b"123456789"), 0xcbf4_3926);
}

#[test]
fn heap_detects_bad_frees_and_exhaustion() {
    let mut h = heap::Heap::new(0x1000_0000, 0x10_0000);
    let (a, b) = (h.alloc(24), h.alloc(5000));
    assert!(a != 0 && b != 0 && a != b);
    assert!(h.free(a));
    assert!(!h.free(a), "double free must be detected");
    assert!(!h.free(a + 8), "interior pointer must be rejected");
    assert!(!h.free(0x1234), "wild pointer must be rejected");
    assert_eq!(h.alloc(24), a, "small blocks are recycled");
    assert_eq!(h.alloc(0x2000_0000), 0);
    assert_eq!(h.alloc(u32::MAX), 0);
    assert!(h.free(b));
    let c = h.alloc(5000);
    assert_eq!(c, b, "large blocks are recycled");
    while h.alloc(0x8000) != 0 {}
    assert_eq!(h.alloc(0x8000), 0, "exhaustion is a failed malloc, not a crash");
}

#[test]
fn malformed_libraries_are_rejected_without_panicking() {
    assert!(elf::load(b"").is_err());
    assert!(elf::load(b"not an elf file at all").is_err());
    let lib = std::fs::read(LIB).unwrap();
    assert!(elf::load(&lib).is_ok());
    for cut in [1, 16, 52, 100, 0x1000, lib.len() / 2, lib.len() - 1] {
        let _ = elf::load(&lib[..cut]);
    }
    // Corrupt header, dynamic-section and symbol-table bytes with a fixed pseudo-random pattern.
    let mut seed = 0x2545_f491u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..300 {
        let mut bad = lib.clone();
        for _ in 0..4 {
            let region = match next() % 4 {
                0 => 0..0x100,
                1 => 0x5790..0x13210,
                2 => 0x21554..0x247ec,
                _ => 0xfdd9c..0xfde7c,
            };
            let at = region.start + next() as usize % (region.end - region.start);
            bad[at] = next() as u8;
        }
        let _ = elf::load(&bad);
    }
}

#[test]
fn wild_guest_jumps_and_pointers_stop_the_guest_not_the_host() {
    let save = temp_dir("save");
    let mut m = machine(&save);
    // Jumping to unmapped memory, to data, or to the stack is a contained CPU fault.
    for target in [0xdead_0000u32, 0, HEAP_BASE + 0x100, STACK_BASE + 0x100, TRAP_BASE + 0x1ff0, TRAP_BASE + 6] {
        assert!(matches!(m.call(target, &[]), Err(Fault::Cpu(_))), "jump to {target:#x}");
    }
    // Host calls with wild pointers are refused by the bounds checks.
    let call = |m: &mut Machine, name: &str, args: &[u32]| {
        let addr = m.import_addr(name).unwrap();
        m.call(addr, args)
    };
    let buf = m.galloc(64).unwrap();
    for (name, args) in [
        ("memcpy", vec![buf, 0xdead_0000, 16]),
        ("memcpy", vec![0xdead_0000, buf, 16]),
        ("memcpy", vec![buf, buf, 0xffff_fff0]),
        ("memset", vec![HEAP_BASE + 0x0fff_fff0, 0, 0x100]),
        ("memset", vec![0xffff_fff0, 0, 0x100]),
        ("strlen", vec![0]),
        ("strcpy", vec![0x1000_0000, buf]),
        ("fread", vec![buf, 0x1_0000, 0x1_0000, 0]),
        ("sprintf", vec![buf, 0x10]),
    ] {
        let r = call(&mut m, name, &args);
        assert!(matches!(r, Err(Fault::BadAccess { .. })), "{name}{args:x?} gave {:?}", r.map_err(|e| e.to_string()));
    }
    // Library code is not writable, even by the host on the guest's behalf.
    let code = m.export("JNI_OnLoad").unwrap();
    assert!(matches!(call(&mut m, "memset", &[code, 0, 4]), Err(Fault::BadAccess { write: true, .. })));
    // The machine is still usable afterwards.
    assert_eq!(call(&mut m, "strlen", &[buf]).unwrap(), 0);
}

#[test]
fn host_calls_refuse_files_network_threads_and_loading() {
    let save = temp_dir("save");
    let mut m = machine(&save);
    let cstr = |m: &mut Machine, s: &str| {
        let a = m.galloc(s.len() as u32 + 1).unwrap();
        m.mem.write(a, s.as_bytes()).unwrap();
        a
    };
    let call = |m: &mut Machine, name: &str, args: &[u32]| {
        let addr = m.import_addr(name).unwrap();
        m.call(addr, args).unwrap()
    };
    let (r, w) = (cstr(&mut m, "r"), cstr(&mut m, "w"));
    for path in ["/etc/passwd", "/data/../../etc/passwd", "/proc/self/maps", "../app/build.gradle", "/data"] {
        let p = cstr(&mut m, path);
        assert_eq!(call(&mut m, "fopen", &[p, r]), 0, "fopen({path})");
        assert_eq!(call(&mut m, "fopen", &[p, w]), 0, "fopen({path}, w)");
        assert_eq!(call(&mut m, "remove", &[p]), u32::MAX);
        assert_eq!(call(&mut m, "mkdir", &[p, 0o777]), u32::MAX);
    }
    let apk = cstr(&mut m, vfs::APK_PATH);
    assert_eq!(call(&mut m, "fopen", &[apk, w]), 0);
    assert_eq!(call(&mut m, "remove", &[apk]), u32::MAX);
    let (inside, outside) = (cstr(&mut m, "/data/Save0.dat"), cstr(&mut m, "/tmp/stolen"));
    assert_eq!(call(&mut m, "rename", &[inside, outside]), u32::MAX);
    assert!(save.join("Save0.dat").exists());

    // A file inside the save directory round-trips.
    let note = cstr(&mut m, "/data/note.txt");
    let f = call(&mut m, "fopen", &[note, w]);
    assert_ne!(f, 0);
    assert_eq!(call(&mut m, "fwrite", &[note, 1, 5, f]), 5);
    assert_eq!(call(&mut m, "fclose", &[f]), 0);
    assert_eq!(std::fs::read(save.join("note.txt")).unwrap(), b"/data");

    assert_eq!(call(&mut m, "socket", &[2, 1, 0]), u32::MAX);
    let host = cstr(&mut m, "account.bicore.co.kr");
    assert_eq!(call(&mut m, "gethostbyname", &[host]), 0);
    assert_ne!(call(&mut m, "pthread_create", &[0, 0, 0, 0]), 0);
    let so = cstr(&mut m, "libGLESv1_CM.so");
    assert_eq!(call(&mut m, "dlopen", &[so, 0]), 0);

    // %n must not write through its argument.
    let (out, target, f) = (m.galloc(64).unwrap(), m.galloc(4).unwrap(), cstr(&mut m, "ab%ncd %d"));
    m.mem.w32(target, 0x5555_5555).unwrap();
    assert_eq!(call(&mut m, "sprintf", &[out, f, target, 42]), 7);
    assert_eq!(m.mem.cstr(out).unwrap(), b"abcd 42");
    assert_eq!(m.mem.r32(target).unwrap(), 0x5555_5555);
}

#[test]
fn printf_formats_like_c() {
    let save = temp_dir("save");
    let mut m = machine(&save);
    let mut fmt = |f: &str, args: &[u32]| {
        let fa = m.galloc(f.len() as u32 + 1).unwrap();
        m.mem.write(fa, f.as_bytes()).unwrap();
        let out = m.galloc(256).unwrap();
        let mut full = vec![out, fa];
        full.extend_from_slice(args);
        let addr = m.import_addr("sprintf").unwrap();
        let n = m.call(addr, &full).unwrap();
        let s = String::from_utf8(m.mem.cstr(out).unwrap().to_vec()).unwrap();
        assert_eq!(n as usize, s.len());
        s
    };
    assert_eq!(fmt("%d|%5d|%-5d|%05d|%x|%X|%u", &[-7i32 as u32, 42, 42, 42, 255, 255, u32::MAX]), "-7|   42|42   |00042|ff|FF|4294967295");
    assert_eq!(fmt("%c%c %% %s|%8s|", &[b'o' as u32, b'k' as u32, 0, 0]), "ok % (null)|  (null)|");
    // A double after one int: skips r3 for alignment and is read from the stack.
    let bits = 2.5f64.to_bits();
    assert_eq!(fmt("%d %.2f", &[1, 0, bits as u32, (bits >> 32) as u32]), "1 2.50");
    assert_eq!(fmt("%lld", &[0xffff_fffe, 0xffff_ffff]), "-2");
}

#[test]
fn the_game_boots_renders_and_stays_inside_its_sandbox() {
    let save = temp_dir("save");
    let mut s = Session::boot(machine(&save), (480, 320)).unwrap();
    assert_eq!((s.width, s.height), (480, 320));
    let mut ms = 0;
    let mut run = |s: &mut Session, frames: u32| {
        for _ in 0..frames {
            s.render(ms).unwrap();
            let step = s.take_timer().unwrap_or(40).max(1) as u64;
            ms += step;
            s.m.clock.advance_ms(step);
        }
    };
    run(&mut s, 100);
    let title = s.pixels().clone();
    let colours: std::collections::HashSet<[u8; 2]> = title.chunks_exact(2).map(|p| [p[0], p[1]]).collect();
    assert!(colours.len() > 1000, "the title screen should be a full-colour picture, got {} colours", colours.len());

    // Tap through the title screen and the menu into a saved game.
    for (x, y) in [(240, 160), (240, 210), (240, 160), (240, 160)] {
        s.input(Input::TouchDown(x, y)).unwrap();
        run(&mut s, 2);
        s.input(Input::TouchUp(x, y, x, y)).unwrap();
        run(&mut s, 28);
    }
    run(&mut s, 60);
    assert_ne!(*s.pixels(), title, "input should have changed the screen");

    // The only files the game produced are in the save directory.
    assert_eq!(files_in(&save), ["Config.dat", "Debug.txt", "Save0.dat", "Save1.dat", "Save2.dat"]);
    // It tried the network once at start-up and was refused; it never started a thread.
    let calls = |name: &str| s.m.imports.iter().find(|i| i.name == name).map_or(0, |i| i.calls);
    assert!(calls("socket") >= 1);
    assert_eq!(calls("connect") + calls("send") + calls("recv") + calls("pthread_create") + calls("dlopen"), 0);
    assert!(!s.exit_requested());
}

#[test]
fn headless_runs_are_reproducible() {
    let frame = || {
        let save = temp_dir("save");
        let mut s = Session::boot(machine(&save), (480, 320)).unwrap();
        let mut ms = 0;
        for n in 0..150 {
            if n == 100 {
                s.input(Input::TouchDown(240, 160)).unwrap();
            }
            s.render(ms).unwrap();
            let step = s.take_timer().unwrap_or(40).max(1) as u64;
            ms += step;
            s.m.clock.advance_ms(step);
        }
        let px = s.pixels().clone();
        px
    };
    assert!(frame() == frame(), "two runs with the same input must draw the same frame");
}

#[cfg(feature = "audio")]
#[test]
fn bundled_audio_decodes() {
    use rodio::Source;
    for (dir, ext) in [("W", "ogg"), ("B", "mp3")] {
        let d = Path::new(ASSETS).join("Default").join(dir);
        let first = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|e| e == ext)).unwrap();
        let src = rodio::Decoder::new(std::io::BufReader::new(std::fs::File::open(&first).unwrap())).unwrap();
        assert!(src.sample_rate() >= 8000);
        assert!(src.take(1000).count() == 1000, "{} should decode", first.display());
    }
}
