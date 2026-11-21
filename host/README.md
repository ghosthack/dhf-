# dhost — desktop emulation host

Runs the original, unmodified `libdhunter.so` (32-bit ARM, Android) on a desktop computer,
inside a CPU emulator. No Android, no 32-bit ARM device, and no trust in the binary: the
game can only see and do what this program hands it.

Tested on Linux x86-64. Nothing in the code is Linux-specific, but macOS and Windows
builds have not been tried.

## Running

Needs a recent Rust toolchain (tested with 1.97), CMake and a C compiler (for the Unicorn
CPU emulator). On
Linux the window and sound also need the X11/Wayland and ALSA development packages
(Debian/Ubuntu: `libx11-dev libxkbcommon-dev libwayland-dev libasound2-dev`).

```sh
cd host
cargo run --release
```

By default it takes the library and assets from `../app/src/main` and keeps saves in
`./save`. For English text, start it once with `cargo run --release -- --lang en`. `cargo run --release -- --help` lists the options.

| Input | Action |
|---|---|
| Mouse (left button) | Touch |
| Arrow keys | Move; up jumps |
| Z, Space or Enter | Button A: attack, talk, confirm |
| X | Button B: jump |
| C | Button C |
| A, S | Quick slots 1 and 2 (potions) |
| `1`–`5` | Skill icons, left column from the top |
| `6`–`0` | Skill icons, right column from the top |
| M | Map |
| Tab | Menu |
| Esc or Backspace | Android back key |
| Numeric keypad, `-`, `=` | Phone keypad `0`–`9`, `*`, `#` |

**Language.** The bundled configuration is set to Chinese. `--lang en` (or `ko`, `ja`,
`zh`, `es`, `fr`, `de`) changes it: the host sets the language through the game's own
functions and the game writes it to `Config.dat` in the save directory, so it only needs
to be given once. It can also be changed in the game under Menu → System → Option →
Language Change.

**Shops.** The Cash store needed the publisher's servers and is dead. The Cubic store
works offline, but Cubics could only be bought with real money. `--add-cubics N` gives
the game you load or start N Cubics, about a second after you arrive in the game world.
The game holds at most 32,767. Buying something saves the game; otherwise save from the
menu to keep them.

**Text.** The game draws text with the system font. The host looks for common Latin and
CJK fonts; pass `--font PATH` (repeatable, earlier fonts win) to choose. Korean, Japanese
and Chinese need a CJK font, otherwise text shows as empty boxes.

**Sound.** The bundled configuration has music and sound effects turned down to zero;
raise them in the same Option screen.

**Without a window.** `--headless` runs a fixed number of frames on a virtual clock, with
scripted input, and can write frames as images. Two runs with the same arguments draw
identical frames.

```sh
cargo run --release -- --headless --frames 300 --tap 100:240,160 --dump frames --dump-every 50
```

To build without the window and sound dependencies:
`cargo build --release --no-default-features` (headless only).

## How it works

```
 libdhunter.so (ARM code, untouched)
        │  jumps to a trap address for every imported function and JNI call
        ▼
 Unicorn CPU emulator ── stops ──► host handler (Rust) ── bounds-checked ──► guest memory
        ▲                                │
        └────────── resumes ─────────────┘
```

- `elf.rs` loads and relocates the library. Each of its 100 imports, and each slot of the
  `JNIEnv` and `JavaVM` tables, is pointed at its own address in a *trap page*.
- `machine.rs` runs the CPU. When the guest jumps to a trap address the emulator stops and
  the matching handler runs as ordinary Rust, then the guest resumes.
- `libc.rs` is the guest's whole operating system: memory, strings, `printf`, time, files,
  zlib inflate. `heap.rs` is its allocator, with all bookkeeping on the host side.
- `vfs.rs` is its file system: a read-only "APK" (a zip built in memory from the assets
  directory, because the game reads assets with its own zip code) and the save directory.
- `jni.rs` and `java.rs` stand in for Android's Java runtime and the game's Java classes:
  strings, byte arrays, fonts (`font.rs`), timers, audio requests.
- `session.rs` boots the game in the order the Android activity did and exposes "render a
  frame" and "deliver input". `window.rs` and the headless loop in `main.rs` sit on top.

The game renders in software: each frame it fills a 480×320 RGB565 buffer
(`BitmapSurfaceView.FillBuffer`), so the host needs no OpenGL.

## Security model

The binary is treated as hostile. What it can reach:

| Resource | What the guest gets |
|---|---|
| Code | Its own ARM code, emulated. It has no system-call instructions and the host offers no way to load or generate more code (`dlopen` is refused; library code is read-only, data is not executable). |
| Memory | A fixed set of regions owned by the host. Every pointer and length the guest passes to a host function is checked against them; a bad one stops the guest. |
| Files | `/apk/base.apk` (read-only, in memory) and `/data/…` (the save directory). Paths are normalised component by component and symlinks leading outside are refused. Nothing else exists. |
| Network | None. Socket functions fail; the host contains no networking code. |
| Threads, processes | None. `pthread_create` fails (its only caller is the network code). |
| Device identity | Fixed fake values, the same ones the Android build reports. |
| Time | UTC only; a virtual clock in headless mode. |
| URLs, payments, ads | Logged and ignored. |
| Fonts, audio | The guest names a character or a clip; the host does the rendering and decoding with memory-safe Rust libraries and never hands over a file. |

A guest fault — wild jump, bad pointer, stack-canary failure, unknown import — ends the
run with a diagnostic (registers, symbolised addresses). It is never a host crash by
design; `cargo test` exercises this boundary.

What this does **not** protect against: bugs in the Unicorn emulator itself (it is C code
running in the host process), and resource exhaustion (the guest can spin the CPU, and
although its own heap is capped at 256 MiB, the host does not strictly limit the Java
objects it creates). For stronger isolation, run the headless
build inside an OS sandbox as well.

## Tests

```sh
cargo test --release
```

They cover the file-system confinement, the heap, rejected and corrupted library files,
wild guest jumps and pointers, refused network/thread/loader calls, `printf`, the sound
path, and an
end-to-end run that boots the game, taps into a saved game and checks which files exist
afterwards.

## Notes from getting it running

- **Speed.** With the ARM MMU off, Unicorn treats every page as executable and sends every
  guest store down a slow path. The host therefore turns the guest MMU on with identity
  page tables that mark data no-execute (`Machine::enable_mmu`). A gameplay frame costs
  about 8 ms on a 2.1 GHz Xeon core, against the game's 40 ms tick; without this it was
  31 ms.
- **`JNI_ABORT`.** `FillBuffer` draws into the Java byte array and then releases it with
  `JNI_ABORT`, relying on Dalvik having handed out the array's own storage. The host
  copies array contents back on every release to match.
- **Strings.** The library converts Java strings with `String.getBytes("EUC_KR")` and
  builds them with `new String(byte[])`.
- **Keys.** `nativeHandleEvent` type 2/3 is key down/up with feature-phone codes
  (−1…−4 directions, −5 OK, ASCII digits, −22 back); types 23/24/26 are touch
  down/up/move.
- **Start-up.** The library makes one `socket()` call at start-up and continues when it
  fails. It also probes for `/data/Patch.dat` and writes `/data/Debug.txt`.

## Not done yet

- Only the mouse acts as a finger; there is no multi-touch.
- The on-screen text field (`EditText`, used for online login) is not implemented.
- Vibration is ignored.
- Audio is tested up to the samples a sound device would be given (`cargo test` plays a
  real effect and music track into memory and checks loudness, volume, looping and
  stopping). The final hand-off to a device has not been listened to.
