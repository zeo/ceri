// pure-Rust baseline x86-64 JIT compiler: lowers hot bytecode functions directly to native
// machine code operating on the shared vm_stack, register frame, cached scope chain, and
// monomorphic inline caches, with direct JIT-to-JIT tail jumps and zero-sync leaf helpers

use super::ast::BinaryOperator;
use super::compiler::{Code, Op};
use super::heap::{Entry, Heap};
use super::object::{
    JIT_FLAG_NO_PROTO_IC, JIT_FLAG_ORDINARY, JIT_FLAG_PROXY, JsObject, MONO_MISSING, MONO_OWN_DATA,
    MONO_PROTO_DATA, MonoEntry, PropIc, Properties, SHARED_CAP,
};
use super::vm::{FastNativeCall, RunResult};
use super::*;
use std::mem::offset_of;

pub(super) const JIT_THRESHOLD: u32 = 16;
const MAX_JIT_OPS: usize = 4096;
const SCOPE_CACHE_DEPTH: usize = 6;

// helper return status codes shared between generated machine code and Rust helpers
const STATUS_OK: u32 = 0;
const STATUS_FRAME_CHANGED: u32 = 1;
const STATUS_DONE: u32 = 2;
const STATUS_ERR: u32 = 3;
const STATUS_DISPATCH_NEXT: u32 = 4;

// x86-64 register numbers
const RAX: u8 = 0;
const RCX: u8 = 1;
const RDX: u8 = 2;
const RBX: u8 = 3;
const RSP: u8 = 4;
const RBP: u8 = 5;
const RSI: u8 = 6;
const RDI: u8 = 7;
const R8: u8 = 8;
const R9: u8 = 9;
const R10: u8 = 10;
const R11: u8 = 11;
const R12: u8 = 12;
const R13: u8 = 13;
const R14: u8 = 14;
const R15: u8 = 15;

// pinned callee-saved roles across generated code
const R_RT: u8 = RBX;
const R_HEAP: u8 = R12;
const R_SP: u8 = R13;
const R_REGS: u8 = R14;
const R_CTX: u8 = R15;

const XMM0: u8 = 0;
const XMM1: u8 = 1;

// condition codes for 0F 8x rel32
const CC_AE: u8 = 0x3;
const CC_E: u8 = 0x4;
const CC_NE: u8 = 0x5;
const CC_BE: u8 = 0x6;
const CC_A: u8 = 0x7;
const CC_S: u8 = 0x8;
const CC_P: u8 = 0xA;
const CC_L: u8 = 0xC;

// layout constants checked at compile time and verified by heap::tests
const VAL_SIZE: i32 = 16;
const ENTRY_OBJ_SIZE: usize = std::mem::size_of::<Entry<JsObject>>();
const OFF_ENTRY_BORROW: i32 = offset_of!(Entry<JsObject>, borrow) as i32;
const OFF_ENTRY_VAL: i32 = offset_of!(Entry<JsObject>, value) as i32;
const OFF_OBJ_PROPS_PTR: i32 =
    OFF_ENTRY_VAL + offset_of!(JsObject, properties) as i32 + offset_of!(Properties, ptr) as i32;
const OFF_OBJ_PROPS_CAP: i32 =
    OFF_ENTRY_VAL + offset_of!(JsObject, properties) as i32 + offset_of!(Properties, cap) as i32;
const OFF_OBJ_PROTOTYPE: i32 = OFF_ENTRY_VAL + offset_of!(JsObject, prototype) as i32;
const OFF_OBJ_SHAPE_ID: i32 = OFF_ENTRY_VAL + offset_of!(JsObject, shape_id) as i32;
const OFF_OBJ_JIT_FLAGS: i32 = OFF_ENTRY_VAL + offset_of!(JsObject, jit_flags) as i32;

const MONO_ENTRY_SIZE: i32 = std::mem::size_of::<MonoEntry>() as i32;
const OFF_MONO_SHAPE_ID: i32 = offset_of!(MonoEntry, shape_id) as i32;
const OFF_MONO_PROTO_SHAPE_ID: i32 = offset_of!(MonoEntry, proto_shape_id) as i32;
const OFF_MONO_INDEX: i32 = offset_of!(MonoEntry, index) as i32;
const OFF_MONO_KIND: i32 = offset_of!(MonoEntry, kind) as i32;
const OFF_MONO_WRITABLE: i32 = offset_of!(MonoEntry, writable) as i32;

const OFF_HEAP_ID: i32 = Heap::OFF_ID as i32;
const OFF_HEAP_OBJ_CHUNKS: i32 = Heap::OFF_OBJECTS_CHUNKS as i32;

// stack-allocated scratch area inside the JIT prologue (RSP+0..RSP+32)
const SCRATCH_VAL: i32 = 0;
const SCRATCH_TMP: i32 = 16;

struct ExecBuf {
    ptr: *mut u8,
    len: usize,
}

#[cfg(target_os = "windows")]
mod os_mem {
    const MEM_COMMIT_RESERVE: u32 = 0x3000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_READWRITE: u32 = 0x04;
    const PAGE_EXECUTE_READ: u32 = 0x20;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(addr: *mut u8, size: usize, alloc_type: u32, protect: u32) -> *mut u8;
        fn VirtualProtect(
            addr: *mut u8,
            size: usize,
            new_protect: u32,
            old_protect: *mut u32,
        ) -> i32;
        fn VirtualFree(addr: *mut u8, size: usize, free_type: u32) -> i32;
    }

    pub(super) fn alloc_exec(code: &[u8]) -> Option<(*mut u8, usize)> {
        let len = code.len().max(1);
        // safety: requesting a fresh anonymous RW allocation from the OS
        let ptr = unsafe {
            VirtualAlloc(
                std::ptr::null_mut(),
                len,
                MEM_COMMIT_RESERVE,
                PAGE_READWRITE,
            )
        };
        if ptr.is_null() {
            return None;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, code.len());
            let mut old = 0u32;
            if VirtualProtect(ptr, len, PAGE_EXECUTE_READ, &mut old) == 0 {
                VirtualFree(ptr, 0, MEM_RELEASE);
                return None;
            }
        }
        Some((ptr, len))
    }

    pub(super) unsafe fn free_exec(ptr: *mut u8, _len: usize) {
        unsafe {
            VirtualFree(ptr, 0, MEM_RELEASE);
        }
    }
}

#[cfg(target_os = "linux")]
mod os_mem {
    const PROT_READ: i32 = 1;
    const PROT_WRITE: i32 = 2;
    const PROT_EXEC: i32 = 4;
    const MAP_PRIVATE: i32 = 0x02;
    const MAP_ANONYMOUS: i32 = 0x20;

    unsafe extern "C" {
        fn mmap(
            addr: *mut u8,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: isize,
        ) -> *mut u8;
        fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
        fn munmap(addr: *mut u8, len: usize) -> i32;
    }

    pub(super) fn alloc_exec(code: &[u8]) -> Option<(*mut u8, usize)> {
        let len = code.len().max(1);
        // safety: requesting a fresh anonymous RW mapping from the OS
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr.is_null() || ptr == usize::MAX as *mut u8 {
            return None;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, code.len());
            if mprotect(ptr, len, PROT_READ | PROT_EXEC) != 0 {
                munmap(ptr, len);
                return None;
            }
        }
        Some((ptr, len))
    }

    pub(super) unsafe fn free_exec(ptr: *mut u8, len: usize) {
        unsafe {
            munmap(ptr, len);
        }
    }
}

impl ExecBuf {
    fn from_bytes(code: &[u8]) -> Option<Self> {
        let (ptr, len) = os_mem::alloc_exec(code)?;
        Some(Self { ptr, len })
    }
}

impl Drop for ExecBuf {
    fn drop(&mut self) {
        // safety: ptr/len came from os_mem::alloc_exec and no JIT code is executing on drop
        unsafe { os_mem::free_exec(self.ptr, self.len) };
    }
}

pub(super) struct JitCode {
    buf: ExecBuf,
    pc_offsets: Box<[u32]>,
}

impl std::fmt::Debug for JitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JitCode")
    }
}

impl JitCode {
    #[inline(always)]
    pub(super) fn entry(&self) -> extern "sysv64" fn(*mut JitRun, *const u8) -> u32 {
        // safety: buf.ptr starts with our sysv64 prologue and has RX permissions
        unsafe { std::mem::transmute(self.buf.ptr) }
    }

    #[inline(always)]
    pub(super) fn target(&self, pc: usize) -> *const u8 {
        unsafe {
            self.buf
                .ptr
                .add(*self.pc_offsets.get_unchecked(pc) as usize)
        }
    }
}

#[repr(C)]
pub(super) struct JitCtx {
    pub(super) rt: *mut ScriptRuntime,
    pub(super) heap: *const Heap,
    pub(super) stack_ptr: *mut Value,
    pub(super) stack_safe_limit: *mut Value,
    pub(super) sp: *mut Value,
    pub(super) regs: *mut Value,
    pub(super) next_target: *const u8,
    pub(super) scope_ptrs: [*mut Value; SCOPE_CACHE_DEPTH],
    pub(super) scope_lens: [usize; SCOPE_CACHE_DEPTH],
    pub(super) steps: u64,
    pub(super) safepoint_at: u64,
    pub(super) reg_base: usize,
    pub(super) top: usize,
    pub(super) frame_floor: usize,
    pub(super) max_pushes: usize,
    pub(super) sampling: u8,
}

#[repr(C)]
pub(super) struct JitRun {
    pub(super) ctx: JitCtx,
    pub(super) error: Option<ScriptError>,
    pub(super) result: Option<RunResult>,
    // keeps the returning function's Rc<Code> alive across the tail-jump into its caller
    pub(super) prev_code: Option<Rc<Code>>,
}

const OFF_CTX_RT: i32 = offset_of!(JitCtx, rt) as i32;
const OFF_CTX_HEAP: i32 = offset_of!(JitCtx, heap) as i32;
const OFF_CTX_STACK_SAFE_LIMIT: i32 = offset_of!(JitCtx, stack_safe_limit) as i32;
const OFF_CTX_SP: i32 = offset_of!(JitCtx, sp) as i32;
const OFF_CTX_REGS: i32 = offset_of!(JitCtx, regs) as i32;
const OFF_CTX_NEXT_TARGET: i32 = offset_of!(JitCtx, next_target) as i32;
const OFF_CTX_SCOPE_PTRS: i32 = offset_of!(JitCtx, scope_ptrs) as i32;
const OFF_CTX_SCOPE_LENS: i32 = offset_of!(JitCtx, scope_lens) as i32;
const OFF_CTX_STEPS: i32 = offset_of!(JitCtx, steps) as i32;
const OFF_CTX_SAFEPOINT: i32 = offset_of!(JitCtx, safepoint_at) as i32;
const OFF_CTX_TOP: i32 = offset_of!(JitCtx, top) as i32;

pub(super) enum JitOutcome {
    FrameChanged,
    Done(RunResult),
    Error(ScriptError),
}

struct Asm {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    fixups: Vec<(usize, usize)>,
}

impl Asm {
    fn new(capacity: usize) -> Self {
        Self {
            code: Vec::with_capacity(capacity),
            labels: Vec::new(),
            fixups: Vec::new(),
        }
    }

    fn label(&mut self) -> usize {
        let id = self.labels.len();
        self.labels.push(None);
        id
    }

    fn bind(&mut self, label: usize) {
        self.labels[label] = Some(self.code.len());
    }

    fn patch_labels(&mut self) {
        for &(patch_at, label) in &self.fixups {
            let target = self.labels[label].expect("all labels bound") as isize;
            let rel = (target - (patch_at as isize + 4)) as i32;
            self.code[patch_at..patch_at + 4].copy_from_slice(&rel.to_le_bytes());
        }
    }

    #[inline(always)]
    fn emit(&mut self, byte: u8) {
        self.code.push(byte);
    }

    #[inline(always)]
    fn emit_u32(&mut self, v: u32) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    #[inline(always)]
    fn emit_i32(&mut self, v: i32) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    #[inline(always)]
    fn emit_u64(&mut self, v: u64) {
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    #[inline(always)]
    fn rex(&mut self, w: u8, r: u8, x: u8, b: u8) {
        let byte = 0x40 | (w << 3) | ((r >> 3) << 2) | ((x >> 3) << 1) | (b >> 3);
        self.emit(byte);
    }

    #[inline(always)]
    fn rex_if(&mut self, w: u8, r: u8, x: u8, b: u8) {
        let byte = 0x40 | (w << 3) | ((r >> 3) << 2) | ((x >> 3) << 1) | (b >> 3);
        if byte != 0x40 {
            self.emit(byte);
        }
    }

    #[inline(always)]
    fn modrm(&mut self, mode: u8, reg: u8, rm: u8) {
        self.emit((mode << 6) | ((reg & 7) << 3) | (rm & 7));
    }

    // encodes [base + disp], handling RSP/R12 (SIB) and RBP/R13 (disp8=0) accurately
    fn emit_mem(&mut self, reg: u8, base: u8, disp: i32) {
        let b7 = base & 7;
        let need_sib = b7 == 4;
        if disp == 0 && b7 != 5 {
            self.modrm(0, reg, b7);
            if need_sib {
                self.emit(0x24);
            }
        } else if (-128..=127).contains(&disp) {
            self.modrm(1, reg, b7);
            if need_sib {
                self.emit(0x24);
            }
            self.emit(disp as i8 as u8);
        } else {
            self.modrm(2, reg, b7);
            if need_sib {
                self.emit(0x24);
            }
            self.emit_i32(disp);
        }
    }

    fn emit_mem_sib(&mut self, reg: u8, base: u8, index: u8, scale_shift: u8, disp: i32) {
        debug_assert!((index & 7) != 4);
        let sib = (scale_shift << 6) | ((index & 7) << 3) | (base & 7);
        if disp == 0 && (base & 7) != 5 {
            self.modrm(0, reg, 4);
            self.emit(sib);
        } else if (-128..=127).contains(&disp) {
            self.modrm(1, reg, 4);
            self.emit(sib);
            self.emit(disp as i8 as u8);
        } else {
            self.modrm(2, reg, 4);
            self.emit(sib);
            self.emit_i32(disp);
        }
    }

    fn push(&mut self, r: u8) {
        if r >= 8 {
            self.emit(0x41);
        }
        self.emit(0x50 + (r & 7));
    }

    fn pop(&mut self, r: u8) {
        if r >= 8 {
            self.emit(0x41);
        }
        self.emit(0x58 + (r & 7));
    }

    fn ret(&mut self) {
        self.emit(0xC3);
    }

    fn mov_rr(&mut self, dst: u8, src: u8) {
        self.rex(1, src, 0, dst);
        self.emit(0x89);
        self.modrm(3, src, dst);
    }

    fn mov_ri64(&mut self, dst: u8, imm: u64) {
        self.rex(1, 0, 0, dst);
        self.emit(0xB8 + (dst & 7));
        self.emit_u64(imm);
    }

    fn mov_ri32(&mut self, dst: u8, imm: u32) {
        self.rex_if(0, 0, 0, dst);
        self.emit(0xB8 + (dst & 7));
        self.emit_u32(imm);
    }

    fn mov_r_mem(&mut self, dst: u8, base: u8, disp: i32) {
        self.rex(1, dst, 0, base);
        self.emit(0x8B);
        self.emit_mem(dst, base, disp);
    }

    fn mov_mem_r(&mut self, base: u8, disp: i32, src: u8) {
        self.rex(1, src, 0, base);
        self.emit(0x89);
        self.emit_mem(src, base, disp);
    }

    fn mov_r32_mem(&mut self, dst: u8, base: u8, disp: i32) {
        self.rex_if(0, dst, 0, base);
        self.emit(0x8B);
        self.emit_mem(dst, base, disp);
    }

    fn movzx_r_m8(&mut self, dst: u8, base: u8, disp: i32) {
        self.rex_if(0, dst, 0, base);
        self.emit(0x0F);
        self.emit(0xB6);
        self.emit_mem(dst, base, disp);
    }

    fn movzx_r_m16(&mut self, dst: u8, base: u8, disp: i32) {
        self.rex_if(0, dst, 0, base);
        self.emit(0x0F);
        self.emit(0xB7);
        self.emit_mem(dst, base, disp);
    }

    fn mov_m8_imm(&mut self, base: u8, disp: i32, imm: u8) {
        self.rex_if(0, 0, 0, base);
        self.emit(0xC6);
        self.emit_mem(0, base, disp);
        self.emit(imm);
    }

    fn mov_m8_r(&mut self, base: u8, disp: i32, src: u8) {
        if src >= 4 || base >= 8 {
            self.rex(0, src, 0, base);
        }
        self.emit(0x88);
        self.emit_mem(src, base, disp);
    }

    fn mov_r_sib(&mut self, dst: u8, base: u8, index: u8, scale_shift: u8, disp: i32) {
        self.rex(1, dst, index, base);
        self.emit(0x8B);
        self.emit_mem_sib(dst, base, index, scale_shift, disp);
    }

    fn lea_mem(&mut self, dst: u8, base: u8, disp: i32) {
        self.rex(1, dst, 0, base);
        self.emit(0x8D);
        self.emit_mem(dst, base, disp);
    }

    fn alu_ri(&mut self, ext: u8, r: u8, imm: i32) {
        self.rex(1, 0, 0, r);
        if (-128..=127).contains(&imm) {
            self.emit(0x83);
            self.modrm(3, ext, r);
            self.emit(imm as i8 as u8);
        } else {
            self.emit(0x81);
            self.modrm(3, ext, r);
            self.emit_i32(imm);
        }
    }

    fn add_ri(&mut self, r: u8, imm: i32) {
        self.alu_ri(0, r, imm);
    }

    fn sub_ri(&mut self, r: u8, imm: i32) {
        self.alu_ri(5, r, imm);
    }

    fn add_rr(&mut self, dst: u8, src: u8) {
        self.rex(1, src, 0, dst);
        self.emit(0x01);
        self.modrm(3, src, dst);
    }

    fn and_r32_imm(&mut self, r: u8, imm: i32) {
        self.rex_if(0, 0, 0, r);
        self.emit(0x81);
        self.modrm(3, 4, r);
        self.emit_i32(imm);
    }

    fn and_r32_rr(&mut self, dst: u8, src: u8) {
        self.rex_if(0, src, 0, dst);
        self.emit(0x21);
        self.modrm(3, src, dst);
    }

    fn or_r32_rr(&mut self, dst: u8, src: u8) {
        self.rex_if(0, src, 0, dst);
        self.emit(0x09);
        self.modrm(3, src, dst);
    }

    fn xor_r32_rr(&mut self, dst: u8, src: u8) {
        self.rex_if(0, src, 0, dst);
        self.emit(0x31);
        self.modrm(3, src, dst);
    }

    fn shl_ri(&mut self, r: u8, n: u8) {
        self.rex(1, 0, 0, r);
        self.emit(0xC1);
        self.modrm(3, 4, r);
        self.emit(n);
    }

    fn shr_ri(&mut self, r: u8, n: u8) {
        self.rex(1, 0, 0, r);
        self.emit(0xC1);
        self.modrm(3, 5, r);
        self.emit(n);
    }

    fn shr_r32_ri(&mut self, r: u8, n: u8) {
        self.rex_if(0, 0, 0, r);
        self.emit(0xC1);
        self.modrm(3, 5, r);
        self.emit(n);
    }

    fn shl_r32_cl(&mut self, r: u8) {
        self.rex_if(0, 0, 0, r);
        self.emit(0xD3);
        self.modrm(3, 4, r);
    }

    fn sar_r32_cl(&mut self, r: u8) {
        self.rex_if(0, 0, 0, r);
        self.emit(0xD3);
        self.modrm(3, 7, r);
    }

    fn shr_r32_cl(&mut self, r: u8) {
        self.rex_if(0, 0, 0, r);
        self.emit(0xD3);
        self.modrm(3, 5, r);
    }

    fn add_m64_imm(&mut self, base: u8, disp: i32, imm: i32) {
        self.rex(1, 0, 0, base);
        if (-128..=127).contains(&imm) {
            self.emit(0x83);
            self.emit_mem(0, base, disp);
            self.emit(imm as i8 as u8);
        } else {
            self.emit(0x81);
            self.emit_mem(0, base, disp);
            self.emit_i32(imm);
        }
    }

    fn sub_m64_imm(&mut self, base: u8, disp: i32, imm: i32) {
        self.rex(1, 0, 0, base);
        if (-128..=127).contains(&imm) {
            self.emit(0x83);
            self.emit_mem(5, base, disp);
            self.emit(imm as i8 as u8);
        } else {
            self.emit(0x81);
            self.emit_mem(5, base, disp);
            self.emit_i32(imm);
        }
    }

    fn cmp_r32_r32(&mut self, a: u8, b: u8) {
        self.rex_if(0, b, 0, a);
        self.emit(0x39);
        self.modrm(3, b, a);
    }

    fn cmp_r8_imm(&mut self, r: u8, imm: u8) {
        if r >= 4 {
            self.rex(0, 0, 0, r);
        }
        self.emit(0x80);
        self.modrm(3, 7, r);
        self.emit(imm);
    }

    fn cmp_r32_imm(&mut self, r: u8, imm: i32) {
        self.rex_if(0, 0, 0, r);
        self.emit(0x81);
        self.modrm(3, 7, r);
        self.emit_i32(imm);
    }

    fn cmp_r16_mem(&mut self, r: u8, base: u8, disp: i32) {
        self.emit(0x66);
        self.rex_if(0, r, 0, base);
        self.emit(0x3B);
        self.emit_mem(r, base, disp);
    }

    fn cmp_r32_mem(&mut self, r: u8, base: u8, disp: i32) {
        self.rex_if(0, r, 0, base);
        self.emit(0x3B);
        self.emit_mem(r, base, disp);
    }

    fn cmp_r_mem(&mut self, r: u8, base: u8, disp: i32) {
        self.rex(1, r, 0, base);
        self.emit(0x3B);
        self.emit_mem(r, base, disp);
    }

    fn cmp_m8_imm(&mut self, base: u8, disp: i32, imm: u8) {
        self.rex_if(0, 0, 0, base);
        self.emit(0x80);
        self.emit_mem(7, base, disp);
        self.emit(imm);
    }

    fn cmp_m32_imm(&mut self, base: u8, disp: i32, imm: i32) {
        self.rex_if(0, 0, 0, base);
        self.emit(0x81);
        self.emit_mem(7, base, disp);
        self.emit_i32(imm);
    }

    fn cmp_m64_imm(&mut self, base: u8, disp: i32, imm: i32) {
        self.rex(1, 0, 0, base);
        if (-128..=127).contains(&imm) {
            self.emit(0x83);
            self.emit_mem(7, base, disp);
            self.emit(imm as i8 as u8);
        } else {
            self.emit(0x81);
            self.emit_mem(7, base, disp);
            self.emit_i32(imm);
        }
    }

    fn test_rr(&mut self, a: u8, b: u8) {
        self.rex(1, b, 0, a);
        self.emit(0x85);
        self.modrm(3, b, a);
    }

    fn test_r32(&mut self, a: u8, b: u8) {
        self.rex_if(0, b, 0, a);
        self.emit(0x85);
        self.modrm(3, b, a);
    }

    fn test_m8_imm(&mut self, base: u8, disp: i32, imm: u8) {
        self.rex_if(0, 0, 0, base);
        self.emit(0xF6);
        self.emit_mem(0, base, disp);
        self.emit(imm);
    }

    fn xor_m8_imm(&mut self, base: u8, disp: i32, imm: u8) {
        self.rex_if(0, 0, 0, base);
        self.emit(0x80);
        self.emit_mem(6, base, disp);
        self.emit(imm);
    }

    fn movdqu_load(&mut self, xmm: u8, base: u8, disp: i32) {
        self.emit(0xF3);
        self.rex_if(0, xmm, 0, base);
        self.emit(0x0F);
        self.emit(0x6F);
        self.emit_mem(xmm, base, disp);
    }

    fn movdqu_store(&mut self, base: u8, disp: i32, xmm: u8) {
        self.emit(0xF3);
        self.rex_if(0, xmm, 0, base);
        self.emit(0x0F);
        self.emit(0x7F);
        self.emit_mem(xmm, base, disp);
    }

    fn movsd_load(&mut self, xmm: u8, base: u8, disp: i32) {
        self.emit(0xF2);
        self.rex_if(0, xmm, 0, base);
        self.emit(0x0F);
        self.emit(0x10);
        self.emit_mem(xmm, base, disp);
    }

    fn movsd_store(&mut self, base: u8, disp: i32, xmm: u8) {
        self.emit(0xF2);
        self.rex_if(0, xmm, 0, base);
        self.emit(0x0F);
        self.emit(0x11);
        self.emit_mem(xmm, base, disp);
    }

    fn sse_sd(&mut self, op: u8, dst: u8, src: u8) {
        self.emit(0xF2);
        self.rex_if(0, dst, 0, src);
        self.emit(0x0F);
        self.emit(op);
        self.modrm(3, dst, src);
    }

    fn addsd(&mut self, dst: u8, src: u8) {
        self.sse_sd(0x58, dst, src);
    }

    fn subsd(&mut self, dst: u8, src: u8) {
        self.sse_sd(0x5C, dst, src);
    }

    fn mulsd(&mut self, dst: u8, src: u8) {
        self.sse_sd(0x59, dst, src);
    }

    fn divsd(&mut self, dst: u8, src: u8) {
        self.sse_sd(0x5E, dst, src);
    }

    fn ucomisd(&mut self, a: u8, b: u8) {
        self.emit(0x66);
        self.rex_if(0, a, 0, b);
        self.emit(0x0F);
        self.emit(0x2E);
        self.modrm(3, a, b);
    }

    fn xorpd(&mut self, a: u8, b: u8) {
        self.emit(0x66);
        self.rex_if(0, a, 0, b);
        self.emit(0x0F);
        self.emit(0x57);
        self.modrm(3, a, b);
    }

    fn cvttsd2si_r32(&mut self, dst: u8, xmm: u8) {
        self.emit(0xF2);
        self.rex_if(0, dst, 0, xmm);
        self.emit(0x0F);
        self.emit(0x2C);
        self.modrm(3, dst, xmm);
    }

    fn cvtsi2sd_r32(&mut self, xmm: u8, src: u8) {
        self.emit(0xF2);
        self.rex_if(0, xmm, 0, src);
        self.emit(0x0F);
        self.emit(0x2A);
        self.modrm(3, xmm, src);
    }

    fn cvtsi2sd_r64(&mut self, xmm: u8, src: u8) {
        self.emit(0xF2);
        self.rex(1, xmm, 0, src);
        self.emit(0x0F);
        self.emit(0x2A);
        self.modrm(3, xmm, src);
    }

    fn movq_xmm_r(&mut self, xmm: u8, r: u8) {
        self.emit(0x66);
        self.rex(1, xmm, 0, r);
        self.emit(0x0F);
        self.emit(0x6E);
        self.modrm(3, xmm, r);
    }

    fn jmp(&mut self, label: usize) {
        self.emit(0xE9);
        let at = self.code.len();
        self.emit_i32(0);
        self.fixups.push((at, label));
    }

    fn jcc(&mut self, cc: u8, label: usize) {
        self.emit(0x0F);
        self.emit(0x80 + cc);
        let at = self.code.len();
        self.emit_i32(0);
        self.fixups.push((at, label));
    }

    fn jmp_r(&mut self, r: u8) {
        self.rex_if(0, 0, 0, r);
        self.emit(0xFF);
        self.modrm(3, 4, r);
    }

    fn call_r(&mut self, r: u8) {
        self.rex_if(0, 0, 0, r);
        self.emit(0xFF);
        self.modrm(3, 2, r);
    }

    fn call_abs(&mut self, addr: usize) {
        self.mov_ri64(RAX, addr as u64);
        self.call_r(RAX);
    }
}

#[inline(always)]
unsafe fn sync_from_jit(run: *mut JitRun) -> &'static mut ScriptRuntime {
    unsafe {
        let ctx = &mut (*run).ctx;
        let rt = &mut *ctx.rt;
        let new_len = (ctx.sp as usize - ctx.stack_ptr as usize) / std::mem::size_of::<Value>();
        rt.vm_stack.set_len(new_len);
        rt.steps = ctx.steps;
        rt
    }
}

#[inline(always)]
unsafe fn sync_to_jit(run: *mut JitRun, rt: &mut ScriptRuntime, refresh_scopes: bool) {
    unsafe {
        let ctx = &mut (*run).ctx;
        let cap = rt.vm_stack.capacity();
        let len = rt.vm_stack.len();
        if cap - len < ctx.max_pushes {
            rt.vm_stack.reserve(ctx.max_pushes);
        }
        let base = rt.vm_stack.as_mut_ptr();
        let cap = rt.vm_stack.capacity();
        ctx.stack_ptr = base;
        ctx.stack_safe_limit = base.add(cap.saturating_sub(ctx.max_pushes));
        ctx.sp = base.add(rt.vm_stack.len());
        ctx.regs = base.add(ctx.reg_base);
        ctx.steps = rt.steps;
        if refresh_scopes {
            refresh_scope_cache(ctx, rt);
        }
    }
}

#[inline(always)]
pub(super) unsafe fn refresh_scope_cache(ctx: &mut JitCtx, rt: &ScriptRuntime) {
    let frame = unsafe { rt.frames.get_unchecked(ctx.top) };
    let limit = (frame.code.max_scope_depth as usize).min(SCOPE_CACHE_DEPTH);
    if limit == 0 {
        return;
    }
    let heap = unsafe { &*ctx.heap };
    let mut cur = frame.scope.as_ref();
    let mut depth = 0;
    while depth < limit {
        let Some(scope_ref) = cur else {
            break;
        };
        let Some(scope) = (unsafe { heap.scope_at_mut(scope_ref, 0) }) else {
            break;
        };
        if scope.layout.grows {
            unsafe {
                *ctx.scope_ptrs.get_unchecked_mut(depth) = std::ptr::null_mut();
                *ctx.scope_lens.get_unchecked_mut(depth) = 0;
            }
        } else {
            unsafe {
                *ctx.scope_ptrs.get_unchecked_mut(depth) = scope.slots.as_mut_ptr();
                *ctx.scope_lens.get_unchecked_mut(depth) = scope.slots.len();
            }
        }
        cur = scope.parent.as_ref();
        depth += 1;
    }
    // zero only remaining active slots directly (avoiding glibc memset call overhead)
    while depth < limit {
        unsafe {
            std::ptr::write(ctx.scope_ptrs.as_mut_ptr().add(depth), std::ptr::null_mut());
            std::ptr::write(ctx.scope_lens.as_mut_ptr().add(depth), 0);
        }
        depth += 1;
    }
}

// transitions ctx to the current top frame and returns STATUS_DISPATCH_NEXT if compiled
#[inline(always)]
unsafe fn try_dispatch_top(run: *mut JitRun, rt: &mut ScriptRuntime) -> u32 {
    unsafe {
        let new_top = rt.frames.len() - 1;
        let frame = rt.frames.get_unchecked(new_top);
        let code_ref: &Code = &frame.code;
        let jit_opt = if let Some(cached) = code_ref.jit_code.get() {
            cached.as_ref()
        } else {
            let hits = code_ref.jit_hits.get().wrapping_add(1);
            code_ref.jit_hits.set(hits);
            if hits >= JIT_THRESHOLD {
                let compiled = compile(code_ref);
                let _ = code_ref.jit_code.set(compiled);
                code_ref.jit_code.get().and_then(Option::as_ref)
            } else {
                None
            }
        };
        let Some(jit) = jit_opt else {
            return STATUS_FRAME_CHANGED;
        };
        let pc = frame.pc;
        let reg_base = frame.reg_base;
        let max_pushes = code_ref.max_pushes as usize;
        let target = jit.target(pc);
        let ctx = &mut (*run).ctx;
        ctx.top = new_top;
        ctx.reg_base = reg_base;
        ctx.max_pushes = max_pushes;
        ctx.next_target = target;
        sync_to_jit(run, rt, true);
        STATUS_DISPATCH_NEXT
    }
}

// leaf helpers never run js, reallocate vm_stack, or trigger gc
extern "sysv64" fn helper_clone_in_place(ptr: *mut Value) {
    unsafe {
        let copy = (*ptr).clone();
        std::ptr::write(ptr, copy);
    }
}

extern "sysv64" fn helper_drop_in_place(ptr: *mut Value) {
    unsafe {
        drop_value_slow(std::ptr::read(ptr));
    }
}

extern "sysv64" fn helper_truthy_slow(ptr: *const Value) -> u32 {
    unsafe { u32::from((*ptr).truthy()) }
}

extern "sysv64" fn helper_read_binding(slot_ptr: *const Value, out: *mut Value) -> u32 {
    unsafe {
        match &*slot_ptr {
            Value::Binding(cell) | Value::ConstBinding(cell) => {
                let inner = cell.borrow();
                if matches!(*inner, Value::Uninitialized) {
                    return 0;
                }
                std::ptr::write(out, inner.clone());
                1
            }
            _ => 0,
        }
    }
}

extern "sysv64" fn helper_ic_get_leaf_replace(
    rt: *const ScriptRuntime,
    heap: *const Heap,
    ic: *const PropIc,
    slot: *mut Value,
) -> u32 {
    unsafe {
        let Some(val) = (*rt).jit_ic_get_value_only(&*heap, &*slot, &*ic) else {
            return 0;
        };
        let old = std::mem::replace(&mut *slot, val);
        old.fast_drop();
        1
    }
}

extern "sysv64" fn helper_ic_get_leaf_push(
    rt: *const ScriptRuntime,
    heap: *const Heap,
    ic: *const PropIc,
    recv: *const Value,
    out: *mut Value,
) -> u32 {
    unsafe {
        let Some(val) = (*rt).jit_ic_get_value_only(&*heap, &*recv, &*ic) else {
            return 0;
        };
        std::ptr::write(out, val);
        1
    }
}

extern "sysv64" fn helper_ic_get_leaf_method(
    rt: *const ScriptRuntime,
    heap: *const Heap,
    ic: *const PropIc,
    recv: *const Value,
    out: *mut Value,
) -> u32 {
    unsafe {
        let Some(val) = (*rt).jit_ic_get_value_only(&*heap, &*recv, &*ic) else {
            return 0;
        };
        std::ptr::write(out, (*recv).clone());
        std::ptr::write(out.add(1), val);
        1
    }
}

extern "sysv64" fn helper_ic_set_leaf(
    heap: *const Heap,
    ic: *const PropIc,
    obj_ptr: *mut Value,
    val_ptr: *mut Value,
    pop: u32,
) -> u32 {
    unsafe {
        let Value::Object(handle) = &*obj_ptr else {
            return 0;
        };
        let heap = &*heap;
        let Some(target) = heap.object_mut(handle) else {
            return 0;
        };
        if pop != 0 {
            let val = std::ptr::read(val_ptr);
            match target.ic_set_fast(handle, heap, &*ic, val) {
                Ok(()) => 1,
                Err(val) => {
                    std::ptr::write(val_ptr, val);
                    0
                }
            }
        } else {
            let val = (*val_ptr).clone();
            if target.ic_set_fast(handle, heap, &*ic, val).is_ok() {
                // obj_ptr holds Copy Value::Object; replace with value from val_ptr
                std::ptr::write(obj_ptr, std::ptr::read(val_ptr));
                1
            } else {
                0
            }
        }
    }
}

extern "sysv64" fn helper_get_index_leaf(
    heap: *const Heap,
    arr_ptr: *const Value,
    idx_ptr: *const Value,
    out_ptr: *mut Value,
    replace_two: u32,
) -> u32 {
    unsafe {
        let Some(val) = ScriptRuntime::jit_try_fast_get_index(&*heap, &*arr_ptr, &*idx_ptr) else {
            return 0;
        };
        if replace_two != 0 {
            std::ptr::read(idx_ptr).fast_drop();
            let old = std::mem::replace(&mut *out_ptr, val);
            old.fast_drop();
        } else {
            std::ptr::write(out_ptr, val);
        }
        1
    }
}

extern "sysv64" fn helper_set_index_leaf(
    heap: *const Heap,
    obj_ptr: *mut Value,
    key_ptr: *mut Value,
    val_ptr: *mut Value,
    pop_all: u32,
) -> u32 {
    unsafe {
        if pop_all != 0 {
            let val = std::ptr::read(val_ptr);
            match ScriptRuntime::jit_try_fast_set_index(&*heap, &*obj_ptr, &*key_ptr, val) {
                Ok(()) => {
                    std::ptr::read(key_ptr).fast_drop();
                    std::ptr::read(obj_ptr).fast_drop();
                    1
                }
                Err(val) => {
                    std::ptr::write(val_ptr, val);
                    0
                }
            }
        } else {
            let val = (*val_ptr).clone();
            if ScriptRuntime::jit_try_fast_set_index(&*heap, &*obj_ptr, &*key_ptr, val).is_ok() {
                std::ptr::read(key_ptr).fast_drop();
                let old = std::mem::replace(&mut *obj_ptr, std::ptr::read(val_ptr));
                old.fast_drop();
                1
            } else {
                0
            }
        }
    }
}

extern "sysv64" fn helper_new_object(rt: *const ScriptRuntime, heap: *const Heap, out: *mut Value) {
    unsafe {
        let obj = (*rt).jit_new_object(&*heap);
        std::ptr::write(out, obj);
    }
}

extern "sysv64" fn helper_make_object(
    rt: *const ScriptRuntime,
    heap: *const Heap,
    code: *const Code,
    template: u32,
    src_ptr: *mut Value,
    out_ptr: *mut Value,
) {
    unsafe {
        let obj = (*rt).jit_make_object(&*heap, &*code, template, src_ptr);
        std::ptr::write(out_ptr, obj);
    }
}

extern "sysv64" fn helper_new_array(heap: *const Heap, out: *mut Value) {
    unsafe {
        let array = (*heap).alloc_array_fast(ArrayData::default());
        std::ptr::write(out, Value::Array(array));
    }
}

extern "sysv64" fn helper_make_array(
    heap: *const Heap,
    src_ptr: *const Value,
    count: u32,
    out: *mut Value,
) {
    unsafe {
        let count = count as usize;
        let mut values = Vec::with_capacity(count);
        for i in 0..count {
            values.push(std::ptr::read(src_ptr.add(i)));
        }
        let array = (*heap).alloc_array_fast(ArrayData::from(values));
        std::ptr::write(out, Value::Array(array));
    }
}

extern "sysv64" fn helper_array_push(
    heap: *const Heap,
    arr_ptr: *const Value,
    val_ptr: *const Value,
) {
    unsafe {
        let val = std::ptr::read(val_ptr);
        if let Value::Array(array) = &*arr_ptr {
            if let Some(data) = (*heap).array_mut(array) {
                data.values.push(val);
            } else {
                array.borrow_mut().push(val);
            }
        } else {
            val.fast_drop();
        }
    }
}

extern "sysv64" fn helper_define_field(
    heap: *const Heap,
    code: *const Code,
    name: u32,
    obj_ptr: *const Value,
    val_ptr: *const Value,
) {
    unsafe {
        let val = std::ptr::read(val_ptr);
        if let Value::Object(object) = &*obj_ptr {
            let key: &str = (&*code).names.get_unchecked(name as usize);
            if let Some(target) = (*heap).object_mut(object) {
                target.define_key(key, super::object::Property::data(val));
            } else {
                object
                    .borrow_mut()
                    .define_key(key, super::object::Property::data(val));
            }
        } else {
            val.fast_drop();
        }
    }
}

extern "sysv64" fn helper_closure(
    rt: *mut ScriptRuntime,
    top: usize,
    child_idx: u32,
    out: *mut Value,
) {
    unsafe {
        let val = (*rt).jit_make_closure(top, child_idx);
        std::ptr::write(out, val);
    }
}

extern "sysv64" fn helper_get_global_leaf(
    rt: *const ScriptRuntime,
    code: *const Code,
    name: u32,
    out: *mut Value,
) -> u32 {
    unsafe {
        let key: &str = (&*code).names.get_unchecked(name as usize);
        let Some(val) = (*rt).jit_get_global_fast(key) else {
            return 0;
        };
        std::ptr::write(out, val);
        1
    }
}

extern "sysv64" fn helper_this_leaf(rt: *const ScriptRuntime, top: usize, out: *mut Value) -> u32 {
    unsafe {
        let frame = (&*rt).frames.get_unchecked(top);
        if matches!(frame.this, Value::Uninitialized) {
            return 0;
        }
        std::ptr::write(out, frame.this.clone());
        1
    }
}

extern "sysv64" fn helper_typeof_leaf(rt: *const ScriptRuntime, slot: *mut Value) {
    unsafe {
        let val = std::ptr::read(slot);
        let res = (*rt).jit_typeof(&val);
        val.fast_drop();
        std::ptr::write(slot, res);
    }
}

extern "sysv64" fn helper_push_handler(
    rt: *mut ScriptRuntime,
    top: usize,
    sp_offset: usize,
    catch: u32,
    finally: u32,
) {
    unsafe {
        (*rt).jit_push_handler(top, sp_offset, catch, finally);
    }
}

extern "sysv64" fn helper_pop_handler(rt: *mut ScriptRuntime, top: usize) {
    unsafe {
        (*rt).jit_pop_handler(top);
    }
}

extern "sysv64" fn helper_completion_normal(rt: *mut ScriptRuntime) {
    unsafe {
        (*rt).jit_completion_normal();
    }
}

extern "sysv64" fn helper_chain_start(rt: *mut ScriptRuntime, top: usize, sp_offset: usize) {
    unsafe {
        (*rt).jit_chain_start(top, sp_offset);
    }
}

extern "sysv64" fn helper_chain_end(rt: *mut ScriptRuntime, top: usize) {
    unsafe {
        (*rt).jit_chain_end(top);
    }
}

extern "sysv64" fn helper_args_iter(rt: *mut ScriptRuntime, slot: *mut Value) {
    unsafe {
        (*rt).jit_args_iter(&mut *slot);
    }
}

extern "sysv64" fn helper_get_iter_leaf(
    rt: *mut ScriptRuntime,
    heap: *const Heap,
    slot: *mut Value,
) -> u32 {
    unsafe { u32::from((*rt).jit_get_iter_fast(&*heap, &mut *slot)) }
}

extern "sysv64" fn helper_iter_value_leaf(
    rt: *mut ScriptRuntime,
    heap: *const Heap,
    iter_slot: *const Value,
    out: *mut Value,
) -> u32 {
    unsafe {
        let Some(val) = (*rt).jit_iter_value_fast(&*heap, &*iter_slot) else {
            return 0;
        };
        std::ptr::write(out, val);
        1
    }
}

extern "sysv64" fn helper_iter_step_leaf(
    rt: *mut ScriptRuntime,
    heap: *const Heap,
    iter_slot: *const Value,
    out: *mut Value,
) -> u32 {
    // 0 = slow path, 1 = pushed value, 2 = done (jump to target)
    unsafe {
        match (*rt).jit_iter_step_fast(&*heap, &*iter_slot) {
            None => 0,
            Some(Some(val)) => {
                std::ptr::write(out, val);
                1
            }
            Some(None) => 2,
        }
    }
}

extern "sysv64" fn helper_iter_close_leaf(rt: *mut ScriptRuntime, slot: *const Value) -> u32 {
    unsafe {
        if let Value::ArrayIter(iter_slot) = &*slot {
            (*rt).jit_iter_close_array(*iter_slot);
            1
        } else {
            0
        }
    }
}

extern "sysv64" fn helper_iter_rest_leaf(
    rt: *mut ScriptRuntime,
    heap: *const Heap,
    iter_slot: *const Value,
    out: *mut Value,
) -> u32 {
    unsafe {
        let Some(val) = (*rt).jit_iter_rest_fast(&*heap, &*iter_slot) else {
            return 0;
        };
        std::ptr::write(out, val);
        1
    }
}

extern "sysv64" fn helper_str_eq(a: *const Value, b: *const Value) -> u32 {
    unsafe {
        if let (Value::String(sa), Value::String(sb)) = (&*a, &*b) {
            u32::from(sa == sb)
        } else {
            0
        }
    }
}

extern "sysv64" fn helper_str_concat(
    l: *const Value,
    r: *const Value,
    out: *mut Value,
    drop_inputs: u32,
) {
    unsafe {
        let (Value::String(sl), Value::String(sr)) = (&*l, &*r) else {
            unreachable!();
        };
        let res = if sl.is_empty() {
            sr.clone()
        } else if sr.is_empty() {
            sl.clone()
        } else {
            let mut combined = String::with_capacity(sl.len() + sr.len());
            combined.push_str(sl);
            combined.push_str(sr);
            JsStr::from(combined)
        };
        if drop_inputs != 0 {
            std::ptr::read(r).fast_drop();
            std::ptr::read(l).fast_drop();
        }
        std::ptr::write(out, Value::String(res));
    }
}

extern "sysv64" fn helper_template_concat(src: *mut Value, count: u32, out: *mut Value) -> u32 {
    unsafe {
        let n = count as usize;
        let slice = std::slice::from_raw_parts(src, n);
        let mut cap = 0usize;
        for v in slice {
            match v {
                Value::String(s) => cap += s.len(),
                Value::Number(_) => cap += 16,
                Value::Bool(_) => cap += 5,
                Value::Null => cap += 4,
                Value::Undefined => cap += 9,
                _ => return 0,
            }
        }
        let mut text = String::with_capacity(cap);
        for v in slice {
            match v {
                Value::String(s) => text.push_str(s),
                Value::Number(num) => text.push_str(&number_to_string(*num)),
                Value::Bool(b) => text.push_str(if *b { "true" } else { "false" }),
                Value::Null => text.push_str("null"),
                Value::Undefined => text.push_str("undefined"),
                _ => unreachable!(),
            }
        }
        for i in 0..n {
            let p = src.add(i);
            if !(*p).is_copy() {
                drop_value_slow(std::ptr::read(p));
            }
        }
        std::ptr::write(out, Value::String(JsStr::from(text)));
        1
    }
}

extern "sysv64" fn helper_push_scope(run: *mut JitRun, block: u32, is_var: u32) {
    unsafe {
        let ctx = &mut (*run).ctx;
        let rt = &mut *ctx.rt;
        let heap = &*ctx.heap;
        let (ptr, len) = rt.jit_push_scope(heap, ctx.top, block, is_var != 0);
        for i in (1..SCOPE_CACHE_DEPTH).rev() {
            ctx.scope_ptrs[i] = ctx.scope_ptrs[i - 1];
            ctx.scope_lens[i] = ctx.scope_lens[i - 1];
        }
        ctx.scope_ptrs[0] = ptr;
        ctx.scope_lens[0] = len;
    }
}

extern "sysv64" fn helper_pop_scope(run: *mut JitRun) {
    unsafe {
        let ctx = &mut (*run).ctx;
        let rt = &mut *ctx.rt;
        let heap = &*ctx.heap;
        rt.jit_pop_scope(heap, ctx.top);
        for i in 0..SCOPE_CACHE_DEPTH - 1 {
            ctx.scope_ptrs[i] = ctx.scope_ptrs[i + 1];
            ctx.scope_lens[i] = ctx.scope_lens[i + 1];
        }
        if !ctx.scope_ptrs[SCOPE_CACHE_DEPTH - 2].is_null() {
            let frame = rt.frames.get_unchecked(ctx.top);
            if let Some(start) = frame.scope.as_ref()
                && let Some(scope) = heap.scope_at_mut(start, (SCOPE_CACHE_DEPTH - 1) as u16)
                && !scope.layout.grows
            {
                ctx.scope_ptrs[SCOPE_CACHE_DEPTH - 1] = scope.slots.as_mut_ptr();
                ctx.scope_lens[SCOPE_CACHE_DEPTH - 1] = scope.slots.len();
                return;
            }
        }
        ctx.scope_ptrs[SCOPE_CACHE_DEPTH - 1] = std::ptr::null_mut();
        ctx.scope_lens[SCOPE_CACHE_DEPTH - 1] = 0;
    }
}

extern "sysv64" fn helper_clone_scope(run: *mut JitRun) {
    unsafe {
        let ctx = &mut (*run).ctx;
        let rt = &mut *ctx.rt;
        let heap = &*ctx.heap;
        let (ptr, len) = rt.jit_clone_scope(heap, ctx.top);
        ctx.scope_ptrs[0] = ptr;
        ctx.scope_lens[0] = len;
    }
}

extern "sysv64" fn helper_uninitialize(ctx: *mut JitCtx, index: u32, count: u32) -> u32 {
    unsafe {
        let ptr = (*ctx).scope_ptrs[0];
        if ptr.is_null() {
            return 0;
        }
        let len = (*ctx).scope_lens[0];
        let start = index as usize;
        let end = (start + count as usize).min(len);
        for i in start..end {
            let slot = ptr.add(i);
            if !(*slot).is_copy() {
                drop_value_slow(std::ptr::read(slot));
            }
            std::ptr::write(slot, Value::Uninitialized);
        }
        1
    }
}

extern "sysv64" fn helper_end_finally_normal(rt: *mut ScriptRuntime, top: usize) -> u32 {
    unsafe { u32::from((*rt).jit_end_finally_normal(top)) }
}

extern "sysv64" fn helper_safepoint(run: *mut JitRun, pc: usize) -> u32 {
    unsafe {
        let rt = sync_from_jit(run);
        let sampling = (*run).ctx.sampling != 0;
        match rt.jit_hit_safepoint(sampling, pc) {
            Ok(next_target) => {
                (*run).ctx.safepoint_at = next_target;
                sync_to_jit(run, rt, false);
                STATUS_OK
            }
            Err(err) => {
                (*run).error = Some(err);
                STATUS_ERR
            }
        }
    }
}

extern "sysv64" fn helper_reserve_headroom(run: *mut JitRun) -> u32 {
    unsafe {
        let rt = sync_from_jit(run);
        sync_to_jit(run, rt, false);
        STATUS_OK
    }
}

extern "sysv64" fn helper_tdz_reg(
    run: *mut JitRun,
    name_or_reg: u32,
    from_layout: u32,
    next_pc: usize,
) -> u32 {
    unsafe {
        let rt = sync_from_jit(run);
        let top = (*run).ctx.top;
        rt.frames.get_unchecked_mut(top).pc = next_pc;
        let name = if from_layout != 0 {
            rt.frames.get_unchecked(top).code.layout.names[name_or_reg as usize].clone()
        } else {
            rt.frames.get_unchecked(top).code.names[name_or_reg as usize].clone()
        };
        (*run).error =
            Some(rt.reference_error(format!("cannot access {name} before initialization")));
        STATUS_ERR
    }
}

extern "sysv64" fn helper_require_coercible_err(run: *mut JitRun, next_pc: usize) -> u32 {
    unsafe {
        let rt = sync_from_jit(run);
        let top = (*run).ctx.top;
        rt.frames.get_unchecked_mut(top).pc = next_pc;
        (*run).error = Some(rt.type_error("cannot destructure null or undefined"));
        STATUS_ERR
    }
}

extern "sysv64" fn helper_step_op(run: *mut JitRun, op_pc: usize, refresh_scopes: u32) -> u32 {
    unsafe {
        let rt = sync_from_jit(run);
        let top = (*run).ctx.top;
        let frame_floor = (*run).ctx.frame_floor;
        let heap = &*(*run).ctx.heap;
        let op = *rt.frames.get_unchecked(top).code.ops.get_unchecked(op_pc);
        if matches!(op, Op::EndFinally) {
            // keep returning function's executable page alive if EndFinally finishes the frame
            (*run).prev_code = Some(rt.frames.get_unchecked(top).code.clone());
        }
        let next_pc = op_pc + 1;
        rt.frames.get_unchecked_mut(top).pc = next_pc;
        let frames_before = rt.frames.len();
        match rt.jit_exec_op(
            heap,
            op,
            op_pc,
            next_pc,
            frame_floor,
            &mut (*run).ctx.safepoint_at,
            (*run).ctx.sampling != 0,
        ) {
            Ok(Some(res)) => {
                (*run).result = Some(res);
                STATUS_DONE
            }
            Ok(None) => {
                if rt.frames.len() != frames_before || rt.frames.get_unchecked(top).pc != next_pc {
                    try_dispatch_top(run, rt)
                } else {
                    sync_to_jit(run, rt, refresh_scopes != 0);
                    STATUS_OK
                }
            }
            Err(err) => {
                (*run).error = Some(err);
                STATUS_ERR
            }
        }
    }
}

extern "sysv64" fn helper_call(run: *mut JitRun, count: u32, method: u32, next_pc: usize) -> u32 {
    unsafe {
        let rt = sync_from_jit(run);
        let top = (*run).ctx.top;
        rt.frames.get_unchecked_mut(top).pc = next_pc;
        let heap = &*(*run).ctx.heap;
        let method = method != 0;
        if rt.jit_call_plain(heap, count as usize, method) {
            return try_dispatch_top(run, rt);
        }
        match rt.jit_call_fast_native(heap, count as usize, method) {
            Ok(FastNativeCall::Done) => {
                sync_to_jit(run, rt, false);
                STATUS_OK
            }
            Ok(FastNativeCall::FramePushed) => try_dispatch_top(run, rt),
            Ok(FastNativeCall::FallbackMethod(new_count)) => {
                let before = rt.frames.len();
                match rt.jit_call_in_loop_stack(new_count, true) {
                    Ok(()) if rt.frames.len() == before => {
                        sync_to_jit(run, rt, false);
                        STATUS_OK
                    }
                    Ok(()) => try_dispatch_top(run, rt),
                    Err(err) => {
                        (*run).error = Some(err);
                        STATUS_ERR
                    }
                }
            }
            Ok(FastNativeCall::NotHandled) => {
                let before = rt.frames.len();
                match rt.jit_call_in_loop_stack(count as usize, method) {
                    Ok(()) if rt.frames.len() == before => {
                        sync_to_jit(run, rt, false);
                        STATUS_OK
                    }
                    Ok(()) => try_dispatch_top(run, rt),
                    Err(err) => {
                        (*run).error = Some(err);
                        STATUS_ERR
                    }
                }
            }
            Err(err) => {
                (*run).error = Some(err);
                STATUS_ERR
            }
        }
    }
}

extern "sysv64" fn helper_return(run: *mut JitRun, next_pc: usize) -> u32 {
    unsafe {
        let rt = sync_from_jit(run);
        let top = (*run).ctx.top;
        let frame_floor = (*run).ctx.frame_floor;
        let heap = &*(*run).ctx.heap;
        rt.frames.get_unchecked_mut(top).pc = next_pc;
        match rt.jit_return(heap, top, frame_floor) {
            Ok((Some(res), finished_code)) => {
                if let Some(code) = finished_code {
                    (*run).prev_code = Some(code);
                }
                (*run).result = Some(res);
                STATUS_DONE
            }
            Ok((None, finished_code)) => {
                if let Some(code) = finished_code {
                    // keep returning function's executable page alive across tail-jump into caller
                    (*run).prev_code = Some(code);
                }
                try_dispatch_top(run, rt)
            }
            Err(err) => {
                (*run).error = Some(err);
                STATUS_ERR
            }
        }
    }
}

extern "sysv64" fn helper_get_prop_slow(
    run: *mut JitRun,
    op_pc: usize,
    name: u32,
    ic: u32,
    mode: u32,
) -> u32 {
    unsafe {
        let rt = sync_from_jit(run);
        let top = (*run).ctx.top;
        let next_pc = op_pc + 1;
        let heap = &*(*run).ctx.heap;
        let cache = &*rt
            .frames
            .get_unchecked(top)
            .code
            .ics
            .as_ptr()
            .add(ic as usize);
        let frames_before = rt.frames.len();
        let res = rt.jit_get_prop_ic_or_slow(
            heap,
            top,
            op_pc,
            next_pc,
            name,
            ic,
            cache,
            mode,
            &mut (*run).ctx.safepoint_at,
            (*run).ctx.sampling != 0,
        );
        match res {
            Ok(()) => {
                if rt.frames.len() != frames_before {
                    try_dispatch_top(run, rt)
                } else {
                    sync_to_jit(run, rt, false);
                    STATUS_OK
                }
            }
            Err(err) => {
                (*run).error = Some(err);
                STATUS_ERR
            }
        }
    }
}

extern "sysv64" fn helper_set_prop_slow(
    run: *mut JitRun,
    next_pc: usize,
    name: u32,
    ic: u32,
    pop: u32,
) -> u32 {
    unsafe {
        let rt = sync_from_jit(run);
        let top = (*run).ctx.top;
        let heap = &*(*run).ctx.heap;
        let cache = &*rt
            .frames
            .get_unchecked(top)
            .code
            .ics
            .as_ptr()
            .add(ic as usize);
        rt.frames.get_unchecked_mut(top).pc = next_pc;
        match rt.jit_set_prop_ic_or_slow(heap, name, ic, cache, pop != 0) {
            Ok(()) => {
                sync_to_jit(run, rt, false);
                STATUS_OK
            }
            Err(err) => {
                (*run).error = Some(err);
                STATUS_ERR
            }
        }
    }
}

pub(super) fn can_compile(code: &Code) -> bool {
    !code.is_generator
        && !code.is_async
        && code.self_scope.is_none()
        && !code.ops.is_empty()
        && code.ops.len() <= MAX_JIT_OPS
}

pub(super) fn compile(code: &Code) -> Option<JitCode> {
    if !can_compile(code) {
        return None;
    }
    let mut c = Compiler::new(code);
    c.emit_all();
    c.finish()
}

struct Compiler<'a> {
    code: &'a Code,
    asm: Asm,
    pc_labels: Vec<usize>,
    epilogue: usize,
    dispatch_next: usize,
    stubs: Vec<Stub>,
    block_start: usize,
}

enum Stub {
    StepOp {
        label: usize,
        op_pc: usize,
        refresh_scopes: bool,
        cont: usize,
    },
    TdzReg {
        label: usize,
        name_or_reg: u32,
        from_layout: u32,
        next_pc: usize,
    },
    RequireCoercibleErr {
        label: usize,
        next_pc: usize,
    },
    Safepoint {
        label: usize,
        pc: usize,
        cont: usize,
    },
    ReserveHeadroom {
        label: usize,
        cont: usize,
    },
    GetPropSlow {
        label: usize,
        op_pc: usize,
        name: u32,
        ic: u32,
        mode: u32,
        ic_ptr: *const PropIc,
        recv_reg_or_local: Option<(bool, u16, u16)>, // (is_reg, depth_or_0, slot)
        cont: usize,
    },
    SetPropSlow {
        label: usize,
        next_pc: usize,
        name: u32,
        ic: u32,
        pop: u32,
        ic_ptr: *const PropIc,
        cont: usize,
    },
}

impl<'a> Compiler<'a> {
    fn new(code: &'a Code) -> Self {
        let mut asm = Asm::new(code.ops.len() * 32 + 192);
        let mut pc_labels = Vec::with_capacity(code.ops.len() + 1);
        for _ in 0..=code.ops.len() {
            pc_labels.push(asm.label());
        }
        let epilogue = asm.label();
        let dispatch_next = asm.label();
        Self {
            code,
            asm,
            pc_labels,
            epilogue,
            dispatch_next,
            stubs: Vec::new(),
            block_start: 0,
        }
    }

    fn emit_prologue(&mut self) {
        self.asm.push(RBP);
        self.asm.mov_rr(RBP, RSP);
        self.asm.push(RBX);
        self.asm.push(R12);
        self.asm.push(R13);
        self.asm.push(R14);
        self.asm.push(R15);
        self.asm.sub_ri(RSP, 40);
        self.asm.mov_rr(R_CTX, RDI);
        self.asm.mov_r_mem(R_RT, R_CTX, OFF_CTX_RT);
        self.asm.mov_r_mem(R_HEAP, R_CTX, OFF_CTX_HEAP);
        self.asm.mov_r_mem(R_SP, R_CTX, OFF_CTX_SP);
        self.asm.mov_r_mem(R_REGS, R_CTX, OFF_CTX_REGS);
        self.asm.jmp_r(RSI);
    }

    fn emit_epilogue(&mut self) {
        // direct JIT-to-JIT tail jump: reloads sp/regs and jumps to next_target
        self.asm.bind(self.dispatch_next);
        self.asm.mov_r_mem(R_SP, R_CTX, OFF_CTX_SP);
        self.asm.mov_r_mem(R_REGS, R_CTX, OFF_CTX_REGS);
        self.asm.mov_r_mem(RAX, R_CTX, OFF_CTX_NEXT_TARGET);
        self.asm.jmp_r(RAX);

        self.asm.bind(self.epilogue);
        self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
        self.asm.add_ri(RSP, 40);
        self.asm.pop(R15);
        self.asm.pop(R14);
        self.asm.pop(R13);
        self.asm.pop(R12);
        self.asm.pop(RBX);
        self.asm.pop(RBP);
        self.asm.ret();
    }

    // checks status in EAX after a full-sync helper: 0 -> reload & continue, 4 -> tail-jump, else -> epilogue
    fn emit_check_helper_status(&mut self) {
        let cont = self.asm.label();
        self.asm.test_r32(RAX, RAX);
        self.asm.jcc(CC_E, cont);
        self.asm.cmp_r32_imm(RAX, STATUS_DISPATCH_NEXT as i32);
        self.asm.jcc(CC_E, self.dispatch_next);
        self.asm.jmp(self.epilogue);
        self.asm.bind(cont);
        self.asm.mov_r_mem(R_SP, R_CTX, OFF_CTX_SP);
        self.asm.mov_r_mem(R_REGS, R_CTX, OFF_CTX_REGS);
    }

    // loads cached scope slots pointer for depth < SCOPE_CACHE_DEPTH into dst_reg, or jumps to miss
    fn emit_load_scope_ptr(&mut self, depth: u16, index: u16, dst: u8, miss: usize) {
        let ptr_off = OFF_CTX_SCOPE_PTRS + (depth as i32) * 8;
        let len_off = OFF_CTX_SCOPE_LENS + (depth as i32) * 8;
        self.asm.mov_r_mem(dst, R_CTX, ptr_off);
        self.asm.test_rr(dst, dst);
        self.asm.jcc(CC_E, miss);
        self.asm.mov_ri32(RDX, index as u32);
        self.asm.cmp_r_mem(RDX, R_CTX, len_off);
        self.asm.jcc(CC_AE, miss);
    }

    // clones a Value already copied into [base + disp]; tag must already be in AL
    fn emit_clone_value(&mut self, base: u8, disp: i32) {
        let done = self.asm.label();
        let slow = self.asm.label();
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_ARRAY_ITER);
        self.asm.jcc(CC_BE, done);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_STRING);
        self.asm.jcc(CC_NE, slow);
        self.asm.mov_r_mem(RCX, base, disp + 8);
        self.asm.add_m64_imm(RCX, 0, 1);
        self.asm.jmp(done);
        self.asm.bind(slow);
        self.asm.lea_mem(RDI, base, disp);
        self.asm
            .call_abs(helper_clone_in_place as *const () as usize);
        self.asm.bind(done);
    }

    // drops a Value located at [base + disp]; tag must already be in AL
    fn emit_drop_value(&mut self, base: u8, disp: i32) {
        let done = self.asm.label();
        let slow = self.asm.label();
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_ARRAY_ITER);
        self.asm.jcc(CC_BE, done);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_STRING);
        self.asm.jcc(CC_NE, slow);
        self.asm.mov_r_mem(RCX, base, disp + 8);
        self.asm.sub_m64_imm(RCX, 0, 1);
        self.asm.jcc(CC_NE, done);
        self.asm.add_m64_imm(RCX, 0, 1);
        self.asm.bind(slow);
        self.asm.lea_mem(RDI, base, disp);
        self.asm
            .call_abs(helper_drop_in_place as *const () as usize);
        self.asm.bind(done);
    }

    fn emit_truthy_branch(&mut self, base: u8, disp: i32, if_true: usize, if_false: usize) {
        let check_num = self.asm.label();
        let check_str = self.asm.label();
        let check_other = self.asm.label();
        self.asm.movzx_r_m8(RAX, base, disp);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNDEFINED);
        self.asm.jcc(CC_E, if_false);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_NULL);
        self.asm.jcc(CC_E, if_false);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_BOOL);
        self.asm.jcc(CC_NE, check_num);
        self.asm.cmp_m8_imm(base, disp + 8, 0);
        self.asm.jcc(CC_NE, if_true);
        self.asm.jmp(if_false);

        self.asm.bind(check_num);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_NUMBER);
        self.asm.jcc(CC_NE, check_str);
        self.asm.movsd_load(XMM0, base, disp + 8);
        self.asm.xorpd(XMM1, XMM1);
        self.asm.ucomisd(XMM0, XMM1);
        self.asm.jcc(CC_P, if_false);
        self.asm.jcc(CC_E, if_false);
        self.asm.jmp(if_true);

        self.asm.bind(check_str);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_STRING);
        self.asm.jcc(CC_NE, check_other);
        self.asm.mov_r_mem(RCX, base, disp + 8);
        self.asm.cmp_m64_imm(RCX, 8, 0);
        self.asm.jcc(CC_NE, if_true);
        self.asm.jmp(if_false);

        self.asm.bind(check_other);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_OBJECT);
        self.asm.jcc(CC_E, if_true);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_ARRAY);
        self.asm.jcc(CC_E, if_true);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_ELEMENT);
        self.asm.jcc(CC_E, if_true);
        self.asm.lea_mem(RDI, base, disp);
        self.asm.call_abs(helper_truthy_slow as *const () as usize);
        self.asm.test_r32(RAX, RAX);
        self.asm.jcc(CC_NE, if_true);
        self.asm.jmp(if_false);
    }

    fn emit_object_entry_lookup(&mut self, require_mut: bool, miss: usize) {
        self.asm.mov_rr(RAX, RCX);
        self.asm.shr_ri(RAX, 48);
        self.asm.cmp_r16_mem(RAX, R_HEAP, OFF_HEAP_ID);
        self.asm.jcc(CC_NE, miss);
        self.asm.mov_rr(RAX, RCX);
        self.asm.and_r32_imm(RAX, 0x00FF_FFFF);
        self.asm.mov_rr(RDX, RAX);
        self.asm.shr_r32_ri(RDX, 10);
        self.asm.and_r32_imm(RAX, 0x3FF);
        self.asm.mov_r_mem(R8, R_HEAP, OFF_HEAP_OBJ_CHUNKS);
        self.asm.mov_r_sib(R8, R8, RDX, 3, 0);
        const { assert!(ENTRY_OBJ_SIZE == 64) };
        self.asm.shl_ri(RAX, 6);
        self.asm.add_rr(R8, RAX);
        self.asm.cmp_m32_imm(R8, OFF_ENTRY_BORROW, 0);
        if require_mut {
            self.asm.jcc(CC_NE, miss);
        } else {
            self.asm.jcc(CC_L, miss);
        }
    }

    // overwrites [dst_base + dst_disp] with [src_base + src_disp], skipping scratch spill and
    // drop when the overwritten value is Copy; old tag must already be in AL
    fn emit_overwrite_slot(
        &mut self,
        dst_base: u8,
        dst_disp: i32,
        src_base: u8,
        src_disp: i32,
        clone_src: bool,
    ) {
        let non_copy = self.asm.label();
        let done = self.asm.label();
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_ARRAY_ITER);
        self.asm.jcc(CC_A, non_copy);
        self.asm.movdqu_load(XMM1, src_base, src_disp);
        self.asm.movdqu_store(dst_base, dst_disp, XMM1);
        if clone_src {
            self.asm.movzx_r_m8(RAX, dst_base, dst_disp);
            self.emit_clone_value(dst_base, dst_disp);
        }
        self.asm.jmp(done);

        self.asm.bind(non_copy);
        self.asm.movdqu_load(XMM0, dst_base, dst_disp);
        self.asm.movdqu_store(RSP, SCRATCH_VAL, XMM0);
        self.asm.movdqu_load(XMM1, src_base, src_disp);
        self.asm.movdqu_store(dst_base, dst_disp, XMM1);
        if clone_src {
            self.asm.movzx_r_m8(RAX, dst_base, dst_disp);
            self.emit_clone_value(dst_base, dst_disp);
        }
        self.asm.movzx_r_m8(RAX, RSP, SCRATCH_VAL);
        self.emit_drop_value(RSP, SCRATCH_VAL);
        self.asm.bind(done);
    }

    fn emit_mono_ic_get(
        &mut self,
        ic_ptr: *const PropIc,
        recv_base: u8,
        recv_disp: i32,
        out_base: u8,
        out_disp: i32,
        miss: usize,
    ) {
        let matched = self.asm.label();
        let check_proto = self.asm.label();
        let check_missing = self.asm.label();
        let missing_proto = self.asm.label();
        let emit_undef = self.asm.label();
        let done = self.asm.label();

        self.asm.cmp_m8_imm(recv_base, recv_disp, VALUE_TAG_OBJECT);
        self.asm.jcc(CC_NE, miss);
        self.asm.mov_r_mem(RCX, recv_base, recv_disp + 8);
        self.emit_object_entry_lookup(false, miss);
        self.asm.mov_r32_mem(RAX, R8, OFF_OBJ_SHAPE_ID);
        self.asm.test_r32(RAX, RAX);
        self.asm.jcc(CC_E, miss);
        self.asm.mov_ri64(R9, ic_ptr as u64);
        // check e0 then e1
        self.asm.cmp_r32_mem(RAX, R9, OFF_MONO_SHAPE_ID);
        self.asm.jcc(CC_E, matched);
        self.asm.add_ri(R9, MONO_ENTRY_SIZE);
        self.asm.cmp_r32_mem(RAX, R9, OFF_MONO_SHAPE_ID);
        self.asm.jcc(CC_NE, miss);

        self.asm.bind(matched);
        self.asm.cmp_m8_imm(R9, OFF_MONO_KIND, MONO_OWN_DATA);
        self.asm.jcc(CC_NE, check_proto);
        self.asm.test_m8_imm(R8, OFF_OBJ_JIT_FLAGS, JIT_FLAG_PROXY);
        self.asm.jcc(CC_NE, miss);
        self.asm.movzx_r_m16(RDX, R9, OFF_MONO_INDEX);
        self.asm.shl_ri(RDX, 4);
        self.asm.mov_r_mem(R10, R8, OFF_OBJ_PROPS_PTR);
        self.asm.add_rr(R10, RDX);
        self.asm.movzx_r_m8(RAX, R10, 0);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_BINDING);
        self.asm.jcc(CC_E, miss);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_CONST_BINDING);
        self.asm.jcc(CC_E, miss);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_COUNT);
        self.asm.jcc(CC_AE, miss);
        self.asm.movdqu_load(XMM0, R10, 0);
        self.asm.movdqu_store(out_base, out_disp, XMM0);
        self.emit_clone_value(out_base, out_disp);
        self.asm.jmp(done);

        // 1-level prototype data property hit
        self.asm.bind(check_proto);
        self.asm
            .test_m8_imm(R8, OFF_OBJ_JIT_FLAGS, JIT_FLAG_PROXY | JIT_FLAG_NO_PROTO_IC);
        self.asm.jcc(CC_NE, miss);
        self.asm.cmp_m8_imm(R9, OFF_MONO_KIND, MONO_PROTO_DATA);
        self.asm.jcc(CC_NE, check_missing);
        self.asm.mov_r_mem(RCX, R8, OFF_OBJ_PROTOTYPE);
        self.asm.test_rr(RCX, RCX);
        self.asm.jcc(CC_E, miss);
        self.emit_object_entry_lookup(false, miss);
        self.asm.mov_r32_mem(RAX, R9, OFF_MONO_PROTO_SHAPE_ID);
        self.asm.cmp_r32_mem(RAX, R8, OFF_OBJ_SHAPE_ID);
        self.asm.jcc(CC_NE, miss);
        self.asm.test_m8_imm(R8, OFF_OBJ_JIT_FLAGS, JIT_FLAG_PROXY);
        self.asm.jcc(CC_NE, miss);
        self.asm.movzx_r_m16(RDX, R9, OFF_MONO_INDEX);
        self.asm.shl_ri(RDX, 4);
        self.asm.mov_r_mem(R10, R8, OFF_OBJ_PROPS_PTR);
        self.asm.add_rr(R10, RDX);
        self.asm.movzx_r_m8(RAX, R10, 0);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_BINDING);
        self.asm.jcc(CC_E, miss);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_CONST_BINDING);
        self.asm.jcc(CC_E, miss);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_COUNT);
        self.asm.jcc(CC_AE, miss);
        self.asm.movdqu_load(XMM0, R10, 0);
        self.asm.movdqu_store(out_base, out_disp, XMM0);
        self.emit_clone_value(out_base, out_disp);
        self.asm.jmp(done);

        // 0-level or 1-level missing property hit -> Value::Undefined
        self.asm.bind(check_missing);
        self.asm.cmp_m8_imm(R9, OFF_MONO_KIND, MONO_MISSING);
        self.asm.jcc(CC_NE, miss);
        self.asm.mov_r32_mem(RAX, R9, OFF_MONO_PROTO_SHAPE_ID);
        self.asm.mov_r_mem(RCX, R8, OFF_OBJ_PROTOTYPE);
        self.asm.test_r32(RAX, RAX);
        self.asm.jcc(CC_NE, missing_proto);
        self.asm.test_rr(RCX, RCX);
        self.asm.jcc(CC_NE, miss);
        self.asm.jmp(emit_undef);

        self.asm.bind(missing_proto);
        self.asm.test_rr(RCX, RCX);
        self.asm.jcc(CC_E, miss);
        self.emit_object_entry_lookup(false, miss);
        self.asm.mov_r32_mem(RAX, R9, OFF_MONO_PROTO_SHAPE_ID);
        self.asm.cmp_r32_mem(RAX, R8, OFF_OBJ_SHAPE_ID);
        self.asm.jcc(CC_NE, miss);
        self.asm
            .test_m8_imm(R8, OFF_OBJ_JIT_FLAGS, JIT_FLAG_PROXY | JIT_FLAG_NO_PROTO_IC);
        self.asm.jcc(CC_NE, miss);
        self.asm.cmp_m64_imm(R8, OFF_OBJ_PROTOTYPE, 0);
        self.asm.jcc(CC_NE, miss);

        self.asm.bind(emit_undef);
        self.asm.mov_m8_imm(out_base, out_disp, VALUE_TAG_UNDEFINED);
        self.asm.bind(done);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_mono_ic_set(
        &mut self,
        ic_ptr: *const PropIc,
        obj_base: u8,
        obj_disp: i32,
        val_base: u8,
        val_disp: i32,
        pop_both: bool,
        miss: usize,
    ) {
        let matched = self.asm.label();
        self.asm.cmp_m8_imm(obj_base, obj_disp, VALUE_TAG_OBJECT);
        self.asm.jcc(CC_NE, miss);
        self.asm.mov_r_mem(RCX, obj_base, obj_disp + 8);
        self.emit_object_entry_lookup(true, miss);
        self.asm
            .test_m8_imm(R8, OFF_OBJ_JIT_FLAGS, JIT_FLAG_ORDINARY);
        self.asm.jcc(CC_E, miss);
        self.asm
            .cmp_m32_imm(R8, OFF_OBJ_PROPS_CAP, SHARED_CAP as i32);
        self.asm.jcc(CC_E, miss);
        self.asm.mov_r32_mem(RAX, R8, OFF_OBJ_SHAPE_ID);
        self.asm.test_r32(RAX, RAX);
        self.asm.jcc(CC_E, miss);
        self.asm.mov_ri64(R9, ic_ptr as u64);
        self.asm.cmp_r32_mem(RAX, R9, OFF_MONO_SHAPE_ID);
        self.asm.jcc(CC_E, matched);
        self.asm.add_ri(R9, MONO_ENTRY_SIZE);
        self.asm.cmp_r32_mem(RAX, R9, OFF_MONO_SHAPE_ID);
        self.asm.jcc(CC_NE, miss);

        self.asm.bind(matched);
        self.asm.cmp_m8_imm(R9, OFF_MONO_KIND, MONO_OWN_DATA);
        self.asm.jcc(CC_NE, miss);
        self.asm.cmp_m8_imm(R9, OFF_MONO_WRITABLE, 1);
        self.asm.jcc(CC_NE, miss);
        self.asm.movzx_r_m16(RDX, R9, OFF_MONO_INDEX);
        self.asm.shl_ri(RDX, 4);
        self.asm.mov_r_mem(R10, R8, OFF_OBJ_PROPS_PTR);
        self.asm.add_rr(R10, RDX);
        self.asm.movzx_r_m8(RAX, R10, 0);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_BINDING);
        self.asm.jcc(CC_E, miss);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_CONST_BINDING);
        self.asm.jcc(CC_E, miss);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_COUNT);
        self.asm.jcc(CC_AE, miss);
        let non_copy = self.asm.label();
        let done = self.asm.label();
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_ARRAY_ITER);
        self.asm.jcc(CC_A, non_copy);
        self.asm.movdqu_load(XMM1, val_base, val_disp);
        self.asm.movdqu_store(R10, 0, XMM1);
        if pop_both {
            self.asm.sub_ri(R_SP, 2 * VAL_SIZE);
        } else {
            self.asm.movdqu_store(obj_base, obj_disp, XMM1);
            self.asm.movzx_r_m8(RAX, obj_base, obj_disp);
            self.emit_clone_value(obj_base, obj_disp);
            self.asm.sub_ri(R_SP, VAL_SIZE);
        }
        self.asm.jmp(done);

        self.asm.bind(non_copy);
        self.asm.movdqu_load(XMM0, R10, 0);
        self.asm.movdqu_store(RSP, SCRATCH_VAL, XMM0);
        self.asm.movdqu_load(XMM1, val_base, val_disp);
        self.asm.movdqu_store(R10, 0, XMM1);
        if pop_both {
            self.asm.sub_ri(R_SP, 2 * VAL_SIZE);
        } else {
            self.asm.movdqu_store(obj_base, obj_disp, XMM1);
            self.asm.movzx_r_m8(RAX, obj_base, obj_disp);
            self.emit_clone_value(obj_base, obj_disp);
            self.asm.sub_ri(R_SP, VAL_SIZE);
        }
        self.asm.movzx_r_m8(RAX, RSP, SCRATCH_VAL);
        self.emit_drop_value(RSP, SCRATCH_VAL);
        self.asm.bind(done);
    }

    fn emit_equality_check(
        &mut self,
        op: BinaryOperator,
        lbase: u8,
        ldisp: i32,
        rbase: u8,
        rdisp: i32,
        slow: usize,
    ) {
        let strict = matches!(
            op,
            BinaryOperator::StrictEqual | BinaryOperator::StrictNotEqual
        );
        let invert = matches!(
            op,
            BinaryOperator::StrictNotEqual | BinaryOperator::NotEqual
        );
        let ret_true = self.asm.label();
        let ret_false = self.asm.label();
        let done = self.asm.label();

        self.asm.movzx_r_m8(RAX, lbase, ldisp);
        self.asm.movzx_r_m8(RCX, rbase, rdisp);
        let same_tag = self.asm.label();
        self.asm.cmp_r32_r32(RAX, RCX);
        self.asm.jcc(CC_E, same_tag);

        if strict {
            self.asm.jmp(ret_false);
        } else {
            let check_r_nullish = self.asm.label();
            let l_is_nullish = self.asm.label();
            self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNDEFINED);
            self.asm.jcc(CC_E, l_is_nullish);
            self.asm.cmp_r8_imm(RAX, VALUE_TAG_NULL);
            self.asm.jcc(CC_NE, check_r_nullish);
            self.asm.bind(l_is_nullish);
            self.asm.cmp_r8_imm(RCX, VALUE_TAG_UNDEFINED);
            self.asm.jcc(CC_E, ret_true);
            self.asm.cmp_r8_imm(RCX, VALUE_TAG_NULL);
            self.asm.jcc(CC_E, ret_true);
            self.asm.jmp(ret_false);

            self.asm.bind(check_r_nullish);
            self.asm.cmp_r8_imm(RCX, VALUE_TAG_UNDEFINED);
            self.asm.jcc(CC_E, ret_false);
            self.asm.cmp_r8_imm(RCX, VALUE_TAG_NULL);
            self.asm.jcc(CC_E, ret_false);
            self.asm.jmp(slow);
        }

        self.asm.bind(same_tag);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNDEFINED);
        self.asm.jcc(CC_E, ret_true);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_NULL);
        self.asm.jcc(CC_E, ret_true);

        let not_bool = self.asm.label();
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_BOOL);
        self.asm.jcc(CC_NE, not_bool);
        self.asm.movzx_r_m8(RDX, lbase, ldisp + 8);
        self.asm.movzx_r_m8(R8, rbase, rdisp + 8);
        self.asm.cmp_r32_r32(RDX, R8);
        self.asm.jcc(CC_E, ret_true);
        self.asm.jmp(ret_false);

        self.asm.bind(not_bool);
        let not_num = self.asm.label();
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_NUMBER);
        self.asm.jcc(CC_NE, not_num);
        self.asm.movsd_load(XMM0, lbase, ldisp + 8);
        self.asm.movsd_load(XMM1, rbase, rdisp + 8);
        self.asm.ucomisd(XMM0, XMM1);
        self.asm.jcc(CC_P, ret_false);
        self.asm.jcc(CC_E, ret_true);
        self.asm.jmp(ret_false);

        self.asm.bind(not_num);
        let not_handle = self.asm.label();
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_OBJECT);
        self.asm.jcc(CC_E, not_handle);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_ARRAY);
        let check_str = self.asm.label();
        self.asm.jcc(CC_NE, check_str);
        self.asm.bind(not_handle);
        self.asm.mov_r_mem(RDX, lbase, ldisp + 8);
        self.asm.cmp_r_mem(RDX, rbase, rdisp + 8);
        self.asm.jcc(CC_E, ret_true);
        self.asm.jmp(ret_false);

        self.asm.bind(check_str);
        self.asm.cmp_r8_imm(RAX, VALUE_TAG_STRING);
        self.asm.jcc(CC_NE, slow);
        self.asm.mov_r_mem(RDX, lbase, ldisp + 8);
        self.asm.cmp_r_mem(RDX, rbase, rdisp + 8);
        self.asm.jcc(CC_E, ret_true);
        self.asm.mov_r_mem(R8, RDX, 8);
        self.asm.mov_r_mem(R9, rbase, rdisp + 8);
        self.asm.cmp_r_mem(R8, R9, 8);
        self.asm.jcc(CC_NE, ret_false);
        self.asm.lea_mem(RDI, lbase, ldisp);
        self.asm.lea_mem(RSI, rbase, rdisp);
        self.asm.call_abs(helper_str_eq as *const () as usize);
        self.asm.test_r32(RAX, RAX);
        self.asm.jcc(CC_NE, ret_true);
        self.asm.jmp(ret_false);

        self.asm.bind(ret_true);
        self.asm.mov_ri32(RAX, u32::from(!invert));
        self.asm.jmp(done);
        self.asm.bind(ret_false);
        self.asm.mov_ri32(RAX, u32::from(invert));
        self.asm.bind(done);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_numeric_binary(
        &mut self,
        op: BinaryOperator,
        lbase: u8,
        ldisp: i32,
        rbase: u8,
        rdisp: i32,
        obase: u8,
        odisp: i32,
        slow: usize,
    ) -> bool {
        match op {
            BinaryOperator::Add
            | BinaryOperator::Subtract
            | BinaryOperator::Multiply
            | BinaryOperator::Divide => {
                self.asm.cmp_m8_imm(lbase, ldisp, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.cmp_m8_imm(rbase, rdisp, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.movsd_load(XMM0, lbase, ldisp + 8);
                self.asm.movsd_load(XMM1, rbase, rdisp + 8);
                match op {
                    BinaryOperator::Add => self.asm.addsd(XMM0, XMM1),
                    BinaryOperator::Subtract => self.asm.subsd(XMM0, XMM1),
                    BinaryOperator::Multiply => self.asm.mulsd(XMM0, XMM1),
                    BinaryOperator::Divide => self.asm.divsd(XMM0, XMM1),
                    _ => unreachable!(),
                }
                self.asm.mov_m8_imm(obase, odisp, VALUE_TAG_NUMBER);
                self.asm.movsd_store(obase, odisp + 8, XMM0);
                true
            }
            BinaryOperator::Less
            | BinaryOperator::LessEqual
            | BinaryOperator::Greater
            | BinaryOperator::GreaterEqual => {
                self.asm.cmp_m8_imm(lbase, ldisp, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.cmp_m8_imm(rbase, rdisp, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.movsd_load(XMM0, lbase, ldisp + 8);
                self.asm.movsd_load(XMM1, rbase, rdisp + 8);
                let cc = match op {
                    BinaryOperator::Greater => {
                        self.asm.ucomisd(XMM0, XMM1);
                        CC_A
                    }
                    BinaryOperator::GreaterEqual => {
                        self.asm.ucomisd(XMM0, XMM1);
                        CC_AE
                    }
                    BinaryOperator::Less => {
                        self.asm.ucomisd(XMM1, XMM0);
                        CC_A
                    }
                    BinaryOperator::LessEqual => {
                        self.asm.ucomisd(XMM1, XMM0);
                        CC_AE
                    }
                    _ => unreachable!(),
                };
                let is_true = self.asm.label();
                let done = self.asm.label();
                self.asm.jcc(cc, is_true);
                self.asm.mov_m8_imm(obase, odisp, VALUE_TAG_BOOL);
                self.asm.mov_m8_imm(obase, odisp + 8, 0);
                self.asm.jmp(done);
                self.asm.bind(is_true);
                self.asm.mov_m8_imm(obase, odisp, VALUE_TAG_BOOL);
                self.asm.mov_m8_imm(obase, odisp + 8, 1);
                self.asm.bind(done);
                true
            }
            BinaryOperator::BitAnd
            | BinaryOperator::BitOr
            | BinaryOperator::BitXor
            | BinaryOperator::ShiftLeft
            | BinaryOperator::ShiftRight
            | BinaryOperator::ShiftRightUnsigned => {
                self.asm.cmp_m8_imm(lbase, ldisp, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.cmp_m8_imm(rbase, rdisp, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.movsd_load(XMM0, lbase, ldisp + 8);
                self.asm.cvttsd2si_r32(RAX, XMM0);
                self.asm.cmp_r32_imm(RAX, i32::MIN);
                self.asm.jcc(CC_E, slow);
                self.asm.movsd_load(XMM1, rbase, rdisp + 8);
                self.asm.cvttsd2si_r32(RCX, XMM1);
                self.asm.cmp_r32_imm(RCX, i32::MIN);
                self.asm.jcc(CC_E, slow);
                match op {
                    BinaryOperator::BitAnd => {
                        self.asm.and_r32_rr(RAX, RCX);
                        self.asm.cvtsi2sd_r32(XMM0, RAX);
                    }
                    BinaryOperator::BitOr => {
                        self.asm.or_r32_rr(RAX, RCX);
                        self.asm.cvtsi2sd_r32(XMM0, RAX);
                    }
                    BinaryOperator::BitXor => {
                        self.asm.xor_r32_rr(RAX, RCX);
                        self.asm.cvtsi2sd_r32(XMM0, RAX);
                    }
                    BinaryOperator::ShiftLeft => {
                        self.asm.shl_r32_cl(RAX);
                        self.asm.cvtsi2sd_r32(XMM0, RAX);
                    }
                    BinaryOperator::ShiftRight => {
                        self.asm.sar_r32_cl(RAX);
                        self.asm.cvtsi2sd_r32(XMM0, RAX);
                    }
                    BinaryOperator::ShiftRightUnsigned => {
                        self.asm.shr_r32_cl(RAX);
                        self.asm.cvtsi2sd_r64(XMM0, RAX);
                    }
                    _ => unreachable!(),
                }
                self.asm.mov_m8_imm(obase, odisp, VALUE_TAG_NUMBER);
                self.asm.movsd_store(obase, odisp + 8, XMM0);
                true
            }
            _ => false,
        }
    }

    fn emit_safepoint_inline(&mut self, op_pc: usize, target_pc: usize, check_headroom: bool) {
        let block_ops = (op_pc + 1).saturating_sub(self.block_start).max(1) as i32;
        self.asm.add_m64_imm(R_CTX, OFF_CTX_STEPS, block_ops);
        self.asm.mov_r_mem(RAX, R_CTX, OFF_CTX_STEPS);
        self.asm.cmp_r_mem(RAX, R_CTX, OFF_CTX_SAFEPOINT);
        let sp_stub = self.asm.label();
        let after_sp = self.asm.label();
        self.asm.jcc(CC_AE, sp_stub);
        self.asm.bind(after_sp);
        self.stubs.push(Stub::Safepoint {
            label: sp_stub,
            pc: target_pc,
            cont: after_sp,
        });
        if check_headroom {
            self.asm.cmp_r_mem(R_SP, R_CTX, OFF_CTX_STACK_SAFE_LIMIT);
            let hr_stub = self.asm.label();
            let after_hr = self.asm.label();
            self.asm.jcc(CC_A, hr_stub);
            self.asm.bind(after_hr);
            self.stubs.push(Stub::ReserveHeadroom {
                label: hr_stub,
                cont: after_hr,
            });
        }
    }

    fn emit_step_op_call(&mut self, op_pc: usize, refresh_scopes: bool) {
        self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
        self.asm.mov_rr(RDI, R_CTX);
        self.asm.mov_ri32(RSI, op_pc as u32);
        self.asm.mov_ri32(RDX, u32::from(refresh_scopes));
        self.asm.call_abs(helper_step_op as *const () as usize);
        self.emit_check_helper_status();
    }

    fn defer_step_op(&mut self, op_pc: usize, refresh_scopes: bool, cont: usize) -> usize {
        let lbl = self.asm.label();
        self.stubs.push(Stub::StepOp {
            label: lbl,
            op_pc,
            refresh_scopes,
            cont,
        });
        lbl
    }

    fn emit_all(&mut self) {
        self.emit_prologue();
        let ops_len = self.code.ops.len();
        let mut is_target = vec![false; ops_len + 1];
        for op in &self.code.ops {
            let t = match *op {
                Op::Jump(t)
                | Op::JumpIfFalse(t)
                | Op::JumpIfTrue(t)
                | Op::JumpIfFalseKeep(t)
                | Op::JumpIfTrueKeep(t)
                | Op::JumpIfNotNullishKeep(t)
                | Op::JumpIfNotUndefined(t)
                | Op::JumpIfNullishChain(t)
                | Op::JumpIfRegFalse { target: t, .. }
                | Op::JumpIfRegTrue { target: t, .. }
                | Op::JumpIfNotCompare { target: t, .. }
                | Op::JumpIfNotCompareRegs { target: t, .. }
                | Op::IterStep(t)
                | Op::ForInNext(t)
                | Op::Exit { target: t, .. } => Some(t as usize),
                _ => None,
            };
            if let Some(t) = t
                && t <= ops_len
            {
                is_target[t] = true;
            }
        }
        for (op_pc, &target) in is_target.iter().enumerate().take(ops_len) {
            self.asm.bind(self.pc_labels[op_pc]);
            if target {
                self.block_start = op_pc;
            }
            let op = self.code.ops[op_pc];
            self.emit_op(op_pc, op);
        }
        self.asm.bind(self.pc_labels[ops_len]);
        self.emit_epilogue();
        self.emit_stubs();
    }

    fn emit_op(&mut self, op_pc: usize, op: Op) {
        let next_pc = op_pc + 1;
        let next_label = self.pc_labels[next_pc];
        let code_ptr = self.code as *const Code as u64;
        match op {
            Op::Nop => {}
            Op::Undefined => {
                self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_UNDEFINED);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::Null => {
                self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_NULL);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::True => {
                self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_BOOL);
                self.asm.mov_m8_imm(R_SP, 8, 1);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::False => {
                self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_BOOL);
                self.asm.mov_m8_imm(R_SP, 8, 0);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::Number(value) => {
                self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_NUMBER);
                self.asm.mov_ri64(RAX, value.to_bits());
                self.asm.mov_mem_r(R_SP, 8, RAX);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::String(index) => {
                let raw_ptr = self.code.names[index as usize].as_raw_ptr() as u64;
                self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_STRING);
                self.asm.mov_ri64(RAX, raw_ptr);
                self.asm.mov_mem_r(R_SP, 8, RAX);
                self.asm.add_m64_imm(RAX, 0, 1);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::GlobalThis => {
                self.asm.mov_m8_imm(R_SP, 0, Value::Window.tag());
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::Pop => {
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.asm.movzx_r_m8(RAX, R_SP, 0);
                self.emit_drop_value(R_SP, 0);
            }
            Op::Dup => {
                self.asm.movdqu_load(XMM0, R_SP, -VAL_SIZE);
                self.asm.movdqu_store(R_SP, 0, XMM0);
                self.asm.movzx_r_m8(RAX, R_SP, 0);
                self.asm.add_ri(R_SP, VAL_SIZE);
                self.emit_clone_value(R_SP, -VAL_SIZE);
            }
            Op::Dup2 => {
                self.asm.movdqu_load(XMM0, R_SP, -2 * VAL_SIZE);
                self.asm.movdqu_load(XMM1, R_SP, -VAL_SIZE);
                self.asm.movdqu_store(R_SP, 0, XMM0);
                self.asm.movdqu_store(R_SP, VAL_SIZE, XMM1);
                self.asm.add_ri(R_SP, 2 * VAL_SIZE);
                self.asm.movzx_r_m8(RAX, R_SP, -2 * VAL_SIZE);
                self.emit_clone_value(R_SP, -2 * VAL_SIZE);
                self.asm.movzx_r_m8(RAX, R_SP, -VAL_SIZE);
                self.emit_clone_value(R_SP, -VAL_SIZE);
            }
            Op::Swap => {
                self.asm.movdqu_load(XMM0, R_SP, -2 * VAL_SIZE);
                self.asm.movdqu_load(XMM1, R_SP, -VAL_SIZE);
                self.asm.movdqu_store(R_SP, -2 * VAL_SIZE, XMM1);
                self.asm.movdqu_store(R_SP, -VAL_SIZE, XMM0);
            }
            Op::GetReg { reg, name } => {
                let off = reg as i32 * VAL_SIZE;
                let tdz = self.asm.label();
                self.stubs.push(Stub::TdzReg {
                    label: tdz,
                    name_or_reg: name,
                    from_layout: 0,
                    next_pc,
                });
                self.asm.movzx_r_m8(RAX, R_REGS, off);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, tdz);
                self.asm.movdqu_load(XMM0, R_REGS, off);
                self.asm.movdqu_store(R_SP, 0, XMM0);
                self.asm.add_ri(R_SP, VAL_SIZE);
                self.emit_clone_value(R_SP, -VAL_SIZE);
            }
            Op::SetReg { reg, name } => {
                let off = reg as i32 * VAL_SIZE;
                let tdz = self.asm.label();
                self.stubs.push(Stub::TdzReg {
                    label: tdz,
                    name_or_reg: name,
                    from_layout: 0,
                    next_pc,
                });
                self.asm.movzx_r_m8(RAX, R_REGS, off);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, tdz);
                self.emit_overwrite_slot(R_REGS, off, R_SP, -VAL_SIZE, true);
            }
            Op::SetRegPop { reg } => {
                let off = reg as i32 * VAL_SIZE;
                let tdz = self.asm.label();
                self.stubs.push(Stub::TdzReg {
                    label: tdz,
                    name_or_reg: reg as u32,
                    from_layout: 1,
                    next_pc,
                });
                self.asm.movzx_r_m8(RAX, R_REGS, off);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, tdz);
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.emit_overwrite_slot(R_REGS, off, R_SP, 0, false);
            }
            Op::InitReg(reg) => {
                let off = reg as i32 * VAL_SIZE;
                self.asm.movzx_r_m8(RAX, R_REGS, off);
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.emit_overwrite_slot(R_REGS, off, R_SP, 0, false);
            }
            Op::UpdateReg {
                reg,
                increment,
                prefix,
                ..
            } => {
                let off = reg as i32 * VAL_SIZE;
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_REGS, off, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.movsd_load(XMM0, R_REGS, off + 8);
                let delta = if increment { 1.0f64 } else { -1.0f64 };
                self.asm.mov_ri64(RAX, delta.to_bits());
                self.asm.movq_xmm_r(XMM1, RAX);
                if prefix {
                    self.asm.addsd(XMM0, XMM1);
                    self.asm.movsd_store(R_REGS, off + 8, XMM0);
                    self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_NUMBER);
                    self.asm.movsd_store(R_SP, 8, XMM0);
                } else {
                    self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_NUMBER);
                    self.asm.movsd_store(R_SP, 8, XMM0);
                    self.asm.addsd(XMM0, XMM1);
                    self.asm.movsd_store(R_REGS, off + 8, XMM0);
                }
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::UpdateRegPop { reg, increment } => {
                let off = reg as i32 * VAL_SIZE;
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_REGS, off, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.movsd_load(XMM0, R_REGS, off + 8);
                let delta = if increment { 1.0f64 } else { -1.0f64 };
                self.asm.mov_ri64(RAX, delta.to_bits());
                self.asm.movq_xmm_r(XMM1, RAX);
                self.asm.addsd(XMM0, XMM1);
                self.asm.movsd_store(R_REGS, off + 8, XMM0);
            }
            Op::UninitRegs { index, count } if count <= 8 => {
                for i in 0..count {
                    let off = (index as i32 + i as i32) * VAL_SIZE;
                    self.asm.movzx_r_m8(RAX, R_REGS, off);
                    self.asm.movdqu_load(XMM0, R_REGS, off);
                    self.asm.movdqu_store(RSP, SCRATCH_VAL, XMM0);
                    self.asm.mov_m8_imm(R_REGS, off, VALUE_TAG_UNINITIALIZED);
                    self.emit_drop_value(RSP, SCRATCH_VAL);
                }
            }
            Op::GetLocal { depth, index, .. } if (depth as usize) < SCOPE_CACHE_DEPTH => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let binding = self.asm.label();
                let done = self.asm.label();
                let off = index as i32 * VAL_SIZE;
                self.emit_load_scope_ptr(depth, index, R10, slow);
                self.asm.movzx_r_m8(RAX, R10, off);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_BINDING);
                self.asm.jcc(CC_E, binding);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_CONST_BINDING);
                self.asm.jcc(CC_E, binding);
                self.asm.movdqu_load(XMM0, R10, off);
                self.asm.movdqu_store(R_SP, 0, XMM0);
                self.asm.add_ri(R_SP, VAL_SIZE);
                self.emit_clone_value(R_SP, -VAL_SIZE);
                self.asm.jmp(done);
                // zero-sync leaf read for ES module Binding/ConstBinding
                self.asm.bind(binding);
                self.asm.lea_mem(RDI, R10, off);
                self.asm.mov_rr(RSI, R_SP);
                self.asm.call_abs(helper_read_binding as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.add_ri(R_SP, VAL_SIZE);
                self.asm.bind(done);
            }
            Op::SetLocal { depth, index, .. } if (depth as usize) < SCOPE_CACHE_DEPTH => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let off = index as i32 * VAL_SIZE;
                self.emit_load_scope_ptr(depth, index, R10, slow);
                self.asm.movzx_r_m8(RAX, R10, off);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_BINDING);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_CONST_BINDING);
                self.asm.jcc(CC_E, slow);
                self.emit_overwrite_slot(R10, off, R_SP, -VAL_SIZE, true);
            }
            Op::SetLocalPop { depth, index } if (depth as usize) < SCOPE_CACHE_DEPTH => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let off = index as i32 * VAL_SIZE;
                self.emit_load_scope_ptr(depth, index, R10, slow);
                self.asm.movzx_r_m8(RAX, R10, off);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_BINDING);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_CONST_BINDING);
                self.asm.jcc(CC_E, slow);
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.emit_overwrite_slot(R10, off, R_SP, 0, false);
            }
            Op::InitLocal { depth, index } if (depth as usize) < SCOPE_CACHE_DEPTH => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let off = index as i32 * VAL_SIZE;
                self.emit_load_scope_ptr(depth, index, R10, slow);
                self.asm.movzx_r_m8(RAX, R10, off);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_BINDING);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_CONST_BINDING);
                self.asm.jcc(CC_E, slow);
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.emit_overwrite_slot(R10, off, R_SP, 0, false);
            }
            Op::InitThisSlot(slot) => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let off = slot as i32 * VAL_SIZE;
                self.emit_load_scope_ptr(0, slot, R10, slow);
                self.asm.movzx_r_m8(RAX, R10, off);
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.emit_overwrite_slot(R10, off, R_SP, 0, false);
            }
            Op::UpdateLocal {
                depth,
                index,
                increment,
                prefix,
                ..
            } if (depth as usize) < SCOPE_CACHE_DEPTH => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let off = index as i32 * VAL_SIZE;
                self.emit_load_scope_ptr(depth, index, R10, slow);
                self.asm.cmp_m8_imm(R10, off, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.movsd_load(XMM0, R10, off + 8);
                let delta = if increment { 1.0f64 } else { -1.0f64 };
                self.asm.mov_ri64(RAX, delta.to_bits());
                self.asm.movq_xmm_r(XMM1, RAX);
                if prefix {
                    self.asm.addsd(XMM0, XMM1);
                    self.asm.movsd_store(R10, off + 8, XMM0);
                    self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_NUMBER);
                    self.asm.movsd_store(R_SP, 8, XMM0);
                } else {
                    self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_NUMBER);
                    self.asm.movsd_store(R_SP, 8, XMM0);
                    self.asm.addsd(XMM0, XMM1);
                    self.asm.movsd_store(R10, off + 8, XMM0);
                }
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::UpdateLocalPop {
                depth,
                index,
                increment,
            } if (depth as usize) < SCOPE_CACHE_DEPTH => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let off = index as i32 * VAL_SIZE;
                self.emit_load_scope_ptr(depth, index, R10, slow);
                self.asm.cmp_m8_imm(R10, off, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.movsd_load(XMM0, R10, off + 8);
                let delta = if increment { 1.0f64 } else { -1.0f64 };
                self.asm.mov_ri64(RAX, delta.to_bits());
                self.asm.movq_xmm_r(XMM1, RAX);
                self.asm.addsd(XMM0, XMM1);
                self.asm.movsd_store(R10, off + 8, XMM0);
            }
            Op::GetGlobal(name) => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_ri64(RSI, code_ptr);
                self.asm.mov_ri32(RDX, name);
                self.asm.mov_rr(RCX, R_SP);
                self.asm
                    .call_abs(helper_get_global_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::This => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_r_mem(RSI, R_CTX, OFF_CTX_TOP);
                self.asm.mov_rr(RDX, R_SP);
                self.asm.call_abs(helper_this_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::TypeOf => {
                self.asm.mov_rr(RDI, R_RT);
                self.asm.lea_mem(RSI, R_SP, -VAL_SIZE);
                self.asm.call_abs(helper_typeof_leaf as *const () as usize);
            }
            Op::NewObject => {
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_rr(RSI, R_HEAP);
                self.asm.mov_rr(RDX, R_SP);
                self.asm.call_abs(helper_new_object as *const () as usize);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::MakeObject(template) => {
                let count = self.code.object_shapes[template as usize].0.len() as i32;
                let bytes = count * VAL_SIZE;
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_rr(RSI, R_HEAP);
                self.asm.mov_ri64(RDX, code_ptr);
                self.asm.mov_ri32(RCX, template);
                self.asm.lea_mem(R8, R_SP, -bytes);
                self.asm.lea_mem(R9, R_SP, -bytes);
                self.asm.call_abs(helper_make_object as *const () as usize);
                if count != 1 {
                    self.asm.add_ri(R_SP, VAL_SIZE - bytes);
                }
            }
            Op::NewArray => {
                self.asm.mov_rr(RDI, R_HEAP);
                self.asm.mov_rr(RSI, R_SP);
                self.asm.call_abs(helper_new_array as *const () as usize);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::MakeArray(count) => {
                let bytes = count as i32 * VAL_SIZE;
                self.asm.mov_rr(RDI, R_HEAP);
                self.asm.lea_mem(RSI, R_SP, -bytes);
                self.asm.mov_ri32(RDX, count as u32);
                self.asm.lea_mem(RCX, R_SP, -bytes);
                self.asm.call_abs(helper_make_array as *const () as usize);
                if count != 1 {
                    self.asm.add_ri(R_SP, VAL_SIZE - bytes);
                }
            }
            Op::ArrayPush => {
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.asm.mov_rr(RDI, R_HEAP);
                self.asm.lea_mem(RSI, R_SP, -VAL_SIZE);
                self.asm.mov_rr(RDX, R_SP);
                self.asm.call_abs(helper_array_push as *const () as usize);
            }
            Op::DefineField(name) => {
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.asm.mov_rr(RDI, R_HEAP);
                self.asm.mov_ri64(RSI, code_ptr);
                self.asm.mov_ri32(RDX, name);
                self.asm.lea_mem(RCX, R_SP, -VAL_SIZE);
                self.asm.mov_rr(R8, R_SP);
                self.asm.call_abs(helper_define_field as *const () as usize);
            }
            Op::Closure(index) => {
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_r_mem(RSI, R_CTX, OFF_CTX_TOP);
                self.asm.mov_ri32(RDX, index);
                self.asm.mov_rr(RCX, R_SP);
                self.asm.call_abs(helper_closure as *const () as usize);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::GetIndex => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.mov_rr(RDI, R_HEAP);
                self.asm.lea_mem(RSI, R_SP, -2 * VAL_SIZE);
                self.asm.lea_mem(RDX, R_SP, -VAL_SIZE);
                self.asm.lea_mem(RCX, R_SP, -2 * VAL_SIZE);
                self.asm.mov_ri32(R8, 1);
                self.asm
                    .call_abs(helper_get_index_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.sub_ri(R_SP, VAL_SIZE);
            }
            Op::GetRegIndex { array, index } => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let aoff = array as i32 * VAL_SIZE;
                let ioff = index as i32 * VAL_SIZE;
                self.asm.cmp_m8_imm(R_REGS, aoff, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_m8_imm(R_REGS, ioff, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, slow);
                self.asm.mov_rr(RDI, R_HEAP);
                self.asm.lea_mem(RSI, R_REGS, aoff);
                self.asm.lea_mem(RDX, R_REGS, ioff);
                self.asm.mov_rr(RCX, R_SP);
                self.asm.mov_ri32(R8, 0);
                self.asm
                    .call_abs(helper_get_index_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::SetIndex => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.mov_rr(RDI, R_HEAP);
                self.asm.lea_mem(RSI, R_SP, -3 * VAL_SIZE);
                self.asm.lea_mem(RDX, R_SP, -2 * VAL_SIZE);
                self.asm.lea_mem(RCX, R_SP, -VAL_SIZE);
                self.asm.mov_ri32(R8, 0);
                self.asm
                    .call_abs(helper_set_index_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.sub_ri(R_SP, 2 * VAL_SIZE);
            }
            Op::SetIndexPop => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.mov_rr(RDI, R_HEAP);
                self.asm.lea_mem(RSI, R_SP, -3 * VAL_SIZE);
                self.asm.lea_mem(RDX, R_SP, -2 * VAL_SIZE);
                self.asm.lea_mem(RCX, R_SP, -VAL_SIZE);
                self.asm.mov_ri32(R8, 1);
                self.asm
                    .call_abs(helper_set_index_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.sub_ri(R_SP, 3 * VAL_SIZE);
            }
            Op::PushHandler { catch, finally } => {
                // compute current stack_len = (R_SP - stack_ptr) / 16
                self.asm
                    .mov_r_mem(RAX, R_CTX, offset_of!(JitCtx, stack_ptr) as i32);
                self.asm.mov_rr(RDX, R_SP);
                // sub rdx, rax
                self.asm.rex(1, RAX, 0, RDX);
                self.asm.emit(0x29);
                self.asm.modrm(3, RAX, RDX);
                self.asm.shr_ri(RDX, 4);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_r_mem(RSI, R_CTX, OFF_CTX_TOP);
                self.asm.mov_ri32(RCX, catch);
                self.asm.mov_ri32(R8, finally);
                self.asm.call_abs(helper_push_handler as *const () as usize);
            }
            Op::PopHandler => {
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_r_mem(RSI, R_CTX, OFF_CTX_TOP);
                self.asm.call_abs(helper_pop_handler as *const () as usize);
            }
            Op::CompletionNormal => {
                self.asm.mov_rr(RDI, R_RT);
                self.asm
                    .call_abs(helper_completion_normal as *const () as usize);
            }
            Op::ChainStart => {
                self.asm
                    .mov_r_mem(RAX, R_CTX, offset_of!(JitCtx, stack_ptr) as i32);
                self.asm.mov_rr(RDX, R_SP);
                self.asm.rex(1, RAX, 0, RDX);
                self.asm.emit(0x29);
                self.asm.modrm(3, RAX, RDX);
                self.asm.shr_ri(RDX, 4);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_r_mem(RSI, R_CTX, OFF_CTX_TOP);
                self.asm.call_abs(helper_chain_start as *const () as usize);
            }
            Op::ChainEnd => {
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_r_mem(RSI, R_CTX, OFF_CTX_TOP);
                self.asm.call_abs(helper_chain_end as *const () as usize);
            }
            Op::JumpIfNullishChain(_) => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.movzx_r_m8(RAX, R_SP, -VAL_SIZE);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNDEFINED);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_NULL);
                self.asm.jcc(CC_E, slow);
            }
            Op::ArgumentsIterator => {
                self.asm.mov_rr(RDI, R_RT);
                self.asm.lea_mem(RSI, R_SP, -VAL_SIZE);
                self.asm.call_abs(helper_args_iter as *const () as usize);
            }
            Op::GetIterator(_) => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_ARRAY);
                self.asm.jcc(CC_NE, slow);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_rr(RSI, R_HEAP);
                self.asm.lea_mem(RDX, R_SP, -VAL_SIZE);
                self.asm
                    .call_abs(helper_get_iter_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
            }
            Op::IterValue => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_ARRAY_ITER);
                self.asm.jcc(CC_NE, slow);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_rr(RSI, R_HEAP);
                self.asm.lea_mem(RDX, R_SP, -VAL_SIZE);
                self.asm.mov_rr(RCX, R_SP);
                self.asm
                    .call_abs(helper_iter_value_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::IterStep(target) => {
                let target = target as usize;
                let slow = self.defer_step_op(op_pc, false, next_label);
                let done_iter = self.asm.label();
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_ARRAY_ITER);
                self.asm.jcc(CC_NE, slow);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_rr(RSI, R_HEAP);
                self.asm.lea_mem(RDX, R_SP, -VAL_SIZE);
                self.asm.mov_rr(RCX, R_SP);
                self.asm
                    .call_abs(helper_iter_step_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_r32_imm(RAX, 2);
                self.asm.jcc(CC_E, done_iter);
                self.asm.add_ri(R_SP, VAL_SIZE);
                self.asm.jmp(next_label);
                self.asm.bind(done_iter);
                if target <= op_pc {
                    self.emit_safepoint_inline(op_pc, target, true);
                }
                self.asm.jmp(self.pc_labels[target]);
            }
            Op::IterRest => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_ARRAY_ITER);
                self.asm.jcc(CC_NE, slow);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_rr(RSI, R_HEAP);
                self.asm.lea_mem(RDX, R_SP, -VAL_SIZE);
                self.asm.mov_rr(RCX, R_SP);
                self.asm
                    .call_abs(helper_iter_rest_leaf as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::IterClose => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_ARRAY_ITER);
                self.asm.jcc(CC_NE, slow);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.lea_mem(RSI, R_SP, -VAL_SIZE);
                self.asm
                    .call_abs(helper_iter_close_leaf as *const () as usize);
                self.asm.sub_ri(R_SP, VAL_SIZE);
            }
            Op::PushScope(block) => {
                self.asm.mov_rr(RDI, R_CTX);
                self.asm.mov_ri32(RSI, block as u32);
                self.asm.mov_ri32(RDX, 0);
                self.asm.call_abs(helper_push_scope as *const () as usize);
            }
            Op::PushVarScope(block) => {
                self.asm.mov_rr(RDI, R_CTX);
                self.asm.mov_ri32(RSI, block as u32);
                self.asm.mov_ri32(RDX, 1);
                self.asm.call_abs(helper_push_scope as *const () as usize);
            }
            Op::PopScope => {
                self.asm.mov_rr(RDI, R_CTX);
                self.asm.call_abs(helper_pop_scope as *const () as usize);
            }
            Op::CloneScope => {
                self.asm.mov_rr(RDI, R_CTX);
                self.asm.call_abs(helper_clone_scope as *const () as usize);
            }
            Op::Uninitialize { index, count } => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.mov_rr(RDI, R_CTX);
                self.asm.mov_ri32(RSI, index as u32);
                self.asm.mov_ri32(RDX, count as u32);
                self.asm.call_abs(helper_uninitialize as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
            }
            Op::EndFinally => {
                let slow = self.defer_step_op(op_pc, true, next_label);
                self.asm.mov_rr(RDI, R_RT);
                self.asm.mov_r_mem(RSI, R_CTX, OFF_CTX_TOP);
                self.asm
                    .call_abs(helper_end_finally_normal as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
            }
            Op::Exit { .. } => {
                self.emit_step_op_call(op_pc, true);
            }
            Op::TemplateConcat(count) => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let bytes = count as i32 * VAL_SIZE;
                self.asm.lea_mem(RDI, R_SP, -bytes);
                self.asm.mov_ri32(RSI, count as u32);
                self.asm.lea_mem(RDX, R_SP, -bytes);
                self.asm
                    .call_abs(helper_template_concat as *const () as usize);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_E, slow);
                if count != 1 {
                    self.asm.add_ri(R_SP, VAL_SIZE - bytes);
                }
            }
            Op::RequireCoercible => {
                let err_stub = self.asm.label();
                self.stubs.push(Stub::RequireCoercibleErr {
                    label: err_stub,
                    next_pc,
                });
                self.asm.movzx_r_m8(RAX, R_SP, -VAL_SIZE);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNDEFINED);
                self.asm.jcc(CC_E, err_stub);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_NULL);
                self.asm.jcc(CC_E, err_stub);
            }
            Op::ToPropertyKey => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_STRING);
                self.asm.jcc(CC_NE, slow);
            }
            Op::ToIndexKey => {
                let done = self.asm.label();
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.movzx_r_m8(RAX, R_SP, -VAL_SIZE);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_STRING);
                self.asm.jcc(CC_E, done);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.movsd_load(XMM0, R_SP, -VAL_SIZE + 8);
                self.asm.cvttsd2si_r32(RAX, XMM0);
                self.asm.test_r32(RAX, RAX);
                self.asm.jcc(CC_S, slow);
                self.asm.cvtsi2sd_r32(XMM1, RAX);
                self.asm.ucomisd(XMM0, XMM1);
                self.asm.jcc(CC_P, slow);
                self.asm.jcc(CC_NE, slow);
                self.asm.bind(done);
            }
            Op::Not => {
                let is_true = self.asm.label();
                let is_false = self.asm.label();
                let done = self.asm.label();
                self.emit_truthy_branch(R_SP, -VAL_SIZE, is_true, is_false);
                self.asm.bind(is_true);
                self.asm.movzx_r_m8(RAX, R_SP, -VAL_SIZE);
                self.emit_drop_value(R_SP, -VAL_SIZE);
                self.asm.mov_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_BOOL);
                self.asm.mov_m8_imm(R_SP, -VAL_SIZE + 8, 0);
                self.asm.jmp(done);
                self.asm.bind(is_false);
                self.asm.movzx_r_m8(RAX, R_SP, -VAL_SIZE);
                self.emit_drop_value(R_SP, -VAL_SIZE);
                self.asm.mov_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_BOOL);
                self.asm.mov_m8_imm(R_SP, -VAL_SIZE + 8, 1);
                self.asm.bind(done);
            }
            Op::Negate => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.xor_m8_imm(R_SP, -VAL_SIZE + 15, 0x80);
            }
            Op::Plus => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
            }
            Op::Increment | Op::Decrement => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_NUMBER);
                self.asm.jcc(CC_NE, slow);
                self.asm.movsd_load(XMM0, R_SP, -VAL_SIZE + 8);
                let delta = if matches!(op, Op::Increment) {
                    1.0f64
                } else {
                    -1.0f64
                };
                self.asm.mov_ri64(RAX, delta.to_bits());
                self.asm.movq_xmm_r(XMM1, RAX);
                self.asm.addsd(XMM0, XMM1);
                self.asm.movsd_store(R_SP, -VAL_SIZE + 8, XMM0);
            }
            Op::Binary(operator) => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                if matches!(
                    operator,
                    BinaryOperator::StrictEqual
                        | BinaryOperator::StrictNotEqual
                        | BinaryOperator::Equal
                        | BinaryOperator::NotEqual
                ) {
                    self.emit_equality_check(operator, R_SP, -2 * VAL_SIZE, R_SP, -VAL_SIZE, slow);
                    self.asm.mov_m8_r(RSP, SCRATCH_TMP, RAX);
                    self.asm.movzx_r_m8(RAX, R_SP, -VAL_SIZE);
                    self.emit_drop_value(R_SP, -VAL_SIZE);
                    self.asm.movzx_r_m8(RAX, R_SP, -2 * VAL_SIZE);
                    self.emit_drop_value(R_SP, -2 * VAL_SIZE);
                    self.asm.sub_ri(R_SP, VAL_SIZE);
                    self.asm.mov_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_BOOL);
                    self.asm.movzx_r_m8(RAX, RSP, SCRATCH_TMP);
                    self.asm.mov_m8_r(R_SP, -VAL_SIZE + 8, RAX);
                } else if matches!(operator, BinaryOperator::Add) {
                    let check_str = self.asm.label();
                    self.emit_numeric_binary(
                        operator,
                        R_SP,
                        -2 * VAL_SIZE,
                        R_SP,
                        -VAL_SIZE,
                        R_SP,
                        -2 * VAL_SIZE,
                        check_str,
                    );
                    self.asm.sub_ri(R_SP, VAL_SIZE);
                    self.asm.jmp(next_label);
                    self.asm.bind(check_str);
                    self.asm.cmp_m8_imm(R_SP, -2 * VAL_SIZE, VALUE_TAG_STRING);
                    self.asm.jcc(CC_NE, slow);
                    self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_STRING);
                    self.asm.jcc(CC_NE, slow);
                    self.asm.lea_mem(RDI, R_SP, -2 * VAL_SIZE);
                    self.asm.lea_mem(RSI, R_SP, -VAL_SIZE);
                    self.asm.lea_mem(RDX, R_SP, -2 * VAL_SIZE);
                    self.asm.mov_ri32(RCX, 1);
                    self.asm.call_abs(helper_str_concat as *const () as usize);
                    self.asm.sub_ri(R_SP, VAL_SIZE);
                } else if self.emit_numeric_binary(
                    operator,
                    R_SP,
                    -2 * VAL_SIZE,
                    R_SP,
                    -VAL_SIZE,
                    R_SP,
                    -2 * VAL_SIZE,
                    slow,
                ) {
                    self.asm.sub_ri(R_SP, VAL_SIZE);
                } else {
                    self.emit_step_op_call(op_pc, false);
                }
            }
            Op::BinaryRegs {
                left,
                right,
                op: operator,
            } => {
                let slow = self.defer_step_op(op_pc, false, next_label);
                let loff = left as i32 * VAL_SIZE;
                let roff = right as i32 * VAL_SIZE;
                self.asm.cmp_m8_imm(R_REGS, loff, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_m8_imm(R_REGS, roff, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, slow);
                if matches!(
                    operator,
                    BinaryOperator::StrictEqual
                        | BinaryOperator::StrictNotEqual
                        | BinaryOperator::Equal
                        | BinaryOperator::NotEqual
                ) {
                    self.emit_equality_check(operator, R_REGS, loff, R_REGS, roff, slow);
                    self.asm.mov_m8_imm(R_SP, 0, VALUE_TAG_BOOL);
                    self.asm.mov_m8_r(R_SP, 8, RAX);
                    self.asm.add_ri(R_SP, VAL_SIZE);
                } else if matches!(operator, BinaryOperator::Add) {
                    let check_str = self.asm.label();
                    self.emit_numeric_binary(
                        operator, R_REGS, loff, R_REGS, roff, R_SP, 0, check_str,
                    );
                    self.asm.add_ri(R_SP, VAL_SIZE);
                    self.asm.jmp(next_label);
                    self.asm.bind(check_str);
                    self.asm.cmp_m8_imm(R_REGS, loff, VALUE_TAG_STRING);
                    self.asm.jcc(CC_NE, slow);
                    self.asm.cmp_m8_imm(R_REGS, roff, VALUE_TAG_STRING);
                    self.asm.jcc(CC_NE, slow);
                    self.asm.lea_mem(RDI, R_REGS, loff);
                    self.asm.lea_mem(RSI, R_REGS, roff);
                    self.asm.mov_rr(RDX, R_SP);
                    self.asm.mov_ri32(RCX, 0);
                    self.asm.call_abs(helper_str_concat as *const () as usize);
                    self.asm.add_ri(R_SP, VAL_SIZE);
                } else if self
                    .emit_numeric_binary(operator, R_REGS, loff, R_REGS, roff, R_SP, 0, slow)
                {
                    self.asm.add_ri(R_SP, VAL_SIZE);
                } else {
                    self.emit_step_op_call(op_pc, false);
                }
            }
            Op::Jump(target) => {
                let target = target as usize;
                if target <= op_pc {
                    self.emit_safepoint_inline(op_pc, target, true);
                }
                self.asm.jmp(self.pc_labels[target]);
            }
            Op::JumpIfFalse(target) | Op::JumpIfTrue(target) => {
                let jump_on_true = matches!(op, Op::JumpIfTrue(_));
                let target = target as usize;
                let take = self.asm.label();
                let fall = self.asm.label();
                if jump_on_true {
                    self.emit_truthy_branch(R_SP, -VAL_SIZE, take, fall);
                } else {
                    self.emit_truthy_branch(R_SP, -VAL_SIZE, fall, take);
                }
                self.asm.bind(take);
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.asm.movzx_r_m8(RAX, R_SP, 0);
                self.emit_drop_value(R_SP, 0);
                if target <= op_pc {
                    self.emit_safepoint_inline(op_pc, target, true);
                }
                self.asm.jmp(self.pc_labels[target]);
                self.asm.bind(fall);
                self.asm.sub_ri(R_SP, VAL_SIZE);
                self.asm.movzx_r_m8(RAX, R_SP, 0);
                self.emit_drop_value(R_SP, 0);
            }
            Op::JumpIfFalseKeep(target) | Op::JumpIfTrueKeep(target) => {
                let jump_on_true = matches!(op, Op::JumpIfTrueKeep(_));
                let target = target as usize;
                let take = self.asm.label();
                if jump_on_true {
                    self.emit_truthy_branch(R_SP, -VAL_SIZE, take, next_label);
                } else {
                    self.emit_truthy_branch(R_SP, -VAL_SIZE, next_label, take);
                }
                self.asm.bind(take);
                if target <= op_pc {
                    self.emit_safepoint_inline(op_pc, target, true);
                }
                self.asm.jmp(self.pc_labels[target]);
            }
            Op::JumpIfNotNullishKeep(target) => {
                let target = target as usize;
                self.asm.movzx_r_m8(RAX, R_SP, -VAL_SIZE);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNDEFINED);
                self.asm.jcc(CC_E, next_label);
                self.asm.cmp_r8_imm(RAX, VALUE_TAG_NULL);
                self.asm.jcc(CC_E, next_label);
                if target <= op_pc {
                    self.emit_safepoint_inline(op_pc, target, true);
                }
                self.asm.jmp(self.pc_labels[target]);
            }
            Op::JumpIfNotUndefined(target) => {
                let target = target as usize;
                self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_UNDEFINED);
                self.asm.jcc(CC_E, next_label);
                if target <= op_pc {
                    self.emit_safepoint_inline(op_pc, target, true);
                }
                self.asm.jmp(self.pc_labels[target]);
            }
            Op::JumpIfRegFalse { reg, target } | Op::JumpIfRegTrue { reg, target } => {
                let jump_on_true = matches!(op, Op::JumpIfRegTrue { .. });
                let target = target as usize;
                let off = reg as i32 * VAL_SIZE;
                let tdz = self.asm.label();
                self.stubs.push(Stub::TdzReg {
                    label: tdz,
                    name_or_reg: reg as u32,
                    from_layout: 1,
                    next_pc,
                });
                self.asm.cmp_m8_imm(R_REGS, off, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, tdz);
                let take = self.asm.label();
                if jump_on_true {
                    self.emit_truthy_branch(R_REGS, off, take, next_label);
                } else {
                    self.emit_truthy_branch(R_REGS, off, next_label, take);
                }
                self.asm.bind(take);
                if target <= op_pc {
                    self.emit_safepoint_inline(op_pc, target, true);
                }
                self.asm.jmp(self.pc_labels[target]);
            }
            Op::JumpIfNotCompare {
                op: operator,
                target,
            } => {
                let target = target as usize;
                let slow = self.defer_step_op(op_pc, false, next_label);
                match operator {
                    BinaryOperator::Less
                    | BinaryOperator::LessEqual
                    | BinaryOperator::Greater
                    | BinaryOperator::GreaterEqual => {
                        self.asm.cmp_m8_imm(R_SP, -2 * VAL_SIZE, VALUE_TAG_NUMBER);
                        self.asm.jcc(CC_NE, slow);
                        self.asm.cmp_m8_imm(R_SP, -VAL_SIZE, VALUE_TAG_NUMBER);
                        self.asm.jcc(CC_NE, slow);
                        self.asm.movsd_load(XMM0, R_SP, -2 * VAL_SIZE + 8);
                        self.asm.movsd_load(XMM1, R_SP, -VAL_SIZE + 8);
                        self.asm.sub_ri(R_SP, 2 * VAL_SIZE);
                        let cc_true = match operator {
                            BinaryOperator::Greater => {
                                self.asm.ucomisd(XMM0, XMM1);
                                CC_A
                            }
                            BinaryOperator::GreaterEqual => {
                                self.asm.ucomisd(XMM0, XMM1);
                                CC_AE
                            }
                            BinaryOperator::Less => {
                                self.asm.ucomisd(XMM1, XMM0);
                                CC_A
                            }
                            BinaryOperator::LessEqual => {
                                self.asm.ucomisd(XMM1, XMM0);
                                CC_AE
                            }
                            _ => unreachable!(),
                        };
                        self.asm.jcc(cc_true, next_label);
                        if target <= op_pc {
                            self.emit_safepoint_inline(op_pc, target, true);
                        }
                        self.asm.jmp(self.pc_labels[target]);
                    }
                    BinaryOperator::StrictEqual
                    | BinaryOperator::StrictNotEqual
                    | BinaryOperator::Equal
                    | BinaryOperator::NotEqual => {
                        self.emit_equality_check(
                            operator,
                            R_SP,
                            -2 * VAL_SIZE,
                            R_SP,
                            -VAL_SIZE,
                            slow,
                        );
                        self.asm.mov_m8_r(RSP, SCRATCH_TMP, RAX);
                        self.asm.movzx_r_m8(RAX, R_SP, -VAL_SIZE);
                        self.emit_drop_value(R_SP, -VAL_SIZE);
                        self.asm.movzx_r_m8(RAX, R_SP, -2 * VAL_SIZE);
                        self.emit_drop_value(R_SP, -2 * VAL_SIZE);
                        self.asm.sub_ri(R_SP, 2 * VAL_SIZE);
                        self.asm.cmp_m8_imm(RSP, SCRATCH_TMP, 0);
                        self.asm.jcc(CC_NE, next_label);
                        if target <= op_pc {
                            self.emit_safepoint_inline(op_pc, target, true);
                        }
                        self.asm.jmp(self.pc_labels[target]);
                    }
                    _ => {
                        self.emit_step_op_call(op_pc, false);
                    }
                }
            }
            Op::JumpIfNotCompareRegs {
                left,
                right,
                op: operator,
                target,
            } => {
                let target = target as usize;
                let loff = left as i32 * VAL_SIZE;
                let roff = right as i32 * VAL_SIZE;
                let slow = self.defer_step_op(op_pc, false, next_label);
                self.asm.cmp_m8_imm(R_REGS, loff, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, slow);
                self.asm.cmp_m8_imm(R_REGS, roff, VALUE_TAG_UNINITIALIZED);
                self.asm.jcc(CC_E, slow);
                match operator {
                    BinaryOperator::Less
                    | BinaryOperator::LessEqual
                    | BinaryOperator::Greater
                    | BinaryOperator::GreaterEqual => {
                        self.asm.cmp_m8_imm(R_REGS, loff, VALUE_TAG_NUMBER);
                        self.asm.jcc(CC_NE, slow);
                        self.asm.cmp_m8_imm(R_REGS, roff, VALUE_TAG_NUMBER);
                        self.asm.jcc(CC_NE, slow);
                        self.asm.movsd_load(XMM0, R_REGS, loff + 8);
                        self.asm.movsd_load(XMM1, R_REGS, roff + 8);
                        let cc_true = match operator {
                            BinaryOperator::Greater => {
                                self.asm.ucomisd(XMM0, XMM1);
                                CC_A
                            }
                            BinaryOperator::GreaterEqual => {
                                self.asm.ucomisd(XMM0, XMM1);
                                CC_AE
                            }
                            BinaryOperator::Less => {
                                self.asm.ucomisd(XMM1, XMM0);
                                CC_A
                            }
                            BinaryOperator::LessEqual => {
                                self.asm.ucomisd(XMM1, XMM0);
                                CC_AE
                            }
                            _ => unreachable!(),
                        };
                        self.asm.jcc(cc_true, next_label);
                        if target <= op_pc {
                            self.emit_safepoint_inline(op_pc, target, true);
                        }
                        self.asm.jmp(self.pc_labels[target]);
                    }
                    BinaryOperator::StrictEqual
                    | BinaryOperator::StrictNotEqual
                    | BinaryOperator::Equal
                    | BinaryOperator::NotEqual => {
                        self.emit_equality_check(operator, R_REGS, loff, R_REGS, roff, slow);
                        self.asm.test_r32(RAX, RAX);
                        self.asm.jcc(CC_NE, next_label);
                        if target <= op_pc {
                            self.emit_safepoint_inline(op_pc, target, true);
                        }
                        self.asm.jmp(self.pc_labels[target]);
                    }
                    _ => {
                        self.emit_step_op_call(op_pc, false);
                    }
                }
            }
            Op::GetProp { name, ic } => {
                let ic_ptr = &self.code.ics[ic as usize] as *const PropIc;
                let miss = self.asm.label();
                self.stubs.push(Stub::GetPropSlow {
                    label: miss,
                    op_pc,
                    name,
                    ic,
                    mode: 0,
                    ic_ptr,
                    recv_reg_or_local: None,
                    cont: next_label,
                });
                self.emit_mono_ic_get(ic_ptr, R_SP, -VAL_SIZE, R_SP, -VAL_SIZE, miss);
            }
            Op::GetRegProp { reg, name, ic } => {
                let off = reg as i32 * VAL_SIZE;
                let ic_ptr = &self.code.ics[ic as usize] as *const PropIc;
                let miss = self.asm.label();
                self.stubs.push(Stub::GetPropSlow {
                    label: miss,
                    op_pc,
                    name,
                    ic,
                    mode: 1,
                    ic_ptr,
                    recv_reg_or_local: Some((true, 0, reg)),
                    cont: next_label,
                });
                self.emit_mono_ic_get(ic_ptr, R_REGS, off, R_SP, 0, miss);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::GetRegMethod { reg, name, ic } => {
                let off = reg as i32 * VAL_SIZE;
                let ic_ptr = &self.code.ics[ic as usize] as *const PropIc;
                let miss = self.asm.label();
                self.stubs.push(Stub::GetPropSlow {
                    label: miss,
                    op_pc,
                    name,
                    ic,
                    mode: 2,
                    ic_ptr,
                    recv_reg_or_local: Some((true, 0, reg)),
                    cont: next_label,
                });
                self.emit_mono_ic_get(ic_ptr, R_REGS, off, R_SP, VAL_SIZE, miss);
                self.asm.movdqu_load(XMM0, R_REGS, off);
                self.asm.movdqu_store(R_SP, 0, XMM0);
                self.asm.add_ri(R_SP, 2 * VAL_SIZE);
            }
            Op::GetLocalProp {
                depth,
                index,
                name,
                ic,
            } if (depth as usize) < SCOPE_CACHE_DEPTH => {
                let ic_ptr = &self.code.ics[ic as usize] as *const PropIc;
                let miss = self.asm.label();
                self.stubs.push(Stub::GetPropSlow {
                    label: miss,
                    op_pc,
                    name,
                    ic,
                    mode: 3,
                    ic_ptr,
                    recv_reg_or_local: Some((false, depth, index)),
                    cont: next_label,
                });
                let off = index as i32 * VAL_SIZE;
                self.emit_load_scope_ptr(depth, index, R11, miss);
                self.emit_mono_ic_get(ic_ptr, R11, off, R_SP, 0, miss);
                self.asm.add_ri(R_SP, VAL_SIZE);
            }
            Op::GetLocalMethod {
                depth,
                index,
                name,
                ic,
            } if (depth as usize) < SCOPE_CACHE_DEPTH => {
                let ic_ptr = &self.code.ics[ic as usize] as *const PropIc;
                let miss = self.asm.label();
                self.stubs.push(Stub::GetPropSlow {
                    label: miss,
                    op_pc,
                    name,
                    ic,
                    mode: 4,
                    ic_ptr,
                    recv_reg_or_local: Some((false, depth, index)),
                    cont: next_label,
                });
                let off = index as i32 * VAL_SIZE;
                self.emit_load_scope_ptr(depth, index, R11, miss);
                self.emit_mono_ic_get(ic_ptr, R11, off, R_SP, VAL_SIZE, miss);
                let ptr_off = OFF_CTX_SCOPE_PTRS + (depth as i32) * 8;
                self.asm.mov_r_mem(R11, R_CTX, ptr_off);
                self.asm.movdqu_load(XMM0, R11, off);
                self.asm.movdqu_store(R_SP, 0, XMM0);
                self.asm.add_ri(R_SP, 2 * VAL_SIZE);
            }
            Op::SetProp { name, ic } => {
                let ic_ptr = &self.code.ics[ic as usize] as *const PropIc;
                let miss = self.asm.label();
                self.stubs.push(Stub::SetPropSlow {
                    label: miss,
                    next_pc,
                    name,
                    ic,
                    pop: 0,
                    ic_ptr,
                    cont: next_label,
                });
                self.emit_mono_ic_set(ic_ptr, R_SP, -2 * VAL_SIZE, R_SP, -VAL_SIZE, false, miss);
            }
            Op::SetPropPop { name, ic } => {
                let ic_ptr = &self.code.ics[ic as usize] as *const PropIc;
                let miss = self.asm.label();
                self.stubs.push(Stub::SetPropSlow {
                    label: miss,
                    next_pc,
                    name,
                    ic,
                    pop: 1,
                    ic_ptr,
                    cont: next_label,
                });
                self.emit_mono_ic_set(ic_ptr, R_SP, -2 * VAL_SIZE, R_SP, -VAL_SIZE, true, miss);
            }
            Op::Call(count) | Op::CallMethod(count) => {
                self.emit_safepoint_inline(op_pc, op_pc, false);
                self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
                self.asm.mov_rr(RDI, R_CTX);
                self.asm.mov_ri32(RSI, count as u32);
                self.asm
                    .mov_ri32(RDX, u32::from(matches!(op, Op::CallMethod(_))));
                self.asm.mov_ri32(RCX, next_pc as u32);
                self.asm.call_abs(helper_call as *const () as usize);
                self.emit_check_helper_status();
                self.block_start = next_pc;
            }
            Op::Return => {
                self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
                self.asm.mov_rr(RDI, R_CTX);
                self.asm.mov_ri32(RSI, next_pc as u32);
                self.asm.call_abs(helper_return as *const () as usize);
                self.asm.cmp_r32_imm(RAX, STATUS_DISPATCH_NEXT as i32);
                self.asm.jcc(CC_E, self.dispatch_next);
                self.asm.jmp(self.epilogue);
            }
            _ => {
                self.emit_step_op_call(op_pc, false);
            }
        }
    }

    fn emit_stubs(&mut self) {
        let stubs = std::mem::take(&mut self.stubs);
        for stub in stubs {
            match stub {
                Stub::StepOp {
                    label,
                    op_pc,
                    refresh_scopes,
                    cont,
                } => {
                    self.asm.bind(label);
                    self.emit_step_op_call(op_pc, refresh_scopes);
                    self.asm.jmp(cont);
                }
                Stub::TdzReg {
                    label,
                    name_or_reg,
                    from_layout,
                    next_pc,
                } => {
                    self.asm.bind(label);
                    self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
                    self.asm.mov_rr(RDI, R_CTX);
                    self.asm.mov_ri32(RSI, name_or_reg);
                    self.asm.mov_ri32(RDX, from_layout);
                    self.asm.mov_ri32(RCX, next_pc as u32);
                    self.asm.call_abs(helper_tdz_reg as *const () as usize);
                    self.asm.jmp(self.epilogue);
                }
                Stub::RequireCoercibleErr { label, next_pc } => {
                    self.asm.bind(label);
                    self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
                    self.asm.mov_rr(RDI, R_CTX);
                    self.asm.mov_ri32(RSI, next_pc as u32);
                    self.asm
                        .call_abs(helper_require_coercible_err as *const () as usize);
                    self.asm.jmp(self.epilogue);
                }
                Stub::Safepoint { label, pc, cont } => {
                    self.asm.bind(label);
                    self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
                    self.asm.mov_rr(RDI, R_CTX);
                    self.asm.mov_ri32(RSI, pc as u32);
                    self.asm.call_abs(helper_safepoint as *const () as usize);
                    self.asm.test_r32(RAX, RAX);
                    self.asm.jcc(CC_NE, self.epilogue);
                    self.asm.mov_r_mem(R_SP, R_CTX, OFF_CTX_SP);
                    self.asm.mov_r_mem(R_REGS, R_CTX, OFF_CTX_REGS);
                    self.asm.jmp(cont);
                }
                Stub::ReserveHeadroom { label, cont } => {
                    self.asm.bind(label);
                    self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
                    self.asm.mov_rr(RDI, R_CTX);
                    self.asm
                        .call_abs(helper_reserve_headroom as *const () as usize);
                    self.asm.mov_r_mem(R_SP, R_CTX, OFF_CTX_SP);
                    self.asm.mov_r_mem(R_REGS, R_CTX, OFF_CTX_REGS);
                    self.asm.jmp(cont);
                }
                Stub::GetPropSlow {
                    label,
                    op_pc,
                    name,
                    ic,
                    mode,
                    ic_ptr,
                    recv_reg_or_local,
                    cont,
                } => {
                    self.asm.bind(label);
                    let full_slow = self.asm.label();
                    // try zero-sync PIC leaf first (covers polymorphic OwnData, ProtoData,
                    // DeepProto, Missing, DeepMissing, Array.length, String.length)
                    match (mode, recv_reg_or_local) {
                        (0, None) => {
                            self.asm.mov_rr(RDI, R_RT);
                            self.asm.mov_rr(RSI, R_HEAP);
                            self.asm.mov_ri64(RDX, ic_ptr as u64);
                            self.asm.lea_mem(RCX, R_SP, -VAL_SIZE);
                            self.asm
                                .call_abs(helper_ic_get_leaf_replace as *const () as usize);
                            self.asm.test_r32(RAX, RAX);
                            self.asm.jcc(CC_NE, cont);
                        }
                        (1 | 2, Some((true, _, reg))) => {
                            let off = reg as i32 * VAL_SIZE;
                            self.asm.cmp_m8_imm(R_REGS, off, VALUE_TAG_UNINITIALIZED);
                            self.asm.jcc(CC_E, full_slow);
                            self.asm.mov_rr(RDI, R_RT);
                            self.asm.mov_rr(RSI, R_HEAP);
                            self.asm.mov_ri64(RDX, ic_ptr as u64);
                            self.asm.lea_mem(RCX, R_REGS, off);
                            self.asm.mov_rr(R8, R_SP);
                            let helper = if mode == 1 {
                                helper_ic_get_leaf_push as *const () as usize
                            } else {
                                helper_ic_get_leaf_method as *const () as usize
                            };
                            self.asm.call_abs(helper);
                            self.asm.test_r32(RAX, RAX);
                            self.asm.jcc(CC_E, full_slow);
                            self.asm
                                .add_ri(R_SP, if mode == 1 { VAL_SIZE } else { 2 * VAL_SIZE });
                            self.asm.jmp(cont);
                        }
                        (3 | 4, Some((false, depth, index))) => {
                            let off = index as i32 * VAL_SIZE;
                            self.emit_load_scope_ptr(depth, index, R10, full_slow);
                            self.asm.movzx_r_m8(RAX, R10, off);
                            self.asm.cmp_r8_imm(RAX, VALUE_TAG_UNINITIALIZED);
                            self.asm.jcc(CC_E, full_slow);
                            self.asm.cmp_r8_imm(RAX, VALUE_TAG_BINDING);
                            self.asm.jcc(CC_E, full_slow);
                            self.asm.cmp_r8_imm(RAX, VALUE_TAG_CONST_BINDING);
                            self.asm.jcc(CC_E, full_slow);
                            self.asm.mov_rr(RDI, R_RT);
                            self.asm.mov_rr(RSI, R_HEAP);
                            self.asm.mov_ri64(RDX, ic_ptr as u64);
                            self.asm.lea_mem(RCX, R10, off);
                            self.asm.mov_rr(R8, R_SP);
                            let helper = if mode == 3 {
                                helper_ic_get_leaf_push as *const () as usize
                            } else {
                                helper_ic_get_leaf_method as *const () as usize
                            };
                            self.asm.call_abs(helper);
                            self.asm.test_r32(RAX, RAX);
                            self.asm.jcc(CC_E, full_slow);
                            self.asm
                                .add_ri(R_SP, if mode == 3 { VAL_SIZE } else { 2 * VAL_SIZE });
                            self.asm.jmp(cont);
                        }
                        _ => {}
                    }
                    self.asm.bind(full_slow);
                    self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
                    self.asm.mov_rr(RDI, R_CTX);
                    self.asm.mov_ri32(RSI, op_pc as u32);
                    self.asm.mov_ri32(RDX, name);
                    self.asm.mov_ri32(RCX, ic);
                    self.asm.mov_ri32(R8, mode);
                    self.asm
                        .call_abs(helper_get_prop_slow as *const () as usize);
                    self.emit_check_helper_status();
                    self.asm.jmp(cont);
                }
                Stub::SetPropSlow {
                    label,
                    next_pc,
                    name,
                    ic,
                    pop,
                    ic_ptr,
                    cont,
                } => {
                    self.asm.bind(label);
                    let full_slow = self.asm.label();
                    // try zero-sync PIC set leaf first (polymorphic OwnData + OwnAdd)
                    self.asm.mov_rr(RDI, R_HEAP);
                    self.asm.mov_ri64(RSI, ic_ptr as u64);
                    self.asm.lea_mem(RDX, R_SP, -2 * VAL_SIZE);
                    self.asm.lea_mem(RCX, R_SP, -VAL_SIZE);
                    self.asm.mov_ri32(R8, pop);
                    self.asm.call_abs(helper_ic_set_leaf as *const () as usize);
                    self.asm.test_r32(RAX, RAX);
                    self.asm.jcc(CC_E, full_slow);
                    self.asm
                        .sub_ri(R_SP, if pop != 0 { 2 * VAL_SIZE } else { VAL_SIZE });
                    self.asm.jmp(cont);

                    self.asm.bind(full_slow);
                    self.asm.mov_mem_r(R_CTX, OFF_CTX_SP, R_SP);
                    self.asm.mov_rr(RDI, R_CTX);
                    self.asm.mov_ri32(RSI, next_pc as u32);
                    self.asm.mov_ri32(RDX, name);
                    self.asm.mov_ri32(RCX, ic);
                    self.asm.mov_ri32(R8, pop);
                    self.asm
                        .call_abs(helper_set_prop_slow as *const () as usize);
                    self.emit_check_helper_status();
                    self.asm.jmp(cont);
                }
            }
        }
    }

    fn finish(mut self) -> Option<JitCode> {
        self.asm.patch_labels();
        let pc_offsets: Box<[u32]> = self
            .pc_labels
            .iter()
            .map(|&lbl| self.asm.labels[lbl].expect("pc label bound") as u32)
            .collect();
        let buf = ExecBuf::from_bytes(&self.asm.code)?;
        Some(JitCode { buf, pc_offsets })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_jit_frame(
    rt: &mut ScriptRuntime,
    jit: &JitCode,
    heap_ptr: *const Heap,
    top: usize,
    pc: usize,
    frame_floor: usize,
    sampling: bool,
    safepoint_at: &mut u64,
) -> JitOutcome {
    let (reg_base, max_pushes) = {
        let frame = unsafe { rt.frames.get_unchecked(top) };
        (frame.reg_base, frame.code.max_pushes as usize)
    };
    let cap = rt.vm_stack.capacity();
    let len = rt.vm_stack.len();
    if cap - len < max_pushes {
        rt.vm_stack.reserve(max_pushes);
    }
    let stack_ptr = rt.vm_stack.as_mut_ptr();
    let cap = rt.vm_stack.capacity();
    let mut run = JitRun {
        ctx: JitCtx {
            rt: rt as *mut ScriptRuntime,
            heap: heap_ptr,
            stack_ptr,
            stack_safe_limit: unsafe { stack_ptr.add(cap.saturating_sub(max_pushes)) },
            sp: unsafe { stack_ptr.add(rt.vm_stack.len()) },
            regs: unsafe { stack_ptr.add(reg_base) },
            next_target: std::ptr::null(),
            scope_ptrs: [std::ptr::null_mut(); SCOPE_CACHE_DEPTH],
            scope_lens: [0; SCOPE_CACHE_DEPTH],
            steps: rt.steps,
            safepoint_at: *safepoint_at,
            reg_base,
            top,
            frame_floor,
            max_pushes,
            sampling: u8::from(sampling),
        },
        error: None,
        result: None,
        prev_code: None,
    };
    unsafe { refresh_scope_cache(&mut run.ctx, rt) };
    let entry = jit.entry();
    let target = jit.target(pc);
    let status = entry(&mut run, target);
    *safepoint_at = run.ctx.safepoint_at;
    rt.steps = run.ctx.steps;
    match status {
        STATUS_FRAME_CHANGED => JitOutcome::FrameChanged,
        STATUS_DONE => JitOutcome::Done(run.result.expect("jit done result")),
        STATUS_ERR => JitOutcome::Error(run.error.expect("jit error")),
        _ => unreachable!("unexpected jit exit status"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_js(source: &str) -> Result<String, ScriptError> {
        let document = crate::dom::Document::parse("<p id='out'></p>").expect("doc");
        let document = Rc::new(RefCell::new(document));
        let mut rt = ScriptRuntime::new(document.clone());
        rt.execute(source)?;
        let out = document.borrow().find_element_by_id("out").unwrap();
        Ok(document
            .borrow()
            .element_text_content(out)
            .unwrap_or_default())
    }

    #[test]
    fn jit_handles_hot_loops_bitwise_strings_and_polymorphic_ics() {
        let out = run_js(
            r#"
            function hotArith(n) {
                let s = 0;
                for (let i = 0; i < n; i++) {
                    s += ((i * 3) ^ (i >>> 1)) - ((i & 7) << 2);
                }
                return s;
            }
            function hotStr(a, b) {
                const c = a + "-" + b;
                const t = `${a}:${b}:${c.length}`;
                return (c === a + "-" + b) ? t : "bad";
            }
            const proto = { tag: "P", mul(x) { return x * 2; } };
            function makeA(v) { const o = Object.create(proto); o.x = v; return o; }
            function makeB(v) { const o = Object.create(proto); o.y = 1; o.x = v; return o; }
            function readPoly(o) {
                return o.x + (o.missing === undefined ? 1 : 0) + o.mul(3) + (o.tag === "P" ? 10 : 0);
            }
            let acc = 0;
            for (let i = 0; i < 40; i++) {
                acc += hotArith(20);
                const o = (i & 1) ? makeB(i) : makeA(i);
                acc += readPoly(o);
                if (hotStr("abc", String(i)) !== "abc:" + i + ":" + (4 + String(i).length)) {
                    throw new Error("str mismatch");
                }
            }
            document.getElementById("out").textContent = String(acc);
            "#,
        )
        .expect("jit script");
        let expected: i64 = (0..40)
            .map(|i| {
                let arith: i64 = (0..20).map(|j| ((j * 3) ^ (j >> 1)) - ((j & 7) << 2)).sum();
                arith + i + 1 + 6 + 10
            })
            .sum();
        assert_eq!(out, expected.to_string());
    }

    #[test]
    fn jit_handles_closures_block_scopes_bound_calls_and_try_finally() {
        let out = run_js(
            r#"
            function outer(base) {
                let fns = [];
                for (let i = 0; i < 30; i++) {
                    let captured = base + i;
                    fns.push(() => {
                        try {
                            if (i === 5) return captured * 10;
                            return captured;
                        } finally {
                            captured += 1;
                        }
                    });
                }
                let sum = 0;
                for (const f of fns) sum += f();
                return sum;
            }
            function add(a, b, c) { return this.k + a + b + c; }
            const bound = add.bind({ k: 100 }, 10);
            let total = 0;
            for (let r = 0; r < 25; r++) {
                total += outer(r) + bound(r, 2);
            }
            document.getElementById("out").textContent = String(total);
            "#,
        )
        .expect("jit closures");
        let expected: i64 = (0..25)
            .map(|r| {
                let outer_sum: i64 = (0..30)
                    .map(|i| if i == 5 { (r + i) * 10 } else { r + i })
                    .sum();
                outer_sum + 100 + 10 + r + 2
            })
            .sum();
        assert_eq!(out, expected.to_string());
    }

    #[test]
    fn jit_enforces_stack_limit_and_regex_repeat_split() {
        let out = run_js(
            r#"
            function rec(n) { return rec(n + 1); }
            let caught = "";
            try { rec(0); } catch (e) { caught = e.name; }
            const parts = "  alpha   beta\t\ngamma  ".split(/\s+/).join("|");
            const lim = "a   b   c".split(/\s+/, 2).join("|");
            const empty = "".split(/\s+/).length;
            document.getElementById("out").textContent = caught + ":" + parts + ":" + lim + ":" + empty;
            "#,
        )
        .expect("jit stack and split");
        assert_eq!(out, "RangeError:|alpha|beta|gamma|:a|b:1");
    }
}
