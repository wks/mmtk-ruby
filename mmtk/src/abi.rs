use crate::api::RubyMutator;
use crate::{upcalls, Ruby};
use mmtk::scheduler::{GCController, GCWorker};
use mmtk::util::{ObjectReference, VMMutatorThread, VMWorkerThread};

// For the C binding
pub const OBJREF_OFFSET: usize = 8;
pub const MIN_OBJ_ALIGN: usize = 8; // Even on 32-bit machine.  A Ruby object is at least 40 bytes large.

pub const GC_THREAD_KIND_CONTROLLER: libc::c_int = 0;
pub const GC_THREAD_KIND_WORKER: libc::c_int = 1;

type ObjectClosureFunction =
    extern "C" fn(*mut libc::c_void, *mut libc::c_void, ObjectReference) -> ObjectReference;
#[repr(C)]
pub struct ObjectClosure {
    /// The function to be called from C.
    pub c_function: ObjectClosureFunction,
    /// The pointer to the Rust-level closure object.
    pub rust_closure: *mut libc::c_void,
}

impl Default for ObjectClosure {
    fn default() -> Self {
        Self {
            c_function: THE_UNREGISTERED_CLOSURE_FUNC,
            rust_closure: std::ptr::null_mut(),
        }
    }
}

/// Rust doesn't require function items to have a unique address.
/// We therefore force using this particular constant.
///
/// See: https://rust-lang.github.io/rust-clippy/master/index.html#fn_address_comparisons
const THE_UNREGISTERED_CLOSURE_FUNC: ObjectClosureFunction = ObjectClosure::c_function_unregistered;

impl ObjectClosure {
    /// Set this ObjectClosure temporarily to `visit_object`, and execute `f`.  During the execution of
    /// `f`, the Ruby VM may call this ObjectClosure.  When the Ruby VM calls this ObjectClosure,
    /// it effectively calls `visit_object`.
    ///
    /// This method is intended to run Ruby VM code in `f` with temporarily modified behavior of
    /// `rb_gc_mark`, `rb_gc_mark_movable` and `rb_gc_location`
    ///
    /// Both `f` and `visit_object` may access and modify local variables in the environment where
    /// `set_temporarily_and_run_code` called.
    ///
    /// Note that this function is not reentrant.  Don't call this function in either `callback` or
    /// `f`.
    pub fn set_temporarily_and_run_code<'env, T, F1, F2>(
        &mut self,
        mut visit_object: F1,
        f: F2,
    ) -> T
    where
        F1: 'env + FnMut(&'static mut GCWorker<Ruby>, ObjectReference) -> ObjectReference,
        F2: 'env + FnOnce() -> T,
    {
        debug_assert!(
            self.c_function == THE_UNREGISTERED_CLOSURE_FUNC,
            "set_temporarily_and_run_code is recursively called."
        );
        self.c_function = Self::c_function_registered::<F1>;
        self.rust_closure = &mut visit_object as *mut F1 as *mut libc::c_void;
        let result = f();
        *self = Default::default();
        result
    }

    extern "C" fn c_function_registered<F>(
        rust_closure: *mut libc::c_void,
        worker: *mut libc::c_void,
        object: ObjectReference,
    ) -> ObjectReference
    where
        F: FnMut(&'static mut GCWorker<Ruby>, ObjectReference) -> ObjectReference,
    {
        let rust_closure = unsafe { &mut *(rust_closure as *mut F) };
        let worker = unsafe { &mut *(worker as *mut GCWorker<Ruby>) };
        rust_closure(worker, object)
    }

    extern "C" fn c_function_unregistered(
        _rust_closure: *mut libc::c_void,
        worker: *mut libc::c_void,
        object: ObjectReference,
    ) -> ObjectReference {
        let worker = unsafe { &mut *(worker as *mut GCWorker<Ruby>) };
        panic!(
            "object_closure is not set.  worker ordinal: {}, object: {}",
            worker.ordinal, object
        );
    }
}

#[repr(C)]
pub struct GCThreadTLS {
    pub kind: libc::c_int,
    pub gc_context: *mut libc::c_void,
    pub object_closure: ObjectClosure,
}

impl GCThreadTLS {
    fn new(kind: libc::c_int, gc_context: *mut libc::c_void) -> Self {
        Self {
            kind,
            gc_context,
            object_closure: Default::default(),
        }
    }

    pub fn for_controller(gc_context: *mut GCController<Ruby>) -> Self {
        Self::new(GC_THREAD_KIND_CONTROLLER, gc_context as *mut libc::c_void)
    }

    pub fn for_worker(gc_context: *mut GCWorker<Ruby>) -> Self {
        Self::new(GC_THREAD_KIND_WORKER, gc_context as *mut libc::c_void)
    }

    pub fn from_vwt(vwt: VMWorkerThread) -> *mut GCThreadTLS {
        unsafe { std::mem::transmute(vwt) }
    }

    /// Cast a pointer to `GCThreadTLS` to a ref, with assertion for null pointer.
    ///
    /// # Safety
    ///
    /// Has undefined behavior if `ptr` is invalid.
    pub unsafe fn check_cast(ptr: *mut GCThreadTLS) -> &'static mut GCThreadTLS {
        assert!(!ptr.is_null());
        let result = &mut *ptr;
        debug_assert!({
            let kind = result.kind;
            kind == GC_THREAD_KIND_CONTROLLER || kind == GC_THREAD_KIND_WORKER
        });
        result
    }

    /// Cast a pointer to `VMWorkerThread` to a ref, with assertion for null pointer.
    ///
    /// # Safety
    ///
    /// Has undefined behavior if `ptr` is invalid.
    pub unsafe fn from_vwt_check(vwt: VMWorkerThread) -> &'static mut GCThreadTLS {
        let ptr = Self::from_vwt(vwt);
        Self::check_cast(ptr)
    }

    #[allow(clippy::not_unsafe_ptr_arg_deref)] // `transmute` does not dereference pointer
    pub fn to_vwt(ptr: *mut Self) -> VMWorkerThread {
        unsafe { std::mem::transmute(ptr) }
    }

    /// Get a ref to `GCThreadTLS` from C-level thread-local storage, with assertion for null
    /// pointer.
    ///
    /// # Safety
    ///
    /// Has undefined behavior if the pointer held in C-level TLS is invalid.
    pub unsafe fn from_upcall_check() -> &'static mut GCThreadTLS {
        let ptr = (upcalls().get_gc_thread_tls)();
        Self::check_cast(ptr)
    }

    pub fn worker<'s, 'w>(&'s mut self) -> &'w mut GCWorker<Ruby> {
        // NOTE: The returned ref points to the worker which does not have the same lifetime as self.
        assert!(self.kind == GC_THREAD_KIND_WORKER);
        unsafe { &mut *(self.gc_context as *mut GCWorker<Ruby>) }
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct RawVecOfObjRef {
    pub ptr: *mut ObjectReference,
    pub len: usize,
    pub capa: usize,
}

impl RawVecOfObjRef {
    pub fn from_vec(vec: Vec<ObjectReference>) -> RawVecOfObjRef {
        // Note: Vec::into_raw_parts is unstable. We implement it manually.
        let mut vec = std::mem::ManuallyDrop::new(vec);
        let (ptr, len, capa) = (vec.as_mut_ptr(), vec.len(), vec.capacity());

        RawVecOfObjRef { ptr, len, capa }
    }

    /// # Safety
    ///
    /// This function turns raw pointer into a Vec without check.
    pub unsafe fn into_vec(self) -> Vec<ObjectReference> {
        Vec::from_raw_parts(self.ptr, self.len, self.capa)
    }
}

impl From<Vec<ObjectReference>> for RawVecOfObjRef {
    fn from(v: Vec<ObjectReference>) -> Self {
        Self::from_vec(v)
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct RubyBindingOptions {
    pub ractor_check_mode: bool,
    pub suffix_size: usize,
}

#[repr(C)]
#[derive(Clone)]
pub struct RubyUpcalls {
    pub init_gc_worker_thread: extern "C" fn(gc_worker_tls: *mut GCThreadTLS),
    pub get_gc_thread_tls: extern "C" fn() -> *mut GCThreadTLS,
    pub stop_the_world: extern "C" fn(tls: VMWorkerThread),
    pub resume_mutators: extern "C" fn(tls: VMWorkerThread),
    pub block_for_gc: extern "C" fn(tls: VMMutatorThread),
    pub number_of_mutators: extern "C" fn() -> usize,
    pub reset_mutator_iterator: extern "C" fn(),
    pub get_next_mutator: extern "C" fn() -> *mut RubyMutator,
    pub scan_vm_specific_roots: extern "C" fn(),
    pub scan_thread_roots: extern "C" fn(),
    pub scan_thread_root: extern "C" fn(mutator_tls: VMMutatorThread, worker_tls: VMWorkerThread),
    pub scan_object_ruby_style: extern "C" fn(object: ObjectReference),
    pub object_type_str: extern "C" fn(object: ObjectReference) -> *const libc::c_char,
    pub detail_type_str: extern "C" fn(object: ObjectReference) -> *const libc::c_char,
}

unsafe impl Sync for RubyUpcalls {}
