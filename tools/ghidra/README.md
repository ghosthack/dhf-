# Ghidra helper scripts

Small scripts for reading `libdhunter.so`, two of them with Ghidra in headless mode. They
only read the binary; they do not modify or rebuild anything.

- `Decomp.java`: decompiles the named functions and writes each to `<name>.c` in an output
  folder.
- `Callers.java`: prints which functions call each named function.
- `script_natives.py` (no Ghidra needed): lists the native function behind each script
  `CALL` number by following the stores in `IGL_InitFunTable` and
  `IGL_InitCustomFunTable`. Needs `readelf`, `nm` and `llvm-objdump`.

Requires Ghidra 11.4.2 and JDK 21 (the `dhf-` cloud environment installs Ghidra to
`/opt/ghidra_11.4.2_PUBLIC`).

```sh
GHIDRA=/opt/ghidra_11.4.2_PUBLIC/support/analyzeHeadless
mkdir -p /tmp/ghidra-proj /tmp/ghidra-out

# First run: import and analyse the library (about 30 seconds), then decompile.
$GHIDRA /tmp/ghidra-proj dhf \
  -import app/src/main/libs/armeabi-v7a/libdhunter.so \
  -scriptPath tools/ghidra -postScript Decomp.java /tmp/ghidra-out IANI_Load IFRM_Load

# Later runs reuse the analysed project.
$GHIDRA /tmp/ghidra-proj dhf -process libdhunter.so -noanalysis \
  -scriptPath tools/ghidra -postScript Callers.java IRS_Open IGD_Load
```

`PRESERVATION.md` names the functions each documented file format comes from.
