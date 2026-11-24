# dhost — desktop emulation host

Runs the original, unmodified `libdhunter.so` (32-bit ARM, Android) on a desktop computer,
inside a CPU emulator. No Android, no 32-bit ARM device, and no trust in the binary: the
game can only see and do what this program hands it.

Built and tested by CI on Linux, macOS (Apple Silicon) and Windows. The window and sound
have been used on Linux and started on Windows; on macOS they are built but have not been
looked at.

## Running

Needs a recent Rust toolchain (tested with 1.97), CMake and a C compiler (for the Unicorn
CPU emulator).

- **Linux:** sound also needs the ALSA development package, and the window needs the
  xkbcommon libraries at run time (Debian/Ubuntu:
  `libasound2-dev libxkbcommon0 libxkbcommon-x11-0`).
- **macOS:** the Xcode command line tools and CMake (`brew install cmake`).
- **Windows:** see below.

### Windows

The Unicorn build needs more than Rust and CMake on Windows: the Visual Studio C++ build
tools, LLVM (for `libclang`) and Ninja. With `winget`:

```powershell
winget install --id Git.Git -e
winget install --id Kitware.CMake -e
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id Rustlang.Rustup -e
winget install --id LLVM.LLVM -e
winget install --id Ninja-build.Ninja -e
```

Then, in a new terminal:

```powershell
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cd host
cargo run --release -- --lang en
```

What goes wrong without the last two packages:

- `Unable to find libclang`: LLVM is not installed, or `LIBCLANG_PATH` does not point at
  its `bin` directory.
- `unable to find a build program corresponding to "Ninja"`: Ninja is not on `PATH`. The
  copy that ships inside the Visual Studio build tools is found by Unicorn's build script
  but not passed on to CMake, so it only works from a "Developer PowerShell for VS".

Started from a remote shell (SSH, a service), the window opens in a session with no
desktop and nobody sees it. Start it from a terminal on the desktop itself.

### All systems

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
| F11 or Alt+Enter | Fullscreen on and off |
| Numeric keypad, `-`, `=` | Phone keypad `0`–`9`, `*`, `#` |

**Window.** The window can be resized freely; the game picture is enlarged by the largest
whole number that fits and centred, with black bars around it, so every pixel stays the
same size. `--stretch` instead fills the window as far as its shape allows: a bigger
picture, but some pixels come out a little wider than others. F11 or Alt+Enter switches
fullscreen, `--fullscreen` starts that way, and `--scale N` sets the starting size.

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

### Experimental hybrid AOT

Pass `--aot` to use the build-time translations for the original 1.07 library. Cargo
generates native Rust blocks for nine profiled A32 routines and compiles a dedicated
translation for `__aeabi_idiv`. The SHA-256 check pins both the build and dispatch to
the known library. At runtime the translated blocks reuse the checked guest memory
and hand back to Unicorn for external calls, unsupported instructions or state,
and memory accesses that need the emulator's own checks. `--aot` reports native
segment counts at exit. `--profile` counts Unicorn execution, so profile without
`--aot` when comparing the guest routines.

This 500-frame replay loads the first saved game and runs without frame dumps for
timing. Use fresh save directories for each run.

```bash
cargo build --release --locked
scratch=$(mktemp -d)
replay=(--headless --frames 500 --tap 100:240,160 --tap 130:240,210
        --tap 172:240,100 --tap 202:240,100 --key 330:-4 --key 370:-5)
run=0
for mode in unicorn hybrid hybrid unicorn unicorn hybrid; do
    run=$((run + 1))
    flags=()
    if [[ $mode == hybrid ]]; then flags+=(--aot); fi
    target/release/dhost "${replay[@]}" "${flags[@]}" --save "$scratch/$mode-$run"
done
```

On the x86-64 Codex cloud host, median wall time over three runs per mode was
**4.33 s with Unicorn** and **6.96 s with hybrid AOT**. The hybrid run exercised all
ten routines. A separate run with `--dump DIR --dump-every 1` produced
byte-identical frames and save files in both modes.

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
  frame" and "deliver input". `window.rs` (winit and softbuffer: a resizable window drawn
  in software) and the headless loop in `main.rs` sit on top.

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

There are 16 tests; Windows runs 15, because the symlink test needs Unix.

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
