#!/usr/bin/env python3
"""List the native functions behind script CALL numbers in libdhunter.so.

The game fills its script function table in IGL_InitFunTable and
IGL_InitCustomFunTable: each entry is loaded from a global-offset-table slot and
stored at table + 4 * n. This follows those loads and stores in the ARM code and
prints "n name" for each entry. Needs binutils (readelf, nm) and llvm-objdump.

Usage: python3 script_natives.py app/src/main/libs/armeabi-v7a/libdhunter.so
"""
import re, struct, subprocess, sys

SO = sys.argv[1]
data = open(SO, "rb").read()

def sh(*cmd):
    return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout

# Map virtual addresses to file offsets using the LOAD segments.
segs = []
for l in sh("readelf", "-lW", SO).splitlines():
    m = re.match(r"\s*LOAD\s+0x([0-9a-f]+)\s+0x([0-9a-f]+)\s+0x[0-9a-f]+\s+0x([0-9a-f]+)", l)
    if m: segs.append((int(m.group(2), 16), int(m.group(1), 16), int(m.group(3), 16)))
def word(va):
    for v, o, n in segs:
        if v <= va < v + n: return struct.unpack_from("<I", data, o + va - v)[0]
    raise ValueError(hex(va))

# Some functions share an address (identical code was merged); keep every name.
syms = {}
for l in sh("nm", "-D", "--defined-only", SO).splitlines():
    p = l.split()
    if len(p) == 3: syms.setdefault(int(p[0], 16), []).append(p[2])
addr = {n: a for a, ns in syms.items() for n in ns}
syms = {a: "/".join(sorted(set(ns))) for a, ns in syms.items()}
starts = sorted(syms)
def end_of(a): return next(x for x in starts if x > a)

rel = {}
for l in sh("readelf", "-rW", SO).splitlines():
    m = re.match(r"\s*([0-9a-f]{8})\s+[0-9a-f]+\s+(R_ARM_\w+)\s+(?:[0-9a-f]+\s+(\S+))?", l)
    if m: rel[int(m.group(1), 16)] = (m.group(2), m.group(3))
def slot_target(s):
    t = rel.get(s)
    if t and t[1] and t[0] != "R_ARM_RELATIVE": return syms.get(addr.get(t[1], -1), t[1])
    v = word(s)
    return syms.get(v & ~1, hex(v))

NUM = r"(-?0x[0-9a-f]+|-?\d+)"
def stores(fn):
    a = addr[fn]
    out = sh("llvm-objdump", "-d", "--triple=armv7-linux-gnueabi", f"--start-address={a}", f"--stop-address={end_of(a)}", SO)
    R, stack, res = {}, {}, []
    for l in out.splitlines():
        m = re.match(r"\s*([0-9a-f]+):\s+[0-9a-f]{8}\s+(\w+)\s+(.*)", l)
        if not m: continue
        pc, op, x = int(m.group(1), 16), m.group(2), m.group(3).split("@")[0].strip()
        if op == "ldr":
            if (mm := re.match(rf"(\w+), \[pc, #{NUM}\]$", x)): R[mm[1]] = ("c", word(pc + 8 + int(mm[2], 0)))
            elif (mm := re.match(r"(\w+), \[(\w+), (\w+)\]$", x)):
                b, c = R.get(mm[2]), R.get(mm[3])
                R[mm[1]] = ("slot", (b[1] + c[1]) & 0xffffffff) if b and c and b[0] == c[0] == "c" else None
            elif (mm := re.match(rf"(\w+), \[sp(?:, #{NUM})?\]$", x)): R[mm[1]] = stack.get(int(mm[2] or "0", 0))
            else: R[x.split(",")[0]] = None
        elif op == "add" and (mm := re.match(r"(\w+), pc, (\w+)$", x)) and R.get(mm[2]) and R[mm[2]][0] == "c":
            R[mm[1]] = ("c", (pc + 8 + R[mm[2]][1]) & 0xffffffff)
        elif op == "str":
            if (mm := re.match(rf"(\w+), \[sp(?:, #{NUM})?\]$", x)): stack[int(mm[2] or "0", 0)] = R.get(mm[1])
            elif (mm := re.match(rf"(\w+), \[(\w+)(?:, #{NUM})?\]$", x)):
                b, v = R.get(mm[2]), R.get(mm[1])
                if b and v and b[0] == v[0] == "slot": res.append((b[1], int(mm[3] or "0", 0) // 4, slot_target(v[1])))
        elif op == "ldmib" and x.startswith("sp, {"):
            for i, r in enumerate(x[x.index("{") + 1:x.index("}")].split(",")): R[r.strip()] = stack.get(4 * (i + 1))
        elif op == "mov" and (mm := re.match(r"(\w+), (\w+)$", x)): R[mm[1]] = R.get(mm[2])
    return res

table = {}
for slot, n, name in stores("IGL_InitFunTable") + stores("IGL_InitCustomFunTable"):
    table[n] = name
for n in sorted(table): print(n, table[n])
