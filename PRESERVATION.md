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
  (see History). The build is offline: it has no network permission (see the security
  notes).

## Asset formats and the asset browser

`tools/asset-browser/index.html` is a local viewer for the assets. Open it in a browser,
then choose the `app/src/main/assets` folder (or drag it onto the page). It decodes the
files in the browser; nothing is uploaded and no assets are stored in the tool.

The game loads assets from `assets/<set>/<letter>/<name>`. `<set>` is `Default` or `HVGA`,
and file names follow `<letter><number><letter>`, for example `I12I`.

| Folder | Contents | Status |
|---|---|---|
| `I` | Images: sprites, UI, effects, portraits, item icons (4,210 per set) | Decoded |
| `A` | Animations: characters, body parts, weapons, effects, portraits (2,756 per set) | Decoded and playable |
| `T` | Data tables: items, skills, quests, monsters, tips, UI text (43 tables, 1,983 rows) | Decoded, all 7 languages |
| `S` | Scripts: events, quests and dialogue (432 per language) | Decoded: dialogue view and readable listing |
| `U` | UI layouts | Text extracted; layout not decoded |
| `B` | Music (`.mp3`) | Plays |
| `W` | Sound effects (`.ogg`) | Plays |
| `M` + `R` | Maps: tile layers, objects, properties; `R` holds names and properties (296 maps) | Decoded and drawn, with object sprites |
| `O` | Object definitions: monsters, NPCs, doors, chests, effects (421 objects, 7 languages) | Decoded |
| `P` | Palettes used to recolour shared sprites (94) | Decoded |
| `F` | Fonts: 12 image fonts (damage numbers and similar) and 3 system-font entries | Decoded |
| `V` | Empty | — |

Folders `O`, `R`, `S`, `T` and `U` exist in seven languages, marked by a suffix on the
file name: none for Korean, then `C` Chinese, `E` English, `F` French, `G` German,
`J` Japanese and `S` Spanish (for example `T0TE` is the English version of `T0T`).
Text is UTF-8.

**Resource indexes.** The game refers to assets by resource ID, not file name. The folder
letters are numbered by the string `BWPIFATOMUSR` (so images are type 3, animations 5,
tables 6). Each folder has an index file `Idx<letter>[suffix]`:

| Field | Size | Notes |
|---|---|---|
| format | u8 | 1 = 32-bit fields (all shipped indexes); 0 = 16-bit offset and size |
| compressed | u8 | 1 = data files are raw LZMA (properties `5d`, 1 MB dictionary); 0 in all shipped files |
| file count | int32 | |
| file sizes | file count × u32 | uncompressed size of each data file |
| entries | 12 bytes each | int32 file number, u32 offset, u32 size |

Resource ID *n* is entry *n* (1-based). Its data is `size` bytes at `offset` in
`<letter><file number><letter>[suffix]`. From `IRS_Open` and `IRS_GetIndexData`.

**Image format (`I` files).** Each file is a series of images back to back, with no file
header. Each image is:

| Field | Size | Notes |
|---|---|---|
| width, height | 2 × int16 LE | |
| flags | u8 | see below |
| palette count | u8 | |
| (extended header) | 8 more bytes | only if `flags & 0x80`; width and height then sit at offsets 10 and 12 |
| palette | count × 2 or 3 bytes | RGB565 LE, or RGB888 if `flags & 0x40` |
| pixels | w × h × 1 or 2 bytes | 8-bit palette indices if `flags & 0x10`, RGB565 if `flags & 0x20` |
| alpha | w × h bytes | only if `flags & 0x02` |

For 8-bit images without an alpha channel, palette index 0 is transparent (the game's
renderer always skips it). The layout comes from the game's `II_GetImagePtr` and
`II_GetImageAlphaPtr` functions, and every image file in both sets parses to its exact
last byte with it.

**Animation format (`A` resources).** From `IANI_Load`, `IFRM_Load` and `IFRM_Draw`. All
integers little-endian.

```
animation:
  u8 nameLength, name bytes
  u8 type                     (0 or 1)
  u32 (not used by the loader beyond reading it)
  u8 flags
  u8 imageCount, imageCount × u32 image IDs     (images to preload)
  u8 frameCount, frameCount × frame
  if flags & 0x20: frameCount × u16 sound reference (a data-table reference, see below)

frame:
  u8 layerCount, layerCount × layer
  u8 extra ticks              (frame lasts extra + 1 ticks; values ≥ 150 count as 1)
  if flags & 0x80: u16
  if flags & 0x40: u16
  u8 count, count × box       (box set 1)
  u8 count, count × box       (box set 2)

layer:
  int16 x, int16 y            (top-left, relative to the animation's origin)
  if flags & 0x10: int16 width, int16 height   (draw size; 0 = image's own size)
  if flags & 0x08: u8 alpha
  if flags & 0x04: u8 orientation
  if flags & 0x02: u8 blend mode
  u32 image ID

box: int16 x1, y1, x2, y2     (inclusive corners)
```

Orientation: 0 none, 1 flip horizontally, 2 flip vertically, 3 rotate 90° clockwise,
4 rotate 90° counter-clockwise, 5 anti-transpose, 6 transpose, 7 rotate 180°. Only 0, 1, 2
and 7 occur in the data. Blend modes (from `ES_DrawTexture`) never occur. The two box sets
look like body and attack boxes but this is not confirmed. Player characters are built
from several animations drawn together (body parts, outfit, weapon); the browser can
stack animations to show this. The tick rate is not confirmed; the browser defaults to
15 ticks per second.

**Data tables (`T` resources).** From `IGD_Load`, `IFS_GTableDeCode` and
`IVO_LoadVariantObj`. Code refers to a table row as `tableID × 256 + row`.

Each resource stores the same table twice (int32 length + bytes, twice). Each copy is a
4-byte seed, then text that is RC4-encrypted and base64-encoded, then one trailing byte.
The RC4 key is the decimal string of a signed 32-bit value whose bytes are, from low to
high: `0x65` if the seed is 0 else `0x64`; `0x72`, `0x6f` and `0x63`, each XORed with 1
if the seed's byte 1, 2 or 3 respectively (counting from the low byte) is non-zero.
Note: the game's RC4 swaps with XOR, which zeroes a value when both indexes are equal;
a standard RC4 gives slightly wrong output. Both copies decode to identical data for every
table in every language and set.

Decoded table: u8 row count, u8 column count, column count × u8 type, row count × int32
row offset, then each row's cells in column order. Cell types: 0, 1, 5 u8; 2 i8; 3 i16;
4 i32; 6, 16 u16; 7 u32; 10 text (u8 length); 11, 14, 15 text (u16 length); 12 palette ID
(u16); 13 image ID (u32); 17 animation list (int16 total, int16 n, n × (u16, u16)).
Column meanings are not stored; table 34 is an empty placeholder.

**Maps (`M` and `R` resources).** From `IMAP_Load`, `IMAP_LoadBaseMap`, `IML_Load` and
`IMAP_Draw`. A map ID opens the same resource ID in `M` (tiles, layers, objects) and `R`
(name and properties, translated).

```
R: name, u16 engine, u16 music, u8, u16 base map ID,
   u8 property count, properties: u8 type, name, value
   (value has a u16 length for types 11, 14, 15, otherwise u8; for a base map, those
   long values are read from M instead)
M: u8 cell format (1 = 16-bit cells, else 8-bit)
   tile list (only for base maps, and maps whose base map ID is 0):
     count (u16 if 16-bit cells, else u8), count × u32 image ID
   u16
   u8 layer count, layers

layer: int16 tile width, tile height, columns, rows
  if columns × rows > 0:
    cells (u8 or u16 each; value v draws tile v − 1 of the tile list, 0 = empty)
    u16 flags per cell (bit 0 mirrors the tile horizontally; other bits do not affect
    drawing, probably collision or terrain)
    u8 marker; if 1, one more byte per cell
  else: u8
  u16 object count, objects:
    type, name, int16 x, int16 y, u8, u8 property count, (u8 index, text) pairs
```

A map with a base map uses the base map's tile list, and the base map's layers are added
as a wider background layer (probably parallax). There are 9 base maps. Object types use
prefixes such as `M_` (monster spawns), `MA_` (areas and exits), `MO_` (doors and other
map objects) and `MB_` (boxes); what each becomes is decided by game code
(`IGM_GenMapGObject`) and is not decoded.

**Object definitions (`O` resources).** From `IGO_Load` and `IEMain_GenMapGObject`. Same
encrypted table format as `T`; an object ID is `table × 256 + row`. Columns 0–10 are
fixed: kind (u8), u8, u16, u8, a box (4 × i8: x1, y1, x2, y2), two palette IDs, and an
animation list of (animation ID, image offset) pairs. The image offset is added to every
image ID in the animation, so one animation can show different images. Later columns
depend on the kind; monsters (kind 10 and others) have their name in column 11.

A map object's type (for example `M_Slem`) names a map property whose value lists object
IDs with spawn chances: `id:chance[:…]-id:chance…`. The browser draws the first object's
first animation. Kinds 100 and 110 snap to the 18-pixel grid; kinds 15 and 20–29 snap
vertically (grid + 4).

Of 3,909 map objects, 3,868 draw a sprite. The other 41 are meant to be invisible, not
failures, and the object list labels each with the reason: breakable terrain already drawn
by the tile layer (20, kind 15 `T_Stone`/`T_GenStone`, whose "Blow,N" property marks how
many hits break the underlying tile), placeholder markers whose type has no spawn entry
(12), bare position markers with no type (7), and action-point triggers (2, kind 27). None
of these have an overlay sprite in game.

**Palettes (`P` resources).** int16 count, then count RGB565 colours. When an object names
two palettes, each image colour found in the first is replaced by the colour at the same
position in the second (`II_ConvertPalette`). This is how monster variants are recoloured.

**Fonts (`F` resources).** From `IFNT_Load`: u8 kind, description, 5 × int16. Kind 1 is an
image font: the first two values are the first and last character code, followed by one
u32 image ID per character (0 = no glyph). Other kinds name a system font
(`System:Plain:Medium` and similar).

**Frame sounds.** An animation's per-frame sound reference is a data-table reference
(`table × 256 + row`, always table 41). Column 0 of that row is a sound-effect resource ID
in `W`, whose index entry gives the `W<n>W.ogg` file (`IRM_GetWave`).

**Scripts (`S` resources).** From `ISRT_Load`, `ISOP_NewScriptOp` and the op classes'
`ReadParam` functions. A small stack machine.

```
int32 item count, int32, then items, each starting with a u8 op:
  0   label            u16-length text
  1   function start   u8 function ID, int16
  25  include script   u16 script ID
  2   PUSHVAR          u8
  3   PUSH             u8 (low 2 bits: scope), int16 index
  4   PUSHCONST        u8 type; 0: u8 bool, 3: int32, 7: u8-length text
  20  JUMPFALSE        int32 step
  21  JUMP             int32 step
  22  SETFLAG          u8
  24  CALL             u8 function number
  23  SAVESSP          (marks where the next call's arguments start)
  5–19                 SET ADD SUB MUL DIV NEG AND OR NOT EQ GT LT GTEQ LTEQ NEQ
  other (only 26 occurs, once per function, probably "end")
```

Strings use a length byte that counts a terminating NUL. `CALL n` runs native function
*n* if the game registers one, otherwise script function *n* (defined with op 1 in the
script or an included one). The native table is filled by `IGL_InitFunTable` and
`IGL_InitCustomFunTable` and read by `IGL_GetFun`; its 123 entries were resolved
statically from the binary (following each global-offset-table slot to its function) and
are listed in the browser as `SCRIPT_NATIVES` (regenerate with `tools/ghidra/script_natives.py`). Examples: 8 `ISND_PlayBGM`, 49/50
`IUIF_GetGProperty`/`IUIF_SetGProperty`, 120 `ShowChoiceMsg`, 131 `AddQuest`. Every `CALL`
in every script resolves to either a native or a script function. Script function 70 (in
shared script 30) is the dialogue routine; its arguments are an object, position,
speaker name, face, text colour, display frame, speech-bubble tail and the line.

All 432 scripts in each of the 7 languages parse exactly; there are about 4,100 dialogue
lines per language.

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

## Audit of the native code (November 2026)

A function-by-function read of the network, file and `dlopen` code paths in Ghidra. The
headline is that **the offline build removes the `INTERNET` permission, so every network
path below is now unreachable** — the audit records what the code would do if it ran, not
a live exposure in the shipped offline build.

**Process and code execution.** No `exec*`, `system`, `popen`, `fork` or `ptrace` imports;
no `mmap`/`mprotect` and no writable-executable memory, so the binary cannot run external
programs or generate code.

**`dlopen` / `dlsym`.** Used in exactly one function, `importGLInit`. It opens
`libGLESv1_CM.so` and resolves a fixed list of about 26 OpenGL ES 1.x entry points
(`glClear`, `glMatrixMode`, `glDrawArrays`, …). No other library name or symbol is passed,
and the names are constant strings in the binary, not taken from data or the network. This
is the ordinary way to bind the GL driver.

**Network.** All socket calls go through `MC_net*` wrappers, then `INET_*`, then the game
loops (`IEU_NetComm_*`, `IEU_N_Main_*`). Replies are read into one fixed buffer (about
15 KB, at a fixed offset in a per-connection structure) and parsed by `INM_ReadInt16` /
`INM_ReadString`, which advance a read cursor by length values taken from the reply. The
cursor is not visibly bounds-checked against the buffer, so a hostile or corrupt server
reply could over-read past the buffer; replies are RC4-decoded first. This is the concrete
risk the security section warns about, and it is the reason the build is offline. Hosts
contacted were the Bicore domains and `www.kiwu.net` (all long gone).

**Files.** All file access goes through `MC_fs*` wrappers (`MC_fsOpen`, `MC_fsRead`,
`MC_fsMkDir`, `MC_fsRemove`, `MC_fsRename`). Normal play reads bundled assets and reads and
writes save and config files in the app's own storage. The one place a path is built from
external input is the patch download loop (`IEU_N_Main_GetPatchDataLoop`), which makes
directories and files from names sent by the server (`INM_ReadString` → `MC_fsMkDir`); the
names are not sanitised in the code read. That path is only reached from the network, so
the offline build does not run it.

**Conclusion.** The binary has no hidden capability beyond what its imports allow, does not
run code or other programs, and loads only the system GL library. Its real weakness is
trusting server replies and server-named paths in 2010-era C. Removing `INTERNET` closes
all of it; nothing here needs the binary itself to be changed.

## Security risk and recommended hardening

The main risk is not the binary doing something on its own, but **someone else controlling
the servers it talks to**. If any of the domains above lapse and are registered by someone
else, the game will trust their responses, and it parses them with 2010-era C code
(`sprintf`, `strcpy`) that may be exploitable.

Hardening done (November 2026):

1. `android.permission.INTERNET` is removed from `AndroidManifest.xml`, so Android refuses
   all network connections from the app, native code included.
2. The other unneeded permissions are removed: `READ_PRIVILEGED_PHONE_STATE`,
   `READ_PHONE_STATE`, `ACCESS_NETWORK_STATE` and `ACCESS_WIFI_STATE`. The app now asks
   only for `WAKE_LOCK` and `VIBRATE`.
3. The carrier billing code (`KTMarket`, `LGUMarket` and the KT, LG U+ and SKT service
   interfaces) is removed. The game's activity now extends `BicoreActivity` directly; a
   purchase request is answered at once with "cancelled" (`nativePuchaseCancel`).
4. The Java layer reports no Wi-Fi and no mobile data to the game, since it cannot
   connect.

This has been built but not yet tested on a device. If a menu waits for the network,
look at what that menu asks the Java layer for.

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
