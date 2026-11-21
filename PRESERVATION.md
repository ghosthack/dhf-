# Preservation notes

This repository keeps a playable build of **Demon Hunter** (Bicore, Android, version 1.07)
working, with compatibility fixes for newer Android versions. These notes record what is
here, how to build it, what is known about the original binary, and what options exist
for the future, so nobody has to rediscover them.

The game and its assets belong to their rights holder. This repository keeps the original
binary and assets intact; it does not reimplement the game.

## History

- The original game is a Bicore title from the early 2010s, distributed through Korean
  carrier stores (KT olleh, LG U+, SK T-store).
- The repository is based on [eipip1e0/DemonHunterFix](https://github.com/eipip1e0/DemonHunterFix),
  which rebuilt the APK as a Gradle project (Feb 2023) and patched it to stop crashing on
  newer Android versions.
- Later commits removed the online check when starting a new game (Mar 2023), reduced the
  network request timeout to 1031 ms (Aug 2023) and added 29285 Cubics to `Save1.dat`.
- Nov 2026: pinned the build to a Gradle 7.6.4 wrapper so it builds with current tools.

## What is in the repository

| Path | What it is |
|---|---|
| `app/src/main/libs/armeabi-v7a/libdhunter.so` | **The game.** Prebuilt 32-bit ARM (ARMv7) native library: game logic, UI, rendering, networking. No source. |
| `app/src/main/libs/armeabi-v7a/libdhunter.so.idb` | IDA Pro database of the library from earlier reverse-engineering work. Not used by the build. |
| `app/src/main/java/com/bicore/` | Java layer (about 4,000 lines, recovered from the original APK): activity, GL surface, audio, music, font rendering, JNI bridge (`NativeFuntion.java`). |
| `app/src/main/java/com/kt`, `com/lguplus`, `com/skt` | Interface stubs for the three Korean carriers' billing and licensing services. These services no longer exist. |
| `app/src/main/assets/Default`, `HVGA` | Game assets for two screen sizes (about 27 MB and 20 MB). |
| `app/src/main/assets/Save0-2.dat`, `Config.dat` | Bundled save slots and configuration. `Save1.dat` has been modified (extra Cubics), so it is not the original. |
| `app/src/main/classes.dex` | Original compiled Java from the APK. Kept for reference; the build compiles the Java sources instead. |
| `app/src/main/res` | Icons, layout and strings. |

Checksums of the original binaries (SHA-256):

```
69f4be20fb14ccc43297e2eb9e256e8fe1061746ffa71ec74cf1083d6f813218  libdhunter.so
f13a8a79ecfb193d2f653df9d0d475ae5c3950a5c79735ea7f85a418a6aacfbb  classes.dex
```

## Building

Requirements:

- JDK 17. Gradle 7.6 does not run on JDK 21 or newer.
- Android SDK with `platforms;android-32` and `build-tools;30.0.3`.
- Nothing else: the Gradle wrapper downloads Gradle 7.6.4, and Android Gradle Plugin 7.3.1
  comes from Google's Maven repository.

```sh
echo "sdk.dir=/path/to/android-sdk" > local.properties
JAVA_HOME=/path/to/jdk-17 ./gradlew assembleDebug
```

Output: `app/build/outputs/apk/debug/app-debug.apk` (about 23 MB, signed with the debug key).

To install the SDK from scratch on Linux:

```sh
SDK=/opt/android-sdk
mkdir -p $SDK/cmdline-tools && cd /tmp
curl -LO https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip
unzip commandlinetools-linux-*_latest.zip -d $SDK/cmdline-tools
mv $SDK/cmdline-tools/cmdline-tools $SDK/cmdline-tools/latest
yes | $SDK/cmdline-tools/latest/bin/sdkmanager --sdk_root=$SDK --licenses
$SDK/cmdline-tools/latest/bin/sdkmanager --sdk_root=$SDK "platforms;android-32" "build-tools;30.0.3"
```

If `dl.google.com` stops serving these exact versions, any copy of `android-32` and
`build-tools 30.0.3` works.

## Running

- **Devices:** needs a device that can run 32-bit ARM apps. Many newer phones have dropped
  32-bit support entirely and **cannot run this game at all**. This is the main threat to
  it staying playable.
- **Display:** landscape, OpenGL ES 1.x.
- **Network:** the game's online services are long gone, and new games no longer need them
  (see History). See the security notes before allowing it network access.

## What we know about the native binary

From a static check in November 2026. This is not a full audit.

**Interface to the Java layer**
- 32 JNI entry points (`nativeInit`, `nativeRender`, `nativeHandleEvent`, `nativeResize`,
  `nativeTextureSubImage2D`, the purchase callbacks and so on), declared in
  `com/bicore/NativeFuntion.java`.
- The native code calls back into Java for audio, text drawing, device info and URL opening.

**System dependencies**
- Links only against `libc`, `libm`, `libdl`, `libstdc++`, `libz`, `liblog` and
  `libGLESv1_CM`.
- About 100 imported functions: standard C library, zlib, BSD sockets, 15 OpenGL ES 1.x
  calls and Android logging.
- Not stripped: about 3,000 named functions, which makes analysis much easier.

**Checked and not found**
- No process execution: no `exec*`, `system`, `popen`, `fork` or `ptrace` imports.
- No direct system calls: zero `svc` instructions with correct ARM disassembly. The binary
  contains no Thumb code.
- No hidden or self-modifying code: no memory region is both writable and executable,
  there are no `mmap` or `mprotect` imports, and the code section's entropy (6.25
  bits/byte) is normal for unpacked ARM code.
- `dlopen` only appears next to standard system library names.

**Present and worth knowing about**
- **Full online features:** login, password recovery, rankings, inventory and save sync,
  cash shop and payments (`IEU_NetComm_*`, `INET_*`).
- **Hard-coded hosts:** `account.bicore.co.kr`, `m.bicore.co.kr`, `moregame.bicore.co.kr`,
  `pay.bicore.co.kr` and `www.kiwu.net`.
- **Device identifiers:** the binary asks for a device ID and phone number. The Java layer
  returns a fixed fake ID (`866150004500431`) and an empty phone number, so nothing real
  is sent.
- **File operations:** uses `chmod`, `remove`, `rename`, `mkdir` and `mkstemp`, which looks
  like save-file handling but has not been verified.

**Not yet verified**
- What the network code does with server responses.
- The exact arguments to `dlopen` and `dlsym`.
- Which paths the file operations touch.

These need a function-by-function review in Ghidra or IDA.

## Security risk and recommended hardening

The main risk is not the binary doing something on its own, but **someone else controlling
the servers it talks to**. If any of the domains above lapse and are registered by someone
else, the game will trust their responses, and it parses them with 2010-era C code
(`sprintf`, `strcpy`) that may be exploitable.

Recommended steps:

1. Remove `android.permission.INTERNET` from `AndroidManifest.xml`. Android will then refuse
   all network connections from the app, native code included.
2. Remove the other unneeded permissions: `READ_PRIVILEGED_PHONE_STATE`,
   `READ_PHONE_STATE`, `ACCESS_NETWORK_STATE` and `ACCESS_WIFI_STATE`.
3. Remove the dead carrier billing stubs and make the purchase callbacks no-ops.

## Options for the future

- **Emulation host.** Run the unmodified `libdhunter.so` inside a CPU emulator (for example
  Unicorn), and have the host provide the ~100 imported functions, the 32 JNI entry points
  and a GLES 1 translation layer (for example gl4es). This gets past the 32-bit problem on
  any platform, removes the Android dependency, and sandboxes the game: it can only reach
  files or the network through the host, and the host can log every call.
- **Audit.** Review the network, file and `dlopen` code paths in Ghidra and record the
  results here.
- **Rights.** A rebuild from source would only be possible with the source code or the
  rights holder's permission. If Bicore was acquired or dissolved, the current owner may be
  traceable through Korean company records.
