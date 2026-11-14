use crate::ffi::platform;
use crate::ffi::{FfiError, FfiResult, FfiState, TypeId};
use crate::{
    CPointer, CPointerFlags, CPointerInit, ForeignLibrary, ForeignLibraryInit, Handle, HandleScope,
    Heap, TypeInfo, VM,
};

impl FfiState {
    /// Load a shared library, or the current process when `path` is `None`.
    pub fn open_library<'s>(
        &self,
        vm: &VM,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        path: Option<&str>,
    ) -> FfiResult<Handle<'s, ForeignLibrary>> {
        let address = platform::open(path)? as *mut u8;
        let void = heap
            .known()
            .ffi_types
            .get(heap, TypeId::VOID)
            .expect("C FFI builtins are installed during VM setup");
        let void = scope.handle(void);
        let flags = CPointerFlags::NONE;
        let handle = heap.allocate_handle::<CPointer>(
            CPointerInit {
                pointee: void,
                address,
                flags,
                library: None,
            },
            scope,
        );
        let path = vm.interner().intern_str(heap, scope, path.unwrap_or(""));
        Ok(heap.allocate_handle::<ForeignLibrary>(ForeignLibraryInit { handle, path }, scope))
    }

    /// Unload a library. Idempotent.
    pub fn close_library(&self, heap: &Heap, library: &Handle<ForeignLibrary>) -> FfiResult<()> {
        let library = library.as_tagged(heap);
        let handle = library.handle(heap);
        if handle.is_dead() {
            return Ok(());
        }
        platform::close(handle.address() as usize)?;
        handle.kill(heap);
        Ok(())
    }

    /// Resolve `name` in `library` as a value of the pointer type `ty`.
    ///
    /// `ty` must be a pointer type: `dlsym` returns an untyped address and the
    /// caller supplies the interpretation.
    pub fn library_symbol<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        library: &Handle<ForeignLibrary>,
        name: &str,
        ty: &Handle<TypeInfo>,
    ) -> FfiResult<Handle<'s, CPointer>> {
        let lookup = {
            let library = library.as_tagged(heap);
            if !library.is_live(heap) {
                return Err(FfiError::LibraryClosed);
            }
            library.handle(heap).address() as usize
        };
        let address = platform::symbol(lookup, name)? as *mut u8;
        let pointer = heap.allocate_handle::<CPointer>(
            CPointerInit {
                pointee: ty.clone(),
                address,
                flags: CPointerFlags::NONE,
                library: None,
            },
            scope,
        );
        pointer
            .as_tagged(heap)
            .set_library(heap, library.as_tagged(heap));
        Ok(pointer)
    }

    /// The path a library was opened from; `""` for the current process.
    pub fn library_path(&self, heap: &Heap, library: &Handle<ForeignLibrary>) -> Option<String> {
        Some(library.as_tagged(heap).path(heap).to_rust_string(heap))
    }
}
