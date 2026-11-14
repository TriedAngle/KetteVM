//! Loading libraries and resolving their symbols.

use crate::ffi::platform;
use crate::ffi::{FfiError, FfiResult, FfiState, TypeKind};
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
        let address = platform::open(path)?;
        let void = scope.handle(self.builtin(heap, TypeKind::Void));
        let mut flags = CPointerFlags::LIBRARY.union(CPointerFlags::BORROWED);
        if path.is_none() {
            flags = flags.union(CPointerFlags::PROCESS);
        }
        let handle = heap.allocate_handle::<CPointer>(
            CPointerInit {
                pointee: void,
                address,
                flags,
                library: None,
                owner: None,
            },
            scope,
        );
        let path = vm.interner().intern_str(heap, scope, path.unwrap_or(""));
        Ok(heap.allocate_handle::<ForeignLibrary>(ForeignLibraryInit { handle, path }, scope))
    }

    /// Unload a library. Idempotent; process handles are marked dead but never
    /// unloaded.
    pub fn close_library(&self, heap: &Heap, library: &Handle<ForeignLibrary>) -> FfiResult<()> {
        let library = library.as_tagged(heap);
        let handle = library.handle(heap);
        if handle.is_dead() {
            return Ok(());
        }
        if !handle.flags().contains(CPointerFlags::PROCESS) {
            platform::close(handle.address(heap))?;
        }
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
            library.handle(heap).address(heap)
        };
        let address = platform::symbol(lookup, name)?;
        let pointer = heap.allocate_handle::<CPointer>(
            CPointerInit {
                pointee: ty.clone(),
                address,
                flags: CPointerFlags::LIBRARY.union(CPointerFlags::BORROWED),
                library: None,
                owner: None,
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
