use core::ptr::NonNull;

use crate::heap::RawCell;
use crate::value::Word;

/// A pointer into collector-managed memory.
#[derive(PartialEq, Eq, Hash)]
pub struct HeapPtr<T: ?Sized>(NonNull<T>);

impl<T: ?Sized> Clone for HeapPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for HeapPtr<T> {}

impl<T: ?Sized> core::fmt::Debug for HeapPtr<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "HeapPtr({:#x})", self.addr())
    }
}

unsafe impl<T: ?Sized> Send for HeapPtr<T> {}
unsafe impl<T: ?Sized> Sync for HeapPtr<T> {}

impl<T: ?Sized> HeapPtr<T> {
    pub fn as_non_null(self) -> NonNull<T> {
        self.0
    }

    pub fn as_ptr(self) -> *mut T {
        self.0.as_ptr()
    }

    pub fn addr(self) -> usize {
        self.0.as_ptr().cast::<()>() as usize
    }
}

impl<T> HeapPtr<T> {
    /// A pointer to an existing object: the reference keeps it valid.
    pub fn from_ref(r: &T) -> Self {
        Self(NonNull::from(r))
    }

    /// # Safety
    /// `ptr` points into collector memory and is aligned for `T`.
    pub unsafe fn from_non_null(ptr: NonNull<T>) -> Self {
        Self(ptr)
    }

    /// # Safety
    /// `addr` points into collector memory and is aligned for `T`.
    pub unsafe fn from_addr(addr: usize) -> Self {
        debug_assert!(addr != 0, "null collector pointer");
        Self(unsafe { NonNull::new_unchecked(addr as *mut T) })
    }

    /// Reinterpret the pointee as `U`.
    pub fn cast<U>(self) -> HeapPtr<U> {
        HeapPtr(self.0.cast())
    }

    /// # Safety
    /// The pointer must still name a live `T`: no collection may have run
    /// since its crreation, and the pointee must be fully initialized.
    #[allow(clippy::should_implement_trait)]
    pub unsafe fn as_ref(&self) -> &T {
        unsafe { self.0.as_ref() }
    }

    /// # Safety
    /// The caller upholds the write protocol of whatever `T` is (an
    /// allocation lock, a stop-the-world phase, ...).
    pub unsafe fn as_mut(&mut self) -> &mut T {
        unsafe { self.0.as_mut() }
    }

    /// # Safety
    /// As [`HeapPtr::as_mut`].
    pub unsafe fn write(self, value: T) {
        unsafe { self.0.as_ptr().write(value) };
    }

    /// # Safety
    /// As [`HeapPtr::as_mut`].
    pub unsafe fn read(self) -> T {
        unsafe { self.0.as_ptr().read() }
    }
}

impl HeapPtr<u8> {
    /// # Safety
    /// `bytes` keeps the result inside the same allocation.
    #[inline(always)]
    pub unsafe fn offset(self, bytes: usize) -> Self {
        Self(unsafe { NonNull::new_unchecked(self.0.as_ptr().add(bytes)) })
    }
}

/// A traced slot: a root cell or one word inside an object.
pub type CellRef = HeapPtr<RawCell>;

/// The start of a heap object, as chased through [`Word`]s.
pub type ObjectAddr = HeapPtr<()>;

impl ObjectAddr {
    /// The word parked at the object's start: the forwarding pointer while a
    /// minor cycle evacuates, the object header otherwise.
    #[inline(always)]
    pub fn header_word(self) -> Word {
        unsafe { self.cast::<Word>().read() }
    }

    /// # Safety
    /// Called on a stop-the-world phase, before anything else may touch the
    /// object's header.
    #[inline(always)]
    pub unsafe fn set_header_word(self, word: Word) {
        unsafe { self.cast::<Word>().write(word) };
    }
}
