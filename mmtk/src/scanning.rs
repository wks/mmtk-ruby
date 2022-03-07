use mmtk::vm::{Scanning, GCThreadContext};
use mmtk::{TransitiveClosure, Mutator, MutatorContext};
use mmtk::util::{Address, ObjectReference, VMWorkerThread};
use mmtk::scheduler::{ProcessEdgesWork, GCWork};
use mmtk::scheduler::GCWorker;
use std::os::raw::{c_void, c_ulong};
use crate::abi::GCThreadTLS;
use crate::{Ruby, upcalls};

/* automatically generated by rust-bindgen 0.57.0 */
#[allow(non_camel_case_types)]
pub type size_t = c_ulong;

// Passed to C to perform the transitive closure
pub unsafe extern "C" fn call_process_edge<T: TransitiveClosure>(closure: &mut T, adjacent: *mut *mut c_void) {
    closure.process_edge(Address::from_ptr(adjacent));
}

pub struct VMScanning {}

impl Scanning<Ruby> for VMScanning {
    const SINGLE_THREAD_MUTATOR_SCANNING: bool = false;

    fn scan_objects<W: ProcessEdgesWork<VM=Ruby>>(_objects: &[ObjectReference], _worker: &mut GCWorker<Ruby>) {
        unimplemented!()
    }

    fn scan_thread_roots<W: ProcessEdgesWork<VM=Ruby>>() {
        (upcalls().scan_thread_roots)()
    }

    fn scan_thread_root<W: ProcessEdgesWork<VM=Ruby>>(mutator: &'static mut Mutator<Ruby>, tls: VMWorkerThread) {
        let gc_tls = GCThreadTLS::from_vwt_check(tls);
        gc_tls.set_buffer_callback(Box::new(|_, addr_vec| {
            debug!("[scan_thread_root] Buffer delivered. Addresses:");
            for addr in addr_vec.iter() {
                debug!("[scan_thread_root]  {}", addr);
            }

            // TODO: scan the objects in the buffer.
        }));
        (upcalls().scan_thread_root)(mutator.get_tls(), tls);
        gc_tls.flush_buffer();
    }

    fn scan_vm_specific_roots<W: ProcessEdgesWork<VM=Ruby>>() {
        let gc_tls = GCThreadTLS::from_upcall_check();
        gc_tls.set_buffer_callback(Box::new(|_, addr_vec| {
            debug!("[scan_vm_specific_roots] Buffer delivered. Addresses:");
            for addr in addr_vec.iter() {
                debug!("[scan_vm_specific_roots]  {}", addr);
            }

            // TODO: scan the objects in the buffer.
        }));
        (upcalls().scan_vm_specific_roots)();
        gc_tls.flush_buffer();
    }

    fn scan_object<T: TransitiveClosure>(_trace: &mut T, _object: ObjectReference, _tls: VMWorkerThread) {
        unimplemented!()
    }

    fn notify_initial_thread_scan_complete(_partial_scan: bool, _tls: VMWorkerThread) {
        // Do nothing
    }

    fn supports_return_barrier() -> bool {
        false
    }

    fn prepare_for_roots_re_scanning() {
        todo!()
    }
}
