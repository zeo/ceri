// a script string behind one thin pointer: the count, the length and the bytes share one
// allocation, so a value holding it stays two words wide where an Rc<str> would make it three

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::cell::Cell;
use std::fmt;
use std::ptr::NonNull;

#[repr(C)]
struct Header {
    count: Cell<usize>,
    len: usize,
}

pub(crate) struct JsStr(NonNull<Header>);

fn layout_for(len: usize) -> (Layout, usize) {
    Layout::new::<Header>()
        .extend(Layout::array::<u8>(len).expect("a string fits in memory"))
        .expect("a string fits in memory")
}

thread_local! {
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

    fn alloc_bytes(text: &str) -> Self {
        let (layout, offset) = layout_for(text.len());
        // SAFETY: the layout is never zero-sized, as it holds the header; the header and the
        // bytes are written before anything reads them
        unsafe {
            let raw = alloc(layout);
            let Some(header) = NonNull::new(raw.cast::<Header>()) else {
                handle_alloc_error(layout);
            };
            header.as_ptr().write(Header {
                count: Cell::new(1),
                len: text.len(),
            });
            std::ptr::copy_nonoverlapping(text.as_ptr(), raw.add(offset), text.len());
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
        const DATA_OFFSET: usize = std::mem::size_of::<Header>();
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

    #[cold]
    #[inline(never)]
    fn dealloc_slow(&mut self) {
        let (layout, _) = layout_for(self.header().len);
        // SAFETY: this was the last handle, and the layout is the one it was made with
        unsafe { dealloc(self.0.as_ptr().cast(), layout) }
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
