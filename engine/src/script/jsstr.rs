// a script string behind one thin pointer: the count, the length and the bytes share one
// allocation, so a value holding it stays two words wide where an Rc<str> would make it three

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::cell::{Cell, UnsafeCell};
use std::fmt;
use std::ptr::NonNull;

#[repr(C)]
struct Header {
    count: Cell<usize>,
    len: usize,
}

pub(crate) struct JsStr(NonNull<Header>);

const DATA_OFFSET: usize = std::mem::size_of::<Header>();
const HEADER_ALIGN: usize = std::mem::align_of::<Header>();
const SMALL32_CAP: usize = 32;
const SMALL64_CAP: usize = 64;
const POOL_LIMIT: usize = 128;

#[inline(always)]
fn layout_for(len: usize) -> (Layout, usize) {
    let bucket = if len <= SMALL32_CAP {
        SMALL32_CAP
    } else if len <= SMALL64_CAP {
        SMALL64_CAP
    } else {
        len
    };
    let size = DATA_OFFSET
        .checked_add(bucket)
        .expect("a string fits in memory");
    let layout = Layout::from_size_align(size, HEADER_ALIGN).expect("a string fits in memory");
    (layout, DATA_OFFSET)
}

struct StrPool {
    s32: Vec<*mut u8>,
    s64: Vec<*mut u8>,
}

impl StrPool {
    const fn new() -> Self {
        Self {
            s32: Vec::new(),
            s64: Vec::new(),
        }
    }
}

impl Drop for StrPool {
    fn drop(&mut self) {
        let l32 = layout_for(SMALL32_CAP).0;
        let l64 = layout_for(SMALL64_CAP).0;
        // safety: every cached pointer was allocated with the bucket's layout
        unsafe {
            for ptr in self.s32.drain(..) {
                dealloc(ptr, l32);
            }
            for ptr in self.s64.drain(..) {
                dealloc(ptr, l64);
            }
        }
    }
}

thread_local! {
    static STR_POOL: UnsafeCell<StrPool> = const { UnsafeCell::new(StrPool::new()) };
    static EMPTY: JsStr = JsStr::alloc_bytes("");
}

impl JsStr {
    #[inline]
    pub(crate) fn new(text: &str) -> Self {
        if text.is_empty() {
            return EMPTY.with(JsStr::clone);
        }
        Self::alloc_bytes(text)
    }

    fn alloc_raw(len: usize) -> *mut u8 {
        let pooled = if len <= SMALL32_CAP {
            STR_POOL
                .try_with(|cell| unsafe { (&mut *cell.get()).s32.pop() })
                .ok()
                .flatten()
        } else if len <= SMALL64_CAP {
            STR_POOL
                .try_with(|cell| unsafe { (&mut *cell.get()).s64.pop() })
                .ok()
                .flatten()
        } else {
            None
        };
        match pooled {
            Some(ptr) => ptr,
            None => unsafe {
                let (layout, _) = layout_for(len);
                let raw = alloc(layout);
                if raw.is_null() {
                    handle_alloc_error(layout);
                }
                raw
            },
        }
    }

    fn alloc_bytes(text: &str) -> Self {
        let len = text.len();
        // safety: the layout is never zero-sized, as it holds the header; the header and the
        // bytes are written before anything reads them
        unsafe {
            let raw = Self::alloc_raw(len);
            let header = NonNull::new_unchecked(raw.cast::<Header>());
            header.as_ptr().write(Header {
                count: Cell::new(1),
                len,
            });
            std::ptr::copy_nonoverlapping(text.as_ptr(), raw.add(DATA_OFFSET), len);
            Self(header)
        }
    }

    #[inline]
    pub(crate) fn concat(a: &str, b: &str) -> Self {
        let len = a.len() + b.len();
        if len == 0 {
            return EMPTY.with(JsStr::clone);
        }
        // safety: raw has DATA_OFFSET + len bytes and both slices are valid utf-8
        unsafe {
            let raw = Self::alloc_raw(len);
            let header = NonNull::new_unchecked(raw.cast::<Header>());
            header.as_ptr().write(Header {
                count: Cell::new(1),
                len,
            });
            std::ptr::copy_nonoverlapping(a.as_ptr(), raw.add(DATA_OFFSET), a.len());
            std::ptr::copy_nonoverlapping(b.as_ptr(), raw.add(DATA_OFFSET + a.len()), b.len());
            Self(header)
        }
    }

    #[inline(always)]
    fn header(&self) -> &Header {
        // SAFETY: the header lives while any handle to it does
        unsafe { self.0.as_ref() }
    }

    #[inline(always)]
    pub(crate) fn as_str(&self) -> &str {
        let len = self.header().len;
        // SAFETY: Header is repr(C) with align >= 1 so u8 bytes immediately follow it
        unsafe {
            let bytes = self.0.as_ptr().cast::<u8>().add(DATA_OFFSET);
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(bytes, len))
        }
    }

    #[inline(always)]
    pub(crate) fn ptr_eq(left: &Self, right: &Self) -> bool {
        left.0 == right.0
    }

    #[inline(always)]
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(crate) fn as_raw_ptr(&self) -> *mut u8 {
        self.0.as_ptr().cast()
    }

    #[cold]
    #[inline(never)]
    fn dealloc_slow(&mut self) {
        let len = self.header().len;
        let ptr = self.0.as_ptr().cast::<u8>();
        if len <= SMALL32_CAP {
            let recycled = STR_POOL
                .try_with(|cell| {
                    let pool = unsafe { &mut *cell.get() };
                    if pool.s32.len() < POOL_LIMIT {
                        pool.s32.push(ptr);
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if recycled {
                return;
            }
        } else if len <= SMALL64_CAP {
            let recycled = STR_POOL
                .try_with(|cell| {
                    let pool = unsafe { &mut *cell.get() };
                    if pool.s64.len() < POOL_LIMIT {
                        pool.s64.push(ptr);
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if recycled {
                return;
            }
        }
        let (layout, _) = layout_for(len);
        // safety: this was the last handle, and the layout matches allocation
        unsafe { dealloc(ptr, layout) }
    }
}

impl Clone for JsStr {
    #[inline(always)]
    fn clone(&self) -> Self {
        let count = &self.header().count;
        count.set(count.get() + 1);
        Self(self.0)
    }
}

impl Drop for JsStr {
    #[inline]
    fn drop(&mut self) {
        let count = &self.header().count;
        let next = count.get() - 1;
        count.set(next);
        if next == 0 {
            self.dealloc_slow();
        }
    }
}

impl std::ops::Deref for JsStr {
    type Target = str;

    #[inline(always)]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for JsStr {
    fn as_ref(&self) -> &str {
        self
    }
}

impl std::borrow::Borrow<str> for JsStr {
    fn borrow(&self) -> &str {
        self
    }
}

impl PartialEq for JsStr {
    fn eq(&self, other: &Self) -> bool {
        Self::ptr_eq(self, other) || **self == **other
    }
}

impl Eq for JsStr {}

impl PartialEq<str> for JsStr {
    fn eq(&self, other: &str) -> bool {
        &**self == other
    }
}

impl PartialEq<&str> for JsStr {
    fn eq(&self, other: &&str) -> bool {
        &**self == *other
    }
}

impl PartialOrd for JsStr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JsStr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl std::hash::Hash for JsStr {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
}

impl fmt::Debug for JsStr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, formatter)
    }
}

impl fmt::Display for JsStr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, formatter)
    }
}

impl Default for JsStr {
    fn default() -> Self {
        Self::new("")
    }
}

impl From<&str> for JsStr {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for JsStr {
    fn from(text: String) -> Self {
        Self::new(&text)
    }
}

impl From<&String> for JsStr {
    fn from(text: &String) -> Self {
        Self::new(text)
    }
}

impl From<&JsStr> for JsStr {
    fn from(text: &JsStr) -> Self {
        text.clone()
    }
}

impl From<char> for JsStr {
    fn from(character: char) -> Self {
        Self::new(character.encode_utf8(&mut [0; 4]))
    }
}

impl From<JsStr> for String {
    fn from(text: JsStr) -> Self {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::JsStr;

    #[test]
    fn a_thin_string_reads_back_what_it_was_made_from_and_frees_with_its_last_handle() {
        assert_eq!(std::mem::size_of::<JsStr>(), 8);
        let made = JsStr::from("héllo, wörld");
        let shared = made.clone();
        drop(made);
        assert_eq!(&*shared, "héllo, wörld");
        assert_eq!(shared.len(), "héllo, wörld".len());
        assert_eq!(JsStr::from(""), JsStr::default());
        let (a, b) = (JsStr::from("a"), JsStr::from("b"));
        assert!(a < b);
        assert_eq!(JsStr::from(String::from("x")), "x");
    }
}
