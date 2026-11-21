//! A minimal JNI environment for the guest.
//!
//! The library was written against Android's `JNIEnv`/`JavaVM` function tables. Here both
//! tables live in guest memory and every slot points at a trap, so each JNI call lands in
//! [`env_call`]. Java objects are host-side values; the guest only ever holds opaque
//! integer handles, validated on every use.

use crate::java;
use crate::machine::{Machine, Trap};
use crate::mem::{Fault, R};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const ENV_SLOTS: u32 = 233;
const VM_SLOTS: u32 = 8;
const JNI_VERSION_1_6: u32 = 0x0001_0006;
const MAX_REFS: usize = 1 << 20;

#[derive(Clone)]
pub enum Obj {
    Class(String),
    Str(String),
    Bytes(Rc<RefCell<Vec<u8>>>),
    /// An instance of a Java-layer class that the host models (`com/bicore/Font`, widgets).
    Instance { class: &'static str, id: u32 },
}

impl Obj {
    pub fn class_name(&self) -> &str {
        match self {
            Obj::Class(_) => "java/lang/Class",
            Obj::Str(_) => "java/lang/String",
            Obj::Bytes(_) => "[B",
            Obj::Instance { class, .. } => class,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum JVal {
    Void,
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// Object handle (0 = null).
    Ref(u32),
}

impl JVal {
    pub fn int(self) -> i32 {
        match self {
            JVal::Int(v) => v,
            JVal::Long(v) => v as i32,
            JVal::Ref(v) => v as i32,
            _ => 0,
        }
    }
}

pub struct Method {
    pub class: String,
    pub name: String,
    pub sig: String,
}

#[derive(Default)]
pub struct Jni {
    pub env: u32,
    pub vm: u32,
    refs: Vec<Option<Obj>>,
    free: Vec<u32>,
    frames: Vec<Vec<u32>>,
    methods: Vec<Method>,
    method_ids: HashMap<(String, String, String), u32>,
    /// Guest copies handed out by Get*ArrayElements: pointer -> (array, length).
    pins: HashMap<u32, (Rc<RefCell<Vec<u8>>>, u32)>,
    /// Guest copies handed out by GetStringUTFChars.
    utf: std::collections::HashSet<u32>,
}

impl Jni {
    pub fn new_ref(&mut self, o: Obj) -> R<u32> {
        let h = self.alloc_ref(o)?;
        if let Some(f) = self.frames.last_mut() {
            f.push(h);
        }
        Ok(h)
    }

    fn alloc_ref(&mut self, o: Obj) -> R<u32> {
        if let Some(i) = self.free.pop() {
            self.refs[i as usize] = Some(o);
            return Ok(i + 1);
        }
        if self.refs.len() >= MAX_REFS {
            return Err(Fault::Other("guest leaked too many JNI references".into()));
        }
        self.refs.push(Some(o));
        Ok(self.refs.len() as u32)
    }

    pub fn get(&self, h: u32) -> R<&Obj> {
        h.checked_sub(1)
            .and_then(|i| self.refs.get(i as usize))
            .and_then(|o| o.as_ref())
            .ok_or_else(|| Fault::Other(format!("guest used an invalid JNI reference ({h:#x})")))
    }

    pub fn opt(&self, h: u32) -> R<Option<&Obj>> {
        if h == 0 { Ok(None) } else { self.get(h).map(Some) }
    }

    pub fn string(&self, h: u32) -> R<String> {
        match self.opt(h)? {
            Some(Obj::Str(s)) => Ok(s.clone()),
            None => Ok(String::new()),
            Some(o) => Err(Fault::Other(format!("expected a String, got {}", o.class_name()))),
        }
    }

    pub fn bytes(&self, h: u32) -> R<Rc<RefCell<Vec<u8>>>> {
        match self.get(h)? {
            Obj::Bytes(b) => Ok(b.clone()),
            o => Err(Fault::Other(format!("expected a byte[], got {}", o.class_name()))),
        }
    }

    fn release(&mut self, h: u32) {
        if let Some(i) = h.checked_sub(1) {
            if self.refs.get(i as usize).is_some_and(|o| o.is_some()) {
                self.refs[i as usize] = None;
                self.free.push(i);
            }
        }
    }

    pub fn push_frame(&mut self) {
        self.frames.push(Vec::new());
    }

    pub fn pop_frame(&mut self) {
        for h in self.frames.pop().unwrap_or_default() {
            self.release(h);
        }
    }

    fn method_id(&mut self, class: &str, name: &str, sig: &str) -> u32 {
        let key = (class.to_string(), name.to_string(), sig.to_string());
        if let Some(&id) = self.method_ids.get(&key) {
            return id;
        }
        if self.methods.len() >= 4096 {
            return 0;
        }
        self.methods.push(Method { class: key.0.clone(), name: key.1.clone(), sig: key.2.clone() });
        let id = self.methods.len() as u32;
        self.method_ids.insert(key, id);
        id
    }
}

/// Build the JNIEnv and JavaVM tables in guest memory.
pub fn install(m: &mut Machine) -> R<()> {
    let env_table = m.galloc(ENV_SLOTS * 4)?;
    for i in 0..ENV_SLOTS {
        let t = m.new_trap(Trap::JniEnv(i));
        m.mem.w32(env_table + 4 * i, t)?;
    }
    let vm_table = m.galloc(VM_SLOTS * 4)?;
    for i in 0..VM_SLOTS {
        let t = m.new_trap(Trap::JavaVm(i));
        m.mem.w32(vm_table + 4 * i, t)?;
    }
    m.jni.env = m.galloc(16)?;
    m.jni.vm = m.galloc(16)?;
    m.mem.w32(m.jni.env, env_table)?;
    m.mem.w32(m.jni.vm, vm_table)
}

pub fn vm_call(m: &mut Machine, idx: u32) -> R<()> {
    match idx {
        // AttachCurrentThread, GetEnv, AttachCurrentThreadAsDaemon: (vm, JNIEnv **out, ...)
        4 | 6 | 7 => {
            m.mem.w32(m.arg(1)?, m.jni.env)?;
            m.ret(0)
        }
        5 => m.ret(0),
        _ => Err(Fault::Unimplemented(format!("JavaVM function #{idx}\n{}", m.state()))),
    }
}

/// How a Call*Method variant passes its Java arguments.
enum ArgSrc {
    Var(u32),
    List(u32),
    Array(u32),
}

fn arg_types(sig: &str) -> R<Vec<u8>> {
    let bad = || Fault::Other(format!("malformed method signature `{sig}`"));
    let inner = sig.strip_prefix('(').and_then(|s| s.split_once(')')).ok_or_else(bad)?.0.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < inner.len() {
        let mut c = inner[i];
        let array = c == b'[';
        while c == b'[' {
            i += 1;
            c = *inner.get(i).ok_or_else(bad)?;
        }
        if c == b'L' {
            i += inner[i..].iter().position(|&b| b == b';').ok_or_else(bad)?;
        }
        out.push(if array { b'L' } else { c });
        i += 1;
    }
    Ok(out)
}

fn read_args(m: &Machine, sig: &str, src: ArgSrc) -> R<Vec<JVal>> {
    let mut out = Vec::new();
    match src {
        ArgSrc::Array(mut p) => {
            for t in arg_types(sig)? {
                let raw = m.mem.r64(p)?;
                p = p.wrapping_add(8);
                out.push(match t {
                    b'J' => JVal::Long(raw as i64),
                    b'D' => JVal::Double(f64::from_bits(raw)),
                    b'F' => JVal::Float(f32::from_bits(raw as u32)),
                    b'L' => JVal::Ref(raw as u32),
                    _ => JVal::Int(raw as i32),
                });
            }
        }
        ArgSrc::Var(idx) | ArgSrc::List(idx) => {
            let mut va = match src {
                ArgSrc::Var(_) => crate::fmt::Va::Call { idx },
                _ => crate::fmt::Va::List { addr: idx },
            };
            for t in arg_types(sig)? {
                out.push(match t {
                    b'J' => JVal::Long(va.u64(m)? as i64),
                    // Floats are promoted to double in C varargs.
                    b'D' => JVal::Double(f64::from_bits(va.u64(m)?)),
                    b'F' => JVal::Float(f64::from_bits(va.u64(m)?) as f32),
                    b'L' => JVal::Ref(va.u32(m)?),
                    _ => JVal::Int(va.u32(m)? as i32),
                });
            }
        }
    }
    Ok(out)
}

fn set_ret(m: &mut Machine, v: JVal) -> R<()> {
    match v {
        JVal::Void => Ok(()),
        JVal::Int(v) => m.ret(v as u32),
        JVal::Ref(v) => m.ret(v),
        JVal::Float(v) => m.ret(v.to_bits()),
        JVal::Long(v) => m.ret64(v as u64),
        JVal::Double(v) => m.ret64(v.to_bits()),
    }
}

/// Dispatch a Call<Type>Method / CallStatic<Type>Method / NewObject variant.
fn invoke(m: &mut Machine, mid_arg: u32, form: u32, is_new: bool) -> R<()> {
    let mid = m.arg(mid_arg)?;
    let (class, name, sig) = {
        let me = mid.checked_sub(1).and_then(|i| m.jni.methods.get(i as usize)).ok_or_else(|| Fault::Other(format!("guest used an invalid method id ({mid:#x})")))?;
        (me.class.clone(), me.name.clone(), me.sig.clone())
    };
    let next = mid_arg + 1;
    let src = match form {
        0 => ArgSrc::Var(next),
        1 => ArgSrc::List(m.arg(next)?),
        _ => ArgSrc::Array(m.arg(next)?),
    };
    let args = read_args(m, &sig, src)?;
    // For instance calls the receiver is argument 1; for static calls and NewObject it is the class.
    let this = m.arg(1)?;
    let _ = is_new;
    if m.opts.trace {
        eprintln!("[trace] java {class}.{name}{sig} {args:?}");
    }
    let v = java::call(m, &class, &name, &sig, this, &args)?;
    set_ret(m, v)
}

pub fn env_call(m: &mut Machine, idx: u32) -> R<()> {
    if m.opts.trace {
        eprintln!("[trace] JNIEnv[{idx}]({:#x}, {:#x}, {:#x})", m.arg(1)?, m.arg(2)?, m.arg(3)?);
    }
    match idx {
        4 => m.ret(JNI_VERSION_1_6),
        6 => {
            let name = m.mem.cstr_lossy(m.arg(1)?)?;
            let h = m.jni.new_ref(Obj::Class(name))?;
            m.ret(h)
        }
        // Exceptions: the host's Java layer never throws.
        15 | 228 => m.ret(0),
        16 | 17 => Ok(()),
        19 | 26 => m.ret(0),
        20 => m.ret(m.arg(1)?),
        // NewGlobalRef / NewLocalRef / NewWeakGlobalRef
        21 | 25 | 226 => {
            let h = m.arg(1)?;
            let r = match m.jni.opt(h)?.cloned() {
                Some(o) if idx == 25 => m.jni.new_ref(o)?,
                Some(o) => m.jni.alloc_ref(o)?,
                None => 0,
            };
            m.ret(r)
        }
        22 | 23 | 227 => {
            let h = m.arg(1)?;
            m.jni.release(h);
            Ok(())
        }
        24 => {
            let (a, b) = (m.arg(1)?, m.arg(2)?);
            let same = a == b
                || match (m.jni.opt(a)?, m.jni.opt(b)?) {
                    (Some(Obj::Instance { class: c1, id: i1 }), Some(Obj::Instance { class: c2, id: i2 })) => c1 == c2 && i1 == i2,
                    (Some(Obj::Bytes(x)), Some(Obj::Bytes(y))) => Rc::ptr_eq(x, y),
                    (Some(Obj::Class(x)), Some(Obj::Class(y))) => x == y,
                    _ => false,
                };
            m.ret(same as u32)
        }
        31 => {
            let name = m.jni.get(m.arg(1)?)?.class_name().to_string();
            let h = m.jni.new_ref(Obj::Class(name))?;
            m.ret(h)
        }
        // GetMethodID / GetStaticMethodID
        33 | 113 => {
            let class = match m.jni.get(m.arg(1)?)? {
                Obj::Class(c) => c.clone(),
                o => return Err(Fault::Other(format!("GetMethodID on a non-class ({})", o.class_name()))),
            };
            let (name, sig) = (m.mem.cstr_lossy(m.arg(2)?)?, m.mem.cstr_lossy(m.arg(3)?)?);
            arg_types(&sig)?;
            let id = m.jni.method_id(&class, &name, &sig);
            m.ret(id)
        }
        28..=30 => invoke(m, 2, idx - 28, true),
        34..=63 => invoke(m, 2, (idx - 34) % 3, false),
        64..=93 => invoke(m, 3, (idx - 64) % 3, false),
        114..=143 => invoke(m, 2, (idx - 114) % 3, false),
        164 => {
            let n = m.jni.string(m.arg(1)?)?.encode_utf16().count();
            m.ret(n as u32)
        }
        167 => {
            let p = m.arg(1)?;
            if p == 0 {
                return m.ret(0);
            }
            let s = m.mem.cstr_lossy(p)?;
            let h = m.jni.new_ref(Obj::Str(s))?;
            m.ret(h)
        }
        168 => {
            let n = m.jni.string(m.arg(1)?)?.len();
            m.ret(n as u32)
        }
        169 => {
            let s = m.jni.string(m.arg(1)?)?;
            let is_copy = m.arg(2)?;
            let p = m.galloc(s.len() as u32 + 1)?;
            m.mem.write(p, s.as_bytes())?;
            if is_copy != 0 {
                m.mem.w8(is_copy, 1)?;
            }
            m.jni.utf.insert(p);
            m.ret(p)
        }
        170 => {
            let p = m.arg(2)?;
            if m.jni.utf.remove(&p) {
                m.heap.free(p);
            }
            Ok(())
        }
        171 => {
            let n = m.jni.bytes(m.arg(1)?)?.borrow().len();
            m.ret(n as u32)
        }
        176 => {
            let n = m.arg(1)?;
            if n > 64 << 20 {
                return m.ret(0);
            }
            let h = m.jni.new_ref(Obj::Bytes(Rc::new(RefCell::new(vec![0; n as usize]))))?;
            m.ret(h)
        }
        // GetByteArrayElements / GetPrimitiveArrayCritical
        184 | 222 => {
            let arr = m.jni.bytes(m.arg(1)?)?;
            let is_copy = m.arg(2)?;
            let len = arr.borrow().len() as u32;
            let p = m.galloc(len.max(1))?;
            m.mem.write(p, &arr.borrow())?;
            // Report "not a copy": like Dalvik, writes through this pointer always reach
            // the array (see the release below).
            if is_copy != 0 {
                m.mem.w8(is_copy, 0)?;
            }
            m.jni.pins.insert(p, (arr, len));
            m.ret(p)
        }
        // ReleaseByteArrayElements / ReleasePrimitiveArrayCritical: (array, elems, mode)
        192 | 223 => {
            let (p, mode) = (m.arg(2)?, m.arg(3)?);
            const JNI_COMMIT: u32 = 1;
            let Some((arr, len)) = m.jni.pins.get(&p).cloned() else {
                return Err(Fault::Other(format!("guest released array elements it never obtained ({p:#x})")));
            };
            // Dalvik handed out the array's own storage, so JNI_ABORT discarded nothing.
            // The game depends on that: FillBuffer draws the frame and then releases the
            // buffer with JNI_ABORT. Copy back in every mode to match.
            arr.borrow_mut().copy_from_slice(m.mem.slice(p, len)?);
            if mode != JNI_COMMIT {
                m.jni.pins.remove(&p);
                m.heap.free(p);
            }
            Ok(())
        }
        // Get/SetByteArrayRegion: (array, start, len, buf)
        200 | 208 => {
            let arr = m.jni.bytes(m.arg(1)?)?;
            let (start, len, buf) = (m.arg(2)? as usize, m.arg(3)? as usize, m.arg(4)?);
            let mut a = arr.borrow_mut();
            let range = start.checked_add(len).filter(|&e| e <= a.len()).map(|e| start..e);
            let Some(range) = range else {
                return Err(Fault::Other("guest byte[] region access out of bounds".into()));
            };
            if idx == 200 {
                m.mem.write(buf, &a[range])?;
            } else {
                a[range].copy_from_slice(m.mem.slice(buf, len as u32)?);
            }
            Ok(())
        }
        219 => {
            m.mem.w32(m.arg(1)?, m.jni.vm)?;
            m.ret(0)
        }
        _ => Err(Fault::Unimplemented(format!("JNIEnv function #{idx}\n{}", m.state()))),
    }
}
