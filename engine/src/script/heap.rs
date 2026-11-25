// compact managed arena and mark-sweep GC heap for script objects, arrays and scopes:
// each runtime owns chunked slabs indexed by 8-byte Copy handles packing (heap_id, gen, slot),
// eliminating per-object Rc/RefCell/Weak allocations and reclaiming empty chunks in bulk

use super::object::{JsObject, ObjectClass, ObjectTemplate, Slot};
use super::vm::{Closure, Frame, Scope};
use super::*;
use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;
use std::num::NonZeroU64;
use std::ops::{Deref, DerefMut};

const CHUNK_SHIFT: usize = 10;
const CHUNK_SIZE: usize = 1 << CHUNK_SHIFT;
const CHUNK_MASK: usize = CHUNK_SIZE - 1;

const INDEX_MASK: u64 = 0x00FF_FFFF;
const GEN_SHIFT: u32 = 24;
const GEN_MASK: u64 = 0x00FF_FFFF;
const HEAP_SHIFT: u32 = 48;

const META_GEN_MASK: u32 = 0x00FF_FFFF;
const META_ALIVE: u32 = 1 << 24;
const META_MARKED: u32 = 1 << 25;
const META_PINNED: u32 = 1 << 26;

const MINIMUM_THRESHOLD: usize = 262_144;

#[derive(Debug)]
pub(super) struct BorrowError;

struct Entry<T> {
    meta: Cell<u32>,
    borrow: Cell<i32>,
    value: UnsafeCell<Option<T>>,
}

pub(super) struct GcRef<'a, T> {
    entry: *const Entry<T>,
    slab_borrows: *const Cell<usize>,
    _marker: PhantomData<&'a T>,
}

impl<T> Deref for GcRef<'_, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &T {
        // safety: entry is alive and shared-borrowed; GC never sweeps while active_borrows > 0
        unsafe { (*(*self.entry).value.get()).as_ref().unwrap_unchecked() }
    }
}

impl<T> Drop for GcRef<'_, T> {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            let entry = &*self.entry;
            entry.borrow.set(entry.borrow.get() - 1);
            let borrows = &*self.slab_borrows;
            borrows.set(borrows.get() - 1);
        }
    }
}

pub(super) struct GcRefMut<'a, T> {
    entry: *const Entry<T>,
    slab_borrows: *const Cell<usize>,
    _marker: PhantomData<&'a mut T>,
}

impl<T> Deref for GcRefMut<'_, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &T {
        unsafe { (*(*self.entry).value.get()).as_ref().unwrap_unchecked() }
    }
}

impl<T> DerefMut for GcRefMut<'_, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { (*(*self.entry).value.get()).as_mut().unwrap_unchecked() }
    }
}

impl<T> Drop for GcRefMut<'_, T> {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            (*self.entry).borrow.set(0);
            let borrows = &*self.slab_borrows;
            borrows.set(borrows.get() - 1);
        }
    }
}

struct Slab<T> {
    chunks: UnsafeCell<Vec<*mut Entry<T>>>,
    chunk_gen: UnsafeCell<Vec<u32>>,
    free_list: UnsafeCell<Vec<u32>>,
    live: Cell<usize>,
    active_borrows: Cell<usize>,
    pinned_chunks: Cell<usize>,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self {
            chunks: UnsafeCell::new(Vec::new()),
            chunk_gen: UnsafeCell::new(Vec::new()),
            free_list: UnsafeCell::new(Vec::new()),
            live: Cell::new(0),
            active_borrows: Cell::new(0),
            pinned_chunks: Cell::new(0),
        }
    }
}

impl<T> Drop for Slab<T> {
    fn drop(&mut self) {
        let chunks = self.chunks.get_mut();
        for &ptr in chunks.iter() {
            if !ptr.is_null() {
                let slice_ptr = std::ptr::slice_from_raw_parts_mut(ptr, CHUNK_SIZE);
                // safety: each non-null pointer came from Box::into_raw of length CHUNK_SIZE
                drop(unsafe { Box::from_raw(slice_ptr) });
            }
        }
    }
}

impl<T> Slab<T> {
    #[inline]
    fn alloc(&self, value: T) -> (u32, u32) {
        let index = match unsafe { (&mut *self.free_list.get()).pop() } {
            Some(index) => index,
            None => self.grow(),
        };
        let chunk = (index as usize) >> CHUNK_SHIFT;
        let offset = (index as usize) & CHUNK_MASK;
        let entry = unsafe {
            let base = *(&*self.chunks.get()).get_unchecked(chunk);
            &*base.add(offset)
        };
        let epoch = entry.meta.get() & META_GEN_MASK;
        debug_assert!(
            epoch != 0 && (entry.meta.get() & META_ALIVE) == 0 && entry.borrow.get() == 0
        );
        unsafe {
            *entry.value.get() = Some(value);
        }
        entry.meta.set(epoch | META_ALIVE);
        self.live.set(self.live.get() + 1);
        (index, epoch)
    }

    #[inline]
    fn free_slot(&self, index: u32, expected_gen: u32) -> bool {
        let Some(entry) = self.entry_if_alive(index, expected_gen) else {
            return false;
        };
        let entry = unsafe { &*entry };
        if entry.borrow.get() != 0 {
            return false;
        }
        let meta = entry.meta.get();
        if (meta & META_PINNED) != 0 {
            return false;
        }
        unsafe {
            *entry.value.get() = None;
        }
        let mut next_gen = ((meta & META_GEN_MASK) + 1) & META_GEN_MASK;
        if next_gen == 0 {
            next_gen = 1;
        }
        entry.meta.set(next_gen);
        self.live.set(self.live.get() - 1);
        unsafe {
            (&mut *self.free_list.get()).push(index);
        }
        true
    }

    #[cold]
    #[inline(never)]
    fn grow(&self) -> u32 {
        let chunks = unsafe { &mut *self.chunks.get() };
        let chunk_gen = unsafe { &mut *self.chunk_gen.get() };
        let free_list = unsafe { &mut *self.free_list.get() };
        let slot = match chunks.iter().position(|ptr| ptr.is_null()) {
            Some(slot) => slot,
            None => {
                let slot = chunks.len();
                assert!(
                    (slot << CHUNK_SHIFT) <= INDEX_MASK as usize,
                    "script arena exceeded slot limit"
                );
                chunks.push(std::ptr::null_mut());
                chunk_gen.push(1);
                slot
            }
        };
        let initial_gen = chunk_gen[slot];
        let mut entries: Vec<Entry<T>> = Vec::with_capacity(CHUNK_SIZE);
        for _ in 0..CHUNK_SIZE {
            entries.push(Entry {
                meta: Cell::new(initial_gen),
                borrow: Cell::new(0),
                value: UnsafeCell::new(None),
            });
        }
        let ptr = Box::into_raw(entries.into_boxed_slice()) as *mut Entry<T>;
        chunks[slot] = ptr;
        let base_index = (slot << CHUNK_SHIFT) as u32;
        free_list.reserve(CHUNK_SIZE - 1);
        for offset in (1..CHUNK_SIZE as u32).rev() {
            free_list.push(base_index + offset);
        }
        base_index
    }

    #[inline(always)]
    fn entry_if_alive(&self, index: u32, expected_gen: u32) -> Option<*const Entry<T>> {
        let chunk = (index as usize) >> CHUNK_SHIFT;
        let offset = (index as usize) & CHUNK_MASK;
        let chunks = unsafe { &*self.chunks.get() };
        let base = *chunks.get(chunk)?;
        if base.is_null() {
            return None;
        }
        let entry = unsafe { base.add(offset) };
        let meta = unsafe { (*entry).meta.get() };
        if (meta & (META_ALIVE | META_GEN_MASK)) == (META_ALIVE | expected_gen) {
            Some(entry)
        } else {
            None
        }
    }

    #[inline(always)]
    fn entry_live(&self, index: u32, expected_gen: u32) -> *const Entry<T> {
        let chunk = (index as usize) >> CHUNK_SHIFT;
        let offset = (index as usize) & CHUNK_MASK;
        let entry = unsafe {
            let base = *(&*self.chunks.get()).get_unchecked(chunk);
            base.add(offset)
        };
        debug_assert!({
            let meta = unsafe { (*entry).meta.get() };
            (meta & (META_ALIVE | META_GEN_MASK)) == (META_ALIVE | expected_gen)
        });
        entry
    }

    #[inline(always)]
    fn borrow<'a>(&'a self, index: u32, epoch: u32) -> GcRef<'a, T> {
        let entry = self.entry_live(index, epoch);
        let state = unsafe { (*entry).borrow.get() };
        assert!(state >= 0, "already mutably borrowed");
        unsafe { (*entry).borrow.set(state + 1) };
        self.active_borrows.set(self.active_borrows.get() + 1);
        GcRef {
            entry,
            slab_borrows: &self.active_borrows,
            _marker: PhantomData,
        }
    }

    #[inline(always)]
    fn try_borrow<'a>(&'a self, index: u32, epoch: u32) -> Result<GcRef<'a, T>, BorrowError> {
        let Some(entry) = self.entry_if_alive(index, epoch) else {
            return Err(BorrowError);
        };
        let state = unsafe { (*entry).borrow.get() };
        if state < 0 {
            return Err(BorrowError);
        }
        unsafe { (*entry).borrow.set(state + 1) };
        self.active_borrows.set(self.active_borrows.get() + 1);
        Ok(GcRef {
            entry,
            slab_borrows: &self.active_borrows,
            _marker: PhantomData,
        })
    }

    #[inline(always)]
    fn borrow_mut<'a>(&'a self, index: u32, epoch: u32) -> GcRefMut<'a, T> {
        let entry = self.entry_live(index, epoch);
        let state = unsafe { (*entry).borrow.get() };
        assert!(state == 0, "already borrowed");
        unsafe { (*entry).borrow.set(-1) };
        self.active_borrows.set(self.active_borrows.get() + 1);
        GcRefMut {
            entry,
            slab_borrows: &self.active_borrows,
            _marker: PhantomData,
        }
    }

    #[inline]
    fn is_alive(&self, index: u32, epoch: u32) -> bool {
        self.entry_if_alive(index, epoch).is_some()
    }

    #[inline]
    fn mark_entry(&self, index: u32, epoch: u32) -> Option<*const Entry<T>> {
        let entry = self.entry_if_alive(index, epoch)?;
        let meta = unsafe { (*entry).meta.get() };
        if (meta & META_MARKED) != 0 {
            return None;
        }
        unsafe { (*entry).meta.set(meta | META_MARKED) };
        Some(entry)
    }

    #[inline]
    fn is_marked(&self, index: u32, epoch: u32) -> bool {
        let Some(entry) = self.entry_if_alive(index, epoch) else {
            return false;
        };
        let meta = unsafe { (*entry).meta.get() };
        (meta & (META_MARKED | META_PINNED)) != 0
    }

    fn pin_all(&self) {
        let chunks = unsafe { &*self.chunks.get() };
        self.pinned_chunks.set(chunks.len());
        for &ptr in chunks {
            if ptr.is_null() {
                continue;
            }
            for offset in 0..CHUNK_SIZE {
                let entry = unsafe { &*ptr.add(offset) };
                let meta = entry.meta.get();
                if (meta & META_ALIVE) != 0 {
                    entry.meta.set(meta | META_PINNED);
                }
            }
        }
    }

    fn push_pinned(&self, out: &mut Vec<*const Entry<T>>) {
        let chunks = unsafe { &*self.chunks.get() };
        let limit = self.pinned_chunks.get().min(chunks.len());
        for &ptr in &chunks[..limit] {
            if ptr.is_null() {
                continue;
            }
            for offset in 0..CHUNK_SIZE {
                let entry = unsafe { ptr.add(offset) };
                let meta = unsafe { (*entry).meta.get() };
                if (meta & (META_ALIVE | META_PINNED)) == (META_ALIVE | META_PINNED)
                    && (meta & META_MARKED) == 0
                {
                    unsafe { (*entry).meta.set(meta | META_MARKED) };
                    out.push(entry);
                }
            }
        }
    }

    fn sweep(&self) -> usize {
        if self.active_borrows.get() != 0 {
            return 0;
        }
        let chunks = unsafe { &mut *self.chunks.get() };
        let chunk_gen = unsafe { &mut *self.chunk_gen.get() };
        let free_list = unsafe { &mut *self.free_list.get() };
        free_list.clear();
        let mut swept = 0usize;
        let mut total_live = 0usize;
        for (chunk_idx, ptr) in chunks.iter_mut().enumerate() {
            if ptr.is_null() {
                continue;
            }
            let base = *ptr;
            let mut survivors = 0usize;
            let mut max_gen = chunk_gen[chunk_idx];
            for offset in 0..CHUNK_SIZE {
                let entry = unsafe { &*base.add(offset) };
                let meta = entry.meta.get();
                if (meta & META_ALIVE) == 0 {
                    continue;
                }
                if (meta & (META_MARKED | META_PINNED)) != 0 {
                    entry.meta.set(meta & !META_MARKED);
                    survivors += 1;
                } else {
                    unsafe {
                        *entry.value.get() = None;
                    }
                    let mut next_gen = ((meta & META_GEN_MASK) + 1) & META_GEN_MASK;
                    if next_gen == 0 {
                        next_gen = 1;
                    }
                    max_gen = max_gen.max(next_gen);
                    entry.meta.set(next_gen);
                    swept += 1;
                }
            }
            if survivors == 0 {
                let slice_ptr = std::ptr::slice_from_raw_parts_mut(base, CHUNK_SIZE);
                drop(unsafe { Box::from_raw(slice_ptr) });
                *ptr = std::ptr::null_mut();
                let mut next_chunk_gen = (max_gen + 1) & META_GEN_MASK;
                if next_chunk_gen == 0 {
                    next_chunk_gen = 1;
                }
                chunk_gen[chunk_idx] = next_chunk_gen;
            } else {
                total_live += survivors;
                let base_index = (chunk_idx << CHUNK_SHIFT) as u32;
                for offset in 0..CHUNK_SIZE {
                    let entry = unsafe { &*base.add(offset) };
                    if (entry.meta.get() & META_ALIVE) == 0 {
                        free_list.push(base_index + offset as u32);
                    }
                }
            }
        }
        // pop from lowest chunks first so high chunks drain to zero and return to the OS
        free_list.reverse();
        free_list.shrink_to(free_list.len().max(4096));
        self.live.set(total_live);
        swept
    }
}

pub(super) struct Heap {
    id: u16,
    objects: Slab<JsObject>,
    arrays: Slab<ArrayData>,
    scopes: Slab<Scope>,
    allocs_since_gc: Cell<usize>,
    gc_threshold: Cell<usize>,
}

impl Heap {
    fn new(id: u16) -> Box<Self> {
        Box::new(Self {
            id,
            objects: Slab::default(),
            arrays: Slab::default(),
            scopes: Slab::default(),
            allocs_since_gc: Cell::new(0),
            gc_threshold: Cell::new(MINIMUM_THRESHOLD),
        })
    }

    #[inline]
    fn live_count(&self) -> usize {
        self.objects.live.get() + self.arrays.live.get() + self.scopes.live.get()
    }

    #[inline]
    fn active_borrows(&self) -> usize {
        self.objects.active_borrows.get()
            + self.arrays.active_borrows.get()
            + self.scopes.active_borrows.get()
    }

    // direct non-reentrant accessors used inside run_fast where no GC or overlapping borrow
    // can happen during the reference lifetime
    #[inline(always)]
    pub(super) unsafe fn scope_at_ref(&self, start: &ScopeRef, depth: u16) -> Option<&Scope> {
        let mut raw = start.0;
        let mut steps = depth;
        loop {
            let (heap_id, epoch, index) = unpack_handle(raw);
            if heap_id != self.id {
                return None;
            }
            let entry = self.scopes.entry_live(index, epoch);
            if unsafe { (*entry).borrow.get() } < 0 {
                return None;
            }
            let scope = unsafe { (*(*entry).value.get()).as_ref().unwrap_unchecked() };
            if steps == 0 {
                return Some(scope);
            }
            raw = scope.parent.as_ref()?.0;
            steps -= 1;
        }
    }

    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub(super) unsafe fn scope_at_mut(&self, start: &ScopeRef, depth: u16) -> Option<&mut Scope> {
        let mut raw = start.0;
        let mut steps = depth;
        loop {
            let (heap_id, epoch, index) = unpack_handle(raw);
            if heap_id != self.id {
                return None;
            }
            let entry = self.scopes.entry_live(index, epoch);
            if steps == 0 {
                if unsafe { (*entry).borrow.get() } != 0 {
                    return None;
                }
                return Some(unsafe { (*(*entry).value.get()).as_mut().unwrap_unchecked() });
            }
            if unsafe { (*entry).borrow.get() } < 0 {
                return None;
            }
            let scope = unsafe { (*(*entry).value.get()).as_ref().unwrap_unchecked() };
            raw = scope.parent.as_ref()?.0;
            steps -= 1;
        }
    }

    #[inline(always)]
    pub(super) unsafe fn object_ref(&self, handle: &ObjectHandle) -> Option<&JsObject> {
        let (heap_id, epoch, index) = unpack_handle(handle.0);
        if heap_id != self.id {
            return None;
        }
        let entry = self.objects.entry_live(index, epoch);
        if unsafe { (*entry).borrow.get() } < 0 {
            return None;
        }
        Some(unsafe { (*(*entry).value.get()).as_ref().unwrap_unchecked() })
    }

    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub(super) unsafe fn object_mut(&self, handle: &ObjectHandle) -> Option<&mut JsObject> {
        let (heap_id, epoch, index) = unpack_handle(handle.0);
        if heap_id != self.id {
            return None;
        }
        let entry = self.objects.entry_live(index, epoch);
        if unsafe { (*entry).borrow.get() } != 0 {
            return None;
        }
        Some(unsafe { (*(*entry).value.get()).as_mut().unwrap_unchecked() })
    }

    #[inline(always)]
    pub(super) unsafe fn array_ref(&self, handle: &ArrayHandle) -> Option<&ArrayData> {
        let (heap_id, epoch, index) = unpack_handle(handle.0);
        if heap_id != self.id {
            return None;
        }
        let entry = self.arrays.entry_live(index, epoch);
        if unsafe { (*entry).borrow.get() } < 0 {
            return None;
        }
        Some(unsafe { (*(*entry).value.get()).as_ref().unwrap_unchecked() })
    }

    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub(super) unsafe fn array_mut(&self, handle: &ArrayHandle) -> Option<&mut ArrayData> {
        let (heap_id, epoch, index) = unpack_handle(handle.0);
        if heap_id != self.id {
            return None;
        }
        let entry = self.arrays.entry_live(index, epoch);
        if unsafe { (*entry).borrow.get() } != 0 {
            return None;
        }
        Some(unsafe { (*(*entry).value.get()).as_mut().unwrap_unchecked() })
    }

    #[inline(always)]
    pub(super) fn alloc_object_fast(&self, object: JsObject) -> ObjectHandle {
        let (index, epoch) = self.objects.alloc(object);
        self.allocs_since_gc.set(self.allocs_since_gc.get() + 1);
        ObjectHandle(pack_handle(self.id, epoch, index))
    }

    #[inline(always)]
    pub(super) fn alloc_array_fast(&self, array: ArrayData) -> ArrayHandle {
        let (index, epoch) = self.arrays.alloc(array);
        self.allocs_since_gc.set(self.allocs_since_gc.get() + 1);
        ArrayHandle(pack_handle(self.id, epoch, index))
    }

    #[inline(always)]
    pub(super) fn alloc_scope_fast(&self, scope: Scope) -> ScopeRef {
        let (index, epoch) = self.scopes.alloc(scope);
        self.allocs_since_gc.set(self.allocs_since_gc.get() + 1);
        ScopeRef(pack_handle(self.id, epoch, index))
    }

    #[inline(always)]
    pub(super) fn free_scope_fast(&self, scope: ScopeRef) {
        let (heap_id, epoch, index) = unpack_handle(scope.0);
        if heap_id == self.id {
            if self.scopes.free_slot(index, epoch) {
                let count = self.allocs_since_gc.get();
                if count > 0 {
                    self.allocs_since_gc.set(count - 1);
                }
            }
        } else {
            free_scope(scope);
        }
    }
}

#[inline(always)]
pub(super) fn heap_ptr(id: u16) -> *const Heap {
    heap_for(id).expect("runtime heap is alive")
}

thread_local! {
    static HEAPS: UnsafeCell<Vec<Option<Box<Heap>>>> = const { UnsafeCell::new(Vec::new()) };
    static ACTIVE_ID: Cell<u16> = const { Cell::new(0) };
    static ACTIVE_PTR: Cell<*const Heap> = const { Cell::new(std::ptr::null()) };
}

#[inline(always)]
fn pack_handle(heap_id: u16, epoch: u32, index: u32) -> NonZeroU64 {
    debug_assert!(epoch != 0 && (epoch & !META_GEN_MASK) == 0);
    debug_assert!((index as u64 & !INDEX_MASK) == 0);
    let raw = ((heap_id as u64) << HEAP_SHIFT) | ((epoch as u64) << GEN_SHIFT) | (index as u64);
    unsafe { NonZeroU64::new_unchecked(raw) }
}

#[inline(always)]
fn unpack_handle(raw: NonZeroU64) -> (u16, u32, u32) {
    let bits = raw.get();
    let heap_id = (bits >> HEAP_SHIFT) as u16;
    let epoch = ((bits >> GEN_SHIFT) & GEN_MASK) as u32;
    let index = (bits & INDEX_MASK) as u32;
    (heap_id, epoch, index)
}

#[inline(always)]
fn active_heap() -> *const Heap {
    let ptr = ACTIVE_PTR.get();
    if !ptr.is_null() {
        return ptr;
    }
    ensure_default_heap()
}

#[cold]
#[inline(never)]
fn ensure_default_heap() -> *const Heap {
    HEAPS.with(|cell| {
        let heaps = unsafe { &mut *cell.get() };
        if heaps.is_empty() {
            heaps.push(Some(Heap::new(0)));
        } else if heaps[0].is_none() {
            heaps[0] = Some(Heap::new(0));
        }
        let ptr: *const Heap = &**heaps[0].as_ref().expect("default heap initialized");
        ACTIVE_ID.set(0);
        ACTIVE_PTR.set(ptr);
        ptr
    })
}

#[inline(always)]
fn heap_for(id: u16) -> Option<*const Heap> {
    if ACTIVE_ID.get() == id {
        let ptr = ACTIVE_PTR.get();
        if !ptr.is_null() {
            return Some(ptr);
        }
    }
    heap_for_slow(id)
}

#[cold]
#[inline(never)]
fn heap_for_slow(id: u16) -> Option<*const Heap> {
    HEAPS.with(|cell| {
        let heaps = unsafe { &*cell.get() };
        heaps
            .get(id as usize)
            .and_then(|slot| slot.as_deref())
            .map(|heap| heap as *const Heap)
    })
}

pub(super) struct HeapGuard {
    prev_id: u16,
    restore: bool,
}

impl Drop for HeapGuard {
    #[inline(always)]
    fn drop(&mut self) {
        if self.restore {
            let ptr = heap_for_slow(self.prev_id).unwrap_or(std::ptr::null());
            ACTIVE_ID.set(if ptr.is_null() { 0 } else { self.prev_id });
            ACTIVE_PTR.set(ptr);
        }
    }
}

#[inline]
pub(super) fn enter(id: u16) -> HeapGuard {
    let prev_id = ACTIVE_ID.get();
    let prev_ptr = ACTIVE_PTR.get();
    if prev_id == id && !prev_ptr.is_null() {
        return HeapGuard {
            prev_id: 0,
            restore: false,
        };
    }
    let ptr = heap_for_slow(id).unwrap_or(std::ptr::null());
    ACTIVE_ID.set(if ptr.is_null() { 0 } else { id });
    ACTIVE_PTR.set(ptr);
    HeapGuard {
        prev_id,
        restore: true,
    }
}

pub(super) fn create_heap() -> u16 {
    HEAPS.with(|cell| {
        let heaps = unsafe { &mut *cell.get() };
        if heaps.is_empty() {
            heaps.push(None);
        }
        let id = match heaps
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, slot)| slot.is_none())
        {
            Some((slot, _)) => slot as u16,
            None => {
                let slot = heaps.len();
                assert!(slot <= u16::MAX as usize, "too many live script runtimes");
                heaps.push(None);
                slot as u16
            }
        };
        heaps[id as usize] = Some(Heap::new(id));
        id
    })
}

pub(super) fn destroy_heap(id: u16) -> usize {
    if id == 0 {
        return 0;
    }
    let taken = HEAPS.with(|cell| {
        let heaps = unsafe { &mut *cell.get() };
        let taken = heaps.get_mut(id as usize).and_then(Option::take);
        if ACTIVE_ID.get() == id {
            ACTIVE_ID.set(0);
            ACTIVE_PTR.set(std::ptr::null());
        }
        taken
    });
    match taken {
        Some(heap) => {
            let cleared = heap.live_count();
            drop(heap);
            cleared
        }
        None => 0,
    }
}

pub(super) fn pin_current(id: u16) {
    if let Some(ptr) = heap_for(id) {
        let heap = unsafe { &*ptr };
        heap.objects.pin_all();
        heap.arrays.pin_all();
        heap.scopes.pin_all();
        heap.allocs_since_gc.set(0);
    }
}

#[inline]
pub(super) fn gc_due(id: u16) -> bool {
    let Some(ptr) = heap_for(id) else {
        return false;
    };
    let heap = unsafe { &*ptr };
    heap.allocs_since_gc.get() >= heap.gc_threshold.get()
}

#[inline]
pub(super) fn allocs_since_gc(id: u16) -> usize {
    heap_for(id).map_or(0, |ptr| unsafe { (*ptr).allocs_since_gc.get() })
}

#[inline]
pub(super) fn live_count(id: u16) -> usize {
    heap_for(id).map_or(0, |ptr| unsafe { (*ptr).live_count() })
}

#[inline]
pub(super) fn alloc_object(object: JsObject) -> ObjectHandle {
    let heap = unsafe { &*active_heap() };
    let (index, epoch) = heap.objects.alloc(object);
    heap.allocs_since_gc.set(heap.allocs_since_gc.get() + 1);
    ObjectHandle(pack_handle(heap.id, epoch, index))
}

#[inline]
pub(super) fn alloc_array(array: ArrayData) -> ArrayHandle {
    let heap = unsafe { &*active_heap() };
    let (index, epoch) = heap.arrays.alloc(array);
    heap.allocs_since_gc.set(heap.allocs_since_gc.get() + 1);
    ArrayHandle(pack_handle(heap.id, epoch, index))
}

#[inline]
pub(super) fn alloc_scope(scope: Scope) -> ScopeRef {
    let heap = unsafe { &*active_heap() };
    let (index, epoch) = heap.scopes.alloc(scope);
    heap.allocs_since_gc.set(heap.allocs_since_gc.get() + 1);
    ScopeRef(pack_handle(heap.id, epoch, index))
}

#[inline]
pub(super) fn free_scope(scope: ScopeRef) {
    let (heap_id, epoch, index) = unpack_handle(scope.0);
    if let Some(ptr) = heap_for(heap_id) {
        let heap = unsafe { &*ptr };
        if heap.scopes.free_slot(index, epoch) {
            let count = heap.allocs_since_gc.get();
            if count > 0 {
                heap.allocs_since_gc.set(count - 1);
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) struct ObjectHandle(NonZeroU64);

impl ObjectHandle {
    #[inline(always)]
    pub(super) fn borrow(&self) -> GcRef<'_, JsObject> {
        let (heap_id, epoch, index) = unpack_handle(self.0);
        let heap = unsafe { &*heap_for(heap_id).expect("object heap is alive") };
        heap.objects.borrow(index, epoch)
    }

    #[inline(always)]
    pub(super) fn borrow_mut(&self) -> GcRefMut<'_, JsObject> {
        let (heap_id, epoch, index) = unpack_handle(self.0);
        let heap = unsafe { &*heap_for(heap_id).expect("object heap is alive") };
        heap.objects.borrow_mut(index, epoch)
    }

    #[inline(always)]
    pub(super) fn try_borrow(&self) -> Result<GcRef<'_, JsObject>, BorrowError> {
        let (heap_id, epoch, index) = unpack_handle(self.0);
        let Some(ptr) = heap_for(heap_id) else {
            return Err(BorrowError);
        };
        unsafe { (*ptr).objects.try_borrow(index, epoch) }
    }

    #[inline(always)]
    pub(super) fn ptr_eq(left: &Self, right: &Self) -> bool {
        left.0 == right.0
    }

    #[inline(always)]
    pub(super) fn id(&self) -> usize {
        self.0.get() as usize
    }

    #[inline(always)]
    pub(super) fn downgrade(&self) -> ObjectWeak {
        ObjectWeak(self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct ObjectWeak(NonZeroU64);

impl ObjectWeak {
    #[inline(always)]
    pub(super) fn upgrade(&self) -> Option<ObjectHandle> {
        let (heap_id, epoch, index) = unpack_handle(self.0);
        let ptr = heap_for(heap_id)?;
        unsafe { (*ptr).objects.is_alive(index, epoch) }.then_some(ObjectHandle(self.0))
    }

    #[inline(always)]
    pub(super) fn is_alive(&self) -> bool {
        let (heap_id, epoch, index) = unpack_handle(self.0);
        let Some(ptr) = heap_for(heap_id) else {
            return false;
        };
        unsafe { (*ptr).objects.is_alive(index, epoch) }
    }

    #[inline(always)]
    pub(super) fn matches(&self, handle: &ObjectHandle) -> bool {
        self.0 == handle.0
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) struct ArrayHandle(NonZeroU64);

impl ArrayHandle {
    #[inline(always)]
    pub(super) fn borrow(&self) -> GcRef<'_, ArrayData> {
        let (heap_id, epoch, index) = unpack_handle(self.0);
        let heap = unsafe { &*heap_for(heap_id).expect("array heap is alive") };
        heap.arrays.borrow(index, epoch)
    }

    #[inline(always)]
    pub(super) fn borrow_mut(&self) -> GcRefMut<'_, ArrayData> {
        let (heap_id, epoch, index) = unpack_handle(self.0);
        let heap = unsafe { &*heap_for(heap_id).expect("array heap is alive") };
        heap.arrays.borrow_mut(index, epoch)
    }

    #[inline(always)]
    pub(super) fn ptr_eq(left: &Self, right: &Self) -> bool {
        left.0 == right.0
    }

    #[inline(always)]
    pub(super) fn id(&self) -> usize {
        self.0.get() as usize
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) struct ScopeRef(NonZeroU64);

impl ScopeRef {
    #[inline(always)]
    pub(super) fn borrow(&self) -> GcRef<'_, Scope> {
        let (heap_id, epoch, index) = unpack_handle(self.0);
        let heap = unsafe { &*heap_for(heap_id).expect("scope heap is alive") };
        heap.scopes.borrow(index, epoch)
    }

    #[inline(always)]
    pub(super) fn borrow_mut(&self) -> GcRefMut<'_, Scope> {
        let (heap_id, epoch, index) = unpack_handle(self.0);
        let heap = unsafe { &*heap_for(heap_id).expect("scope heap is alive") };
        heap.scopes.borrow_mut(index, epoch)
    }
}

type RcSet =
    std::collections::HashSet<usize, std::hash::BuildHasherDefault<super::object::KeyHasher>>;

pub(super) struct Tracer {
    heap: *const Heap,
    heap_id: u16,
    objects: Vec<*const Entry<JsObject>>,
    arrays: Vec<*const Entry<ArrayData>>,
    scopes: Vec<*const Entry<Scope>>,
    weak_maps: Vec<Rc<RefCell<super::collections::Collection>>>,
    seen_rc: RcSet,
}

impl Tracer {
    fn new(heap: *const Heap, heap_id: u16) -> Self {
        Self {
            heap,
            heap_id,
            objects: Vec::with_capacity(256),
            arrays: Vec::with_capacity(64),
            scopes: Vec::with_capacity(64),
            weak_maps: Vec::new(),
            seen_rc: RcSet::default(),
        }
    }

    #[inline]
    pub(super) fn visit_rc<T: ?Sized>(&mut self, rc: &Rc<T>) -> bool {
        self.seen_rc.insert(Rc::as_ptr(rc) as *const () as usize)
    }

    #[inline]
    pub(super) fn mark_object(&mut self, handle: ObjectHandle) {
        let (heap_id, epoch, index) = unpack_handle(handle.0);
        if heap_id != self.heap_id {
            return;
        }
        let heap = unsafe { &*self.heap };
        if let Some(entry) = heap.objects.mark_entry(index, epoch) {
            self.objects.push(entry);
        }
    }

    #[inline]
    pub(super) fn mark_array(&mut self, handle: ArrayHandle) {
        let (heap_id, epoch, index) = unpack_handle(handle.0);
        if heap_id != self.heap_id {
            return;
        }
        let heap = unsafe { &*self.heap };
        if let Some(entry) = heap.arrays.mark_entry(index, epoch) {
            self.arrays.push(entry);
        }
    }

    #[inline]
    pub(super) fn mark_scope(&mut self, handle: ScopeRef) {
        let (heap_id, epoch, index) = unpack_handle(handle.0);
        if heap_id != self.heap_id {
            return;
        }
        let heap = unsafe { &*self.heap };
        if let Some(entry) = heap.scopes.mark_entry(index, epoch) {
            self.scopes.push(entry);
        }
    }

    #[inline]
    pub(super) fn is_object_marked(&self, weak: ObjectWeak) -> bool {
        let (heap_id, epoch, index) = unpack_handle(weak.0);
        if heap_id != self.heap_id {
            return true;
        }
        unsafe { (*self.heap).objects.is_marked(index, epoch) }
    }

    #[inline]
    pub(super) fn trace_slot(&mut self, slot: &Slot) {
        match slot {
            Slot::Data(value) => self.trace_value(value),
            Slot::Accessor(accessors) => {
                if let Some(get) = &accessors.get {
                    self.trace_value(get);
                }
                if let Some(set) = &accessors.set {
                    self.trace_value(set);
                }
            }
        }
    }

    pub(super) fn trace_object_template(&mut self, template: &ObjectTemplate) {
        for slot in template.slots() {
            self.trace_slot(slot);
        }
    }

    pub(super) fn trace_element(&mut self, handle: &ElementHandle) {
        self.mark_object(handle.object.clone());
    }

    pub(super) fn trace_promise(&mut self, promise: &PromiseHandle) {
        if !self.visit_rc(promise) {
            return;
        }
        let data = promise.borrow();
        match &data.outcome {
            PromiseOutcome::Pending => {}
            PromiseOutcome::Fulfilled(value) | PromiseOutcome::Rejected(value) => {
                self.trace_value(value);
            }
        }
        for reaction in &data.reactions {
            if let Some(fulfilled) = &reaction.fulfilled {
                self.trace_value(fulfilled);
            }
            if let Some(rejected) = &reaction.rejected {
                self.trace_value(rejected);
            }
            self.trace_promise(&reaction.next);
        }
        if let Some(object) = &data.object {
            self.mark_object(object.clone());
        }
    }

    pub(super) fn trace_abort_signal(&mut self, signal: &AbortSignalHandle) {
        if !self.visit_rc(signal) {
            return;
        }
        let data = signal.borrow();
        if let Some(reason) = &data.reason {
            self.trace_value(reason);
        }
        for listener in &data.listeners {
            self.trace_value(listener);
        }
        if let Some(onabort) = &data.onabort {
            self.trace_value(onabort);
        }
    }

    pub(super) fn trace_event(&mut self, event: &Rc<events::EventRecord>) {
        if !self.visit_rc(event) {
            return;
        }
        event.trace(self);
    }

    pub(super) fn trace_closure(&mut self, closure: &Rc<Closure>) {
        if let Some(scope) = &closure.scope {
            self.mark_scope(scope.clone());
        }
        closure.trace_meta(self);
    }

    pub(super) fn trace_frame(&mut self, frame: &Frame) {
        frame.trace(self);
    }

    pub(super) fn trace_worker(&mut self, worker: &WorkerHandle) {
        if !self.visit_rc(worker) {
            return;
        }
        let data = worker.borrow();
        if let Some(cb) = &data.onmessage {
            self.trace_value(cb);
        }
        if let Some(cb) = &data.onerror {
            self.trace_value(cb);
        }
        for listener in &data.listeners {
            self.trace_value(&listener.callback);
            if let Some(signal) = &listener.signal {
                self.trace_abort_signal(signal);
            }
        }
    }

    pub(super) fn trace_typed_array(&mut self, array: &TypedArrayHandle) {
        if !self.visit_rc(array) {
            return;
        }
        if let Some(object) = &array.borrow().object {
            self.mark_object(object.clone());
        }
    }

    pub(super) fn trace_idb_request(&mut self, req: &IndexedDbRequestHandle) {
        if !self.visit_rc(req) {
            return;
        }
        let data = req.borrow();
        self.trace_value(&data.result);
        if let Some(err) = &data.error {
            self.trace_value(err);
        }
        for cb in [
            &data.on_success,
            &data.on_error,
            &data.on_upgrade_needed,
            &data.on_blocked,
        ]
        .into_iter()
        .flatten()
        {
            self.trace_value(cb);
        }
    }

    pub(super) fn trace_idb_tx(&mut self, tx: &IndexedDbTransactionHandle) {
        if !self.visit_rc(tx) {
            return;
        }
        let data = tx.borrow();
        for cb in [&data.on_complete, &data.on_error, &data.on_abort]
            .into_iter()
            .flatten()
        {
            self.trace_value(cb);
        }
        self.trace_idb_db(&data.database);
    }

    pub(super) fn trace_idb_db(&mut self, db: &IndexedDbDatabaseHandle) {
        if !self.visit_rc(db) {
            return;
        }
        let data = db.borrow();
        if let Some(cb) = &data.on_version_change {
            self.trace_value(cb);
        }
    }

    pub(super) fn trace_value(&mut self, value: &Value) {
        match value {
            Value::Undefined
            | Value::Uninitialized
            | Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Symbol(_)
            | Value::BigInt(_)
            | Value::Error(_)
            | Value::ResponseConstructor
            | Value::Bytes(_)
            | Value::Headers(_)
            | Value::Document
            | Value::Window
            | Value::Location(_)
            | Value::Navigator
            | Value::Clipboard
            | Value::Crypto
            | Value::SubtleCrypto
            | Value::Storage(_)
            | Value::FormData(_)
            | Value::Blob(_)
            | Value::File(_)
            | Value::FileList(_)
            | Value::WebAssemblyNamespace
            | Value::HeadersConstructor
            | Value::RequestConstructor
            | Value::FormDataConstructor
            | Value::BlobConstructor
            | Value::FileConstructor
            | Value::AbortControllerConstructor
            | Value::WorkerConstructor
            | Value::CacheStorage(_)
            | Value::Cache(_)
            | Value::CanvasContext(_)
            | Value::IndexedDbFactory(_)
            | Value::IndexedDbNameList(_) => {}
            Value::Object(handle) => self.mark_object(handle.clone()),
            Value::Array(handle) => self.mark_array(handle.clone()),
            Value::Binding(cell) | Value::ConstBinding(cell) => {
                if self.visit_rc(cell) {
                    self.trace_value(&cell.borrow());
                }
            }
            Value::Element(handle)
            | Value::Dataset(handle)
            | Value::Style(handle)
            | Value::ClassList(handle)
            | Value::AnimatedClass(handle) => self.trace_element(handle),
            Value::NodeList(list) => {
                if self.visit_rc(list) {
                    for handle in list.iter() {
                        self.trace_element(handle);
                    }
                }
            }
            Value::Iterator(iter) => {
                if self.visit_rc(iter) {
                    for value in &iter.borrow().values {
                        self.trace_value(value);
                    }
                }
            }
            Value::Validity(handle) => self.trace_element(handle),
            Value::Range(range) => self.mark_object(range.object.clone()),
            Value::Selection(selection) => {
                self.mark_object(selection.object.clone());
                if let Some(range) = selection.range() {
                    self.mark_object(range.object.clone());
                }
            }
            Value::Event(event) => self.trace_event(event),
            Value::Response(response) => {
                if let Some(object) = &response.stream.borrow().object {
                    self.mark_object(object.clone());
                }
            }
            Value::Request(request) => {
                if let Some(signal) = &request.signal {
                    self.trace_abort_signal(signal);
                }
                if let Some(object) = &request.stream.borrow().object {
                    self.mark_object(object.clone());
                }
            }
            Value::TypedArray(array) => self.trace_typed_array(array),
            Value::Promise(promise) => self.trace_promise(promise),
            Value::History(history) => {
                if self.visit_rc(history) {
                    self.trace_value(&history.borrow().state);
                }
            }
            Value::AbortController(ctrl) => self.trace_abort_signal(&ctrl.borrow().signal),
            Value::AbortSignal(signal) => self.trace_abort_signal(signal),
            Value::Worker(worker) => self.trace_worker(worker),
            Value::IndexedDbRequest(req) => self.trace_idb_request(req),
            Value::IndexedDbDatabase(db) => self.trace_idb_db(db),
            Value::IndexedDbTransaction(tx) => self.trace_idb_tx(tx),
            Value::IndexedDbObjectStore(store) => {
                if self.visit_rc(store)
                    && let Some(tx) = &store.borrow().transaction
                {
                    self.trace_idb_tx(tx);
                }
            }
            Value::VmIterator(iter) => {
                if self.visit_rc(iter) {
                    iter.borrow().trace(self);
                }
            }
            Value::Builtin(builtin) => self.trace_builtin(builtin),
        }
    }

    fn trace_builtin(&mut self, builtin: &Builtin) {
        match builtin {
            Builtin::CeriInvoke
            | Builtin::CeriListen
            | Builtin::CeriUnlisten(_)
            | Builtin::CeriEmit
            | Builtin::DocumentGetElementById
            | Builtin::DocumentQuerySelector
            | Builtin::DocumentQuerySelectorAll
            | Builtin::DocumentCreateElement
            | Builtin::DocumentCreateTextNode
            | Builtin::DocumentImportNode
            | Builtin::DocumentAdoptNode
            | Builtin::DocumentCreateRange
            | Builtin::DocumentCreateTreeWalker
            | Builtin::DocumentCreateNodeIterator
            | Builtin::DocumentGetSelection
            | Builtin::LocationAssign
            | Builtin::LocationReplace
            | Builtin::LocationReload
            | Builtin::WindowScrollTo
            | Builtin::WindowScrollBy
            | Builtin::WindowAlert
            | Builtin::WindowConfirm
            | Builtin::WindowPrompt
            | Builtin::WindowOpen
            | Builtin::WindowGetSelection
            | Builtin::ClipboardReadText
            | Builtin::ClipboardWriteText
            | Builtin::HistoryPushState
            | Builtin::HistoryReplaceState
            | Builtin::HistoryBack
            | Builtin::HistoryForward
            | Builtin::HistoryGo
            | Builtin::CanvasMethod(_)
            | Builtin::StorageClear(_)
            | Builtin::StorageGetItem(_)
            | Builtin::StorageKey(_)
            | Builtin::StorageRemoveItem(_)
            | Builtin::StorageSetItem(_)
            | Builtin::CacheStorageOpen(_)
            | Builtin::CacheStorageHas(_)
            | Builtin::CacheStorageDelete(_)
            | Builtin::CacheStorageKeys(_)
            | Builtin::CacheStorageMatch(_)
            | Builtin::CachePut(_)
            | Builtin::CacheMatch(_)
            | Builtin::CacheDelete(_)
            | Builtin::CacheKeys(_)
            | Builtin::IndexedDbOpen(_)
            | Builtin::IndexedDbDeleteDatabase(_)
            | Builtin::IndexedDbFactoryDatabases(_)
            | Builtin::IndexedDbNameListContains(_)
            | Builtin::IndexedDbNameListItem(_)
            | Builtin::FormDataAppend(_)
            | Builtin::FormDataDelete(_)
            | Builtin::FormDataGet(_)
            | Builtin::FormDataGetAll(_)
            | Builtin::FormDataHas(_)
            | Builtin::FormDataSet(_)
            | Builtin::FormDataKeys(_)
            | Builtin::FormDataValues(_)
            | Builtin::FormDataEntries(_)
            | Builtin::FormDataForEach(_)
            | Builtin::BlobSlice(_)
            | Builtin::BlobText(_)
            | Builtin::BlobStream(_)
            | Builtin::BlobBytes(_)
            | Builtin::BlobUint8(_)
            | Builtin::FileSlice(_)
            | Builtin::FileText(_)
            | Builtin::FileStream(_)
            | Builtin::FileBytes(_)
            | Builtin::FileUint8(_)
            | Builtin::FileListItem(_)
            | Builtin::StructuredClone
            | Builtin::Atob
            | Builtin::Btoa
            | Builtin::CreateImageBitmap
            | Builtin::WindowPostMessage
            | Builtin::DocumentGetAnimations
            | Builtin::ImageBitmapClose(_)
            | Builtin::SetTimeout
            | Builtin::ClearTimeout
            | Builtin::SetInterval
            | Builtin::QueueMicrotask
            | Builtin::RequestAnimationFrame
            | Builtin::CancelAnimationFrame
            | Builtin::CeriQuery
            | Builtin::ResponseErrorConstructor
            | Builtin::ResponseJsonConstructor
            | Builtin::ResponseRedirectConstructor
            | Builtin::NumberIsFinite
            | Builtin::ArrayIsArray
            | Builtin::ElementClassListSupports
            | Builtin::Fetch
            | Builtin::SubtleDigest
            | Builtin::CryptoGetRandomValues
            | Builtin::CryptoRandomUuid
            | Builtin::HeadersAppend(_)
            | Builtin::HeadersDelete(_)
            | Builtin::HeadersGet(_)
            | Builtin::HeadersHas(_)
            | Builtin::HeadersKeys(_)
            | Builtin::HeadersValues(_)
            | Builtin::HeadersEntries(_)
            | Builtin::HeadersForEach(_)
            | Builtin::HeadersSet(_)
            | Builtin::CanvasGradientAddColorStop(_)
            | Builtin::CanvasPatternSetTransform(_)
            | Builtin::ClipboardGetData(_)
            | Builtin::ClipboardSetData(_)
            | Builtin::ClipboardClearData(_)
            | Builtin::ClipboardTypes(_)
            | Builtin::ClipboardItems(_)
            | Builtin::ClipboardItemString(_)
            | Builtin::ClipboardItemFile(_)
            | Builtin::ClipboardItemConstructor
            | Builtin::ClipboardItemGetType(_)
            | Builtin::ClipboardRead
            | Builtin::ClipboardWrite
            | Builtin::AbortSignalAbort
            | Builtin::AbortSignalTimeout
            | Builtin::AbortSignalAny
            | Builtin::AbortSignalHasInstance
            | Builtin::WorkerGlobalImportScripts
            | Builtin::WorkerGlobalPostMessage(_)
            | Builtin::WebAssemblyInstantiate
            | Builtin::WebAssemblyFunction(_) => {}
            Builtin::ReturnValue(value) => self.trace_value(value),
            Builtin::Native(native) => self.trace_native(native),
            Builtin::ElementFocus(h)
            | Builtin::ElementBlur(h)
            | Builtin::ElementRequestSubmit(h)
            | Builtin::ElementReset(h)
            | Builtin::ElementCheckValidity(h)
            | Builtin::ElementReportValidity(h)
            | Builtin::ElementSetCustomValidity(h)
            | Builtin::CanvasGetContext(h)
            | Builtin::ElementAnimate(h)
            | Builtin::ElementGetAnimations(h)
            | Builtin::ElementClick(h)
            | Builtin::ElementAppendChild(h)
            | Builtin::ElementAppend(h)
            | Builtin::ElementPrepend(h)
            | Builtin::ElementReplaceChildren(h)
            | Builtin::ElementBefore(h)
            | Builtin::ElementAfter(h)
            | Builtin::ElementReplaceWith(h)
            | Builtin::ElementInsertAdjacentHtml(h)
            | Builtin::ElementRemoveChild(h)
            | Builtin::ElementInsertBefore(h)
            | Builtin::ElementRemove(h)
            | Builtin::ElementHasChildNodes(h)
            | Builtin::ElementCloneNode(h)
            | Builtin::ElementSelect(h)
            | Builtin::ElementSetSelectionRange(h)
            | Builtin::ElementSetRangeText(h)
            | Builtin::ElementSplitText(h)
            | Builtin::ElementNormalize(h)
            | Builtin::ElementSubstringData(h)
            | Builtin::ElementAppendData(h)
            | Builtin::ElementInsertData(h)
            | Builtin::ElementDeleteData(h)
            | Builtin::ElementReplaceData(h)
            | Builtin::ElementSetAttribute(h)
            | Builtin::ElementRemoveAttribute(h)
            | Builtin::ElementGetAttribute(h)
            | Builtin::ElementQuerySelector(h)
            | Builtin::ElementQuerySelectorAll(h)
            | Builtin::ElementClassListAdd(h)
            | Builtin::ElementClassListRemove(h)
            | Builtin::ElementClassListContains(h)
            | Builtin::ElementClassListToggle(h)
            | Builtin::ElementClassListItem(h)
            | Builtin::ElementClassListReplace(h)
            | Builtin::ElementClassListToString(h)
            | Builtin::ElementClassListForEach(h)
            | Builtin::StyleGetPropertyValue(h)
            | Builtin::StyleSetProperty(h)
            | Builtin::StyleRemoveProperty(h) => self.trace_element(h),
            Builtin::ElementClassListWalk(boxed) => self.trace_element(&boxed.0),
            Builtin::NodeListItem(list)
            | Builtin::NodeListForEach(list)
            | Builtin::NodeListKeys(list)
            | Builtin::NodeListValues(list)
            | Builtin::NodeListEntries(list) => {
                if self.visit_rc(list) {
                    for handle in list.iter() {
                        self.trace_element(handle);
                    }
                }
            }
            Builtin::IteratorNext(iter) | Builtin::IteratorJoin(iter) => {
                if self.visit_rc(iter) {
                    for value in &iter.borrow().values {
                        self.trace_value(value);
                    }
                }
            }
            Builtin::AbortControllerAbort(ctrl) => self.trace_abort_signal(&ctrl.borrow().signal),
            Builtin::AbortSignalAddEventListener(s)
            | Builtin::AbortSignalRemoveEventListener(s)
            | Builtin::AbortSignalThrowIfAborted(s)
            | Builtin::AbortSignalAbortWith(s) => self.trace_abort_signal(s),
            Builtin::AbortSignalFollow(pair) => {
                self.trace_abort_signal(&pair.0);
                self.trace_abort_signal(&pair.1);
            }
            Builtin::WorkerPostMessage(w)
            | Builtin::WorkerTerminate(w)
            | Builtin::WorkerAddEventListener(w)
            | Builtin::WorkerRemoveEventListener(w) => self.trace_worker(w),
            Builtin::ClipboardWriteStep(boxed) => {
                if self.visit_rc(&boxed.0) {
                    boxed.0.borrow().trace(self);
                }
            }
            Builtin::ClipboardWriteFailed(write) => {
                if self.visit_rc(write) {
                    write.borrow().trace(self);
                }
            }
            Builtin::IndexedDbRequestDispatch(req) => self.trace_idb_request(req),
            Builtin::IndexedDbTransactionComplete(tx)
            | Builtin::IndexedDbTransactionObjectStore(tx)
            | Builtin::IndexedDbTransactionAbort(tx) => self.trace_idb_tx(tx),
            Builtin::IndexedDbDatabaseCreateObjectStore(db)
            | Builtin::IndexedDbDatabaseDeleteObjectStore(db)
            | Builtin::IndexedDbDatabaseTransaction(db)
            | Builtin::IndexedDbDatabaseClose(db) => self.trace_idb_db(db),
            Builtin::IndexedDbObjectStorePut(s)
            | Builtin::IndexedDbObjectStoreAdd(s)
            | Builtin::IndexedDbObjectStoreGet(s)
            | Builtin::IndexedDbObjectStoreDelete(s)
            | Builtin::IndexedDbObjectStoreClear(s)
            | Builtin::IndexedDbObjectStoreCount(s)
            | Builtin::IndexedDbObjectStoreGetAll(s)
            | Builtin::IndexedDbObjectStoreGetAllKeys(s) => {
                if self.visit_rc(s)
                    && let Some(tx) = &s.borrow().transaction
                {
                    self.trace_idb_tx(tx);
                }
            }
            Builtin::RequestClone(r)
            | Builtin::RequestText(r)
            | Builtin::RequestBytes(r)
            | Builtin::RequestUint8(r)
            | Builtin::RequestBlob(r)
            | Builtin::RequestFormData(r) => {
                if let Some(signal) = &r.signal {
                    self.trace_abort_signal(signal);
                }
                if let Some(object) = &r.stream.borrow().object {
                    self.mark_object(object.clone());
                }
            }
            Builtin::ResponseClone(r)
            | Builtin::ResponseText(r)
            | Builtin::ResponseJson(r)
            | Builtin::ResponseBytes(r)
            | Builtin::ResponseUint8(r)
            | Builtin::ResponseBlob(r)
            | Builtin::ResponseFormData(r) => {
                if let Some(object) = &r.stream.borrow().object {
                    self.mark_object(object.clone());
                }
            }
        }
    }

    fn trace_native(&mut self, native: &super::realm::Native) {
        use super::realm::Native;
        match native {
            Native::AsyncResume(handle, _) => {
                if self.visit_rc(&handle.0) {
                    let state = handle.0.borrow();
                    if let Some(frame) = &state.frame {
                        self.trace_frame(frame);
                    }
                    self.trace_promise(&state.promise);
                    if let Some(generator) = &state.generator
                        && self.visit_rc(generator)
                    {
                        generator.borrow().trace(self);
                    }
                }
            }
            Native::Promise(func) => func.trace(self),
            Native::ProxyRevoke(proxy) => {
                if self.visit_rc(proxy) {
                    if let Some(target) = &*proxy.target.borrow() {
                        self.trace_value(target);
                    }
                    if let Some(handler) = &*proxy.handler.borrow() {
                        self.trace_value(handler);
                    }
                }
            }
            Native::ViewTransition(transition, _) => transition.trace(self),
            Native::Stream(func) => func.trace(self),
            Native::Nav(func) => func.trace(self),
            Native::PerfDeliver(observer) => observer.trace(self),
            _ => {}
        }
    }

    pub(super) fn trace_jsobject(&mut self, object: &JsObject) {
        for slot in object.slots() {
            self.trace_slot(slot);
        }
        if let Some(prototype) = object.own_prototype() {
            self.mark_object(prototype.clone());
        }
        match &object.class {
            ObjectClass::Ordinary
            | ObjectClass::Arguments
            | ObjectClass::RawJson
            | ObjectClass::Locale(_)
            | ObjectClass::DisplayNames(_)
            | ObjectClass::DurationFormat(_)
            | ObjectClass::UrlPattern(_)
            | ObjectClass::XPath(_)
            | ObjectClass::StyleOm(_)
            | ObjectClass::Temporal(_)
            | ObjectClass::Boolean(_)
            | ObjectClass::Number(_)
            | ObjectClass::String(_)
            | ObjectClass::Symbol(_)
            | ObjectClass::BigInt(_)
            | ObjectClass::Error
            | ObjectClass::RegExp(_)
            | ObjectClass::Date(_)
            | ObjectClass::WeakRef(_)
            | ObjectClass::EventTarget(_)
            | ObjectClass::Observer(_)
            | ObjectClass::DataView(_)
            | ObjectClass::TextDecoder(_)
            | ObjectClass::Intl(_)
            | ObjectClass::Url(_)
            | ObjectClass::UrlSearchParams(_)
            | ObjectClass::Walk(_)
            | ObjectClass::Promise
            | ObjectClass::Path(_)
            | ObjectClass::Matrix(_)
            | ObjectClass::Sheet(_)
            | ObjectClass::Segmenter(_)
            | ObjectClass::TypedArray(_) => {}
            ObjectClass::Function(closure) => self.trace_closure(closure),
            ObjectClass::Bound(bound) => {
                self.trace_value(&bound.target);
                self.trace_value(&bound.this);
                for arg in &bound.arguments {
                    self.trace_value(arg);
                }
            }
            ObjectClass::Offscreen(handle) => self.trace_element(handle),
            ObjectClass::WebGl(gl) => gl.trace(self),
            ObjectClass::PerfObserver(observer) => observer.trace(self),
            ObjectClass::PerfEntries(entries) => {
                for entry in entries.iter() {
                    self.trace_value(&entry.detail);
                }
            }
            ObjectClass::Generator(generator) => {
                if self.visit_rc(generator) {
                    generator.borrow().trace(self);
                }
            }
            ObjectClass::AsyncGenerator(generator) => {
                if self.visit_rc(generator) {
                    generator.borrow().trace(self);
                }
            }
            ObjectClass::Native(native) => self.trace_native(native),
            ObjectClass::Proxy(proxy) => {
                if self.visit_rc(proxy) {
                    if let Some(target) = &*proxy.target.borrow() {
                        self.trace_value(target);
                    }
                    if let Some(handler) = &*proxy.handler.borrow() {
                        self.trace_value(handler);
                    }
                }
            }
            ObjectClass::Map(collection) | ObjectClass::Set(collection) => {
                if self.visit_rc(collection) {
                    collection.borrow().trace_strong(self);
                }
            }
            ObjectClass::WeakMap(collection) | ObjectClass::WeakSet(collection) => {
                if self.visit_rc(collection) {
                    collection.borrow().trace_non_object_keys(self);
                    self.weak_maps.push(collection.clone());
                }
            }
            ObjectClass::ArrayIterator(iter) => {
                if self.visit_rc(iter) {
                    self.trace_value(&iter.borrow().target);
                }
            }
            ObjectClass::Cursor(cursor) => {
                if self.visit_rc(cursor) {
                    let col = cursor.borrow().collection.clone();
                    if self.visit_rc(&col) {
                        col.borrow().trace_strong(self);
                    }
                }
            }
            ObjectClass::Registry(registry) => {
                if self.visit_rc(registry) {
                    registry.borrow().trace(self);
                }
            }
            ObjectClass::FontFace(face) => face.trace(self),
            ObjectClass::CssRule(rule) => rule.trace(self),
            ObjectClass::DisposableStack(stack) => stack.trace(self),
            ObjectClass::Stream(stream) => stream.trace(self),
            ObjectClass::IteratorHelper(helper) => {
                if self.visit_rc(helper) {
                    helper.borrow().trace(self);
                }
            }
        }
    }

    fn drain(&mut self) {
        loop {
            let mut progressed = false;
            while let Some(entry) = self.objects.pop() {
                progressed = true;
                // safety: active_borrows == 0 during GC and chunks do not move during marking
                if let Some(object) = unsafe { (*(*entry).value.get()).as_ref() } {
                    self.trace_jsobject(object);
                }
            }
            while let Some(entry) = self.arrays.pop() {
                progressed = true;
                // safety: active_borrows == 0 during GC and chunks do not move during marking
                if let Some(array) = unsafe { (*(*entry).value.get()).as_ref() } {
                    for value in &array.values {
                        self.trace_value(value);
                    }
                    if let Some(extra) = &array.extra {
                        for (_, property) in &extra.properties {
                            self.trace_slot(&property.slot);
                        }
                        if let Some(Some(prototype)) = &extra.prototype {
                            self.mark_object(prototype.clone());
                        }
                    }
                }
            }
            while let Some(entry) = self.scopes.pop() {
                progressed = true;
                // safety: active_borrows == 0 during GC and chunks do not move during marking
                if let Some(scope) = unsafe { (*(*entry).value.get()).as_ref() } {
                    for value in &scope.slots {
                        self.trace_value(value);
                    }
                    if let Some(parent) = &scope.parent {
                        self.mark_scope(parent.clone());
                    }
                }
            }
            if !progressed {
                break;
            }
        }
    }

    fn ephemeron_fixpoint(&mut self) {
        if self.weak_maps.is_empty() {
            return;
        }
        loop {
            let before = self.objects.len() + self.arrays.len() + self.scopes.len();
            let maps = self.weak_maps.clone();
            for collection in maps {
                collection.borrow().trace_weak_values(self);
            }
            if self.objects.len() + self.arrays.len() + self.scopes.len() == before {
                break;
            }
            self.drain();
        }
    }
}

impl ScriptRuntime {
    pub(super) fn collect_if_due(&mut self) -> usize {
        if self.nested_runs > 1 || self.gc_inhibit != 0 || !gc_due(self.heap_id) {
            return 0;
        }
        if self.nested_runs == 1 && !self.gc_safepoint_allowed {
            return 0;
        }
        self.collect_garbage()
    }

    pub(super) fn collect_garbage(&mut self) -> usize {
        let Some(heap_ptr) = heap_for(self.heap_id) else {
            return 0;
        };
        let heap = unsafe { &*heap_ptr };
        if heap.active_borrows() != 0 {
            return 0;
        }
        let mut tracer = Tracer::new(heap_ptr, self.heap_id);
        // pinned intrinsics are roots and their outgoing edges (e.g. properties added to
        // Object.prototype or global constructors) must be traced too
        heap.objects.push_pinned(&mut tracer.objects);
        heap.arrays.push_pinned(&mut tracer.arrays);
        heap.scopes.push_pinned(&mut tracer.scopes);
        self.trace_roots(&mut tracer);
        tracer.drain();
        tracer.ephemeron_fixpoint();
        let weak_maps = std::mem::take(&mut tracer.weak_maps);
        drop(tracer);

        let cleared = heap.objects.sweep() + heap.arrays.sweep() + heap.scopes.sweep();
        let survivors = heap.live_count();
        heap.allocs_since_gc.set(0);
        heap.gc_threshold.set(survivors.max(MINIMUM_THRESHOLD));

        for collection in weak_maps {
            if let Ok(mut collection) = collection.try_borrow_mut() {
                collection.prune();
            }
        }
        cleared
    }
}

#[cfg(test)]
fn live_nodes() -> usize {
    HEAPS.with(|cell| {
        let heaps = unsafe { &*cell.get() };
        heaps.iter().flatten().map(|heap| heap.live_count()).sum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_handles_and_entries_stay_compact() {
        assert_eq!(std::mem::size_of::<ObjectHandle>(), 8);
        assert_eq!(std::mem::size_of::<ObjectWeak>(), 8);
        assert_eq!(std::mem::size_of::<ArrayHandle>(), 8);
        assert_eq!(std::mem::size_of::<ScopeRef>(), 8);
        assert!(!std::mem::needs_drop::<ObjectHandle>());
        assert!(!std::mem::needs_drop::<ObjectWeak>());
        assert!(!std::mem::needs_drop::<ArrayHandle>());
        assert!(!std::mem::needs_drop::<ScopeRef>());
        assert_eq!(std::mem::size_of::<Entry<JsObject>>(), 64);
        assert_eq!(std::mem::size_of::<Entry<ArrayData>>(), 40);
        assert_eq!(std::mem::size_of::<vm::Closure>(), 24);
        assert_eq!(std::mem::size_of::<Value>(), 16);
        assert_eq!(std::mem::size_of::<compiler::Op>(), 16);
    }

    #[test]
    fn a_released_runtime_leaves_none_of_its_nodes_whatever_holds_them() {
        let document = |markup: &str| {
            Rc::new(RefCell::new(
                crate::dom::Document::parse(markup).expect("document should parse"),
            ))
        };
        let kept = ScriptRuntime::new(document("<p></p>"));
        let baseline = live_nodes();
        let mut page = ScriptRuntime::new(document("<p id='out'></p>"));
        page.execute(
            r#"
            const map = new Map();
            for (let i = 0; i < 500; i++) { const box = { i }; box.self = () => map; map.set(box, [box]); }
            document.getElementById("out").addEventListener("click", () => map);
            "#,
        )
        .expect("script should run");
        assert!(live_nodes() > baseline + 1000);
        page.release_heap();
        assert_eq!(live_nodes(), baseline, "a released runtime's nodes all go");
        drop(kept);
    }

    #[test]
    fn closures_that_hold_their_own_scope_are_collected() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime.collect_garbage();
        let baseline = live_nodes();
        runtime
            .execute(
                r#"
                function make(n) { const data = [n]; const self = () => self && data; return data.length; }
                const kept = (() => { const box = { n: 1 }; const self = () => box; box.self = self; return self; })();
                let total = 0;
                for (let i = 0; i < 2000; i++) total += make(i);
                globalThis.check = () => kept().n + total;
                "#,
            )
            .expect("script should run");
        runtime.collect_garbage();
        // the 2000 calls each left a scope, an array and a closure in a cycle; what is left
        // beyond the runtime itself is the script's own top level
        let grown = live_nodes().saturating_sub(baseline);
        assert!(
            grown < 200,
            "{grown} nodes outlived the calls that made them"
        );
        runtime
            .execute(r#"document.getElementById("out").textContent = String(check());"#)
            .expect("what is still reachable keeps working");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("2001")
        );
    }
}
