use crate::abi::GCThreadTLS;

use crate::{upcalls, Ruby};
use mmtk::util::{ObjectReference, VMWorkerThread};
use mmtk::vm::{EdgeVisitor, ObjectTracer, RootsWorkFactory, Scanning};
use mmtk::{Mutator, MutatorContext};

pub struct VMScanning {}

impl Scanning<Ruby> for VMScanning {
    const SINGLE_THREAD_MUTATOR_SCANNING: bool = false;

    fn support_edge_enqueuing(_tls: VMWorkerThread, _object: ObjectReference) -> bool {
        false
    }

    fn scan_object<EV: EdgeVisitor>(
        _tls: VMWorkerThread,
        _object: ObjectReference,
        _edge_visitor: &mut EV,
    ) {
        unreachable!("We have not enabled edge enqueuing for any types, yet.");
    }

    fn scan_object_and_trace_edges<OT: ObjectTracer>(
        tls: VMWorkerThread,
        object: ObjectReference,
        object_tracer: &mut OT,
    ) {
        let gc_tls = GCThreadTLS::from_vwt_check(tls);
        let visit_object = |_worker, target_object: ObjectReference| {
//            println!("Closure 1 visitor begin");
//            println!("Tracing object: {} -> {}", object, target_object);
//            assert!(mmtk::memory_manager::is_mmtk_object(
//                target_object.to_address()
//            ));
            if !(mmtk::memory_manager::is_mmtk_object(
                target_object.to_address()
            )) {
                println!("Assertion failed! {} is not MMTk object!", target_object.to_address());
                std::process::exit(1);
            }
            if (upcalls().is_parser)(target_object) {
                println!("Tracing parser object: {}", object);
            }
            let result = object_tracer.trace_object(target_object);
            assert_eq!(result, target_object);
//            println!("Closure 1 visitor end");
            result
        };
        gc_tls
            .object_closure
            .set_temporarily_and_run_code(visit_object, || {
//                println!("Closure 1 begin");
                (upcalls().scan_object_ruby_style)(object);
//                println!("Closure 1 end");
            });
    }

    fn notify_initial_thread_scan_complete(_partial_scan: bool, _tls: VMWorkerThread) {
        // Do nothing
    }

    fn scan_thread_roots(_tls: VMWorkerThread, _factory: impl RootsWorkFactory) {
        unreachable!();
    }

    fn scan_thread_root(
        tls: VMWorkerThread,
        mutator: &'static mut Mutator<Ruby>,
        mut factory: impl RootsWorkFactory,
    ) {
        let gc_tls = GCThreadTLS::from_vwt_check(tls);
        Self::collect_object_roots_in("scan_thread_root", gc_tls, &mut factory, || {
            (upcalls().scan_thread_root)(mutator.get_tls(), tls);
        });
    }

    fn scan_vm_specific_roots(tls: VMWorkerThread, mut factory: impl RootsWorkFactory) {
        let gc_tls = GCThreadTLS::from_vwt_check(tls);
        Self::collect_object_roots_in("scan_vm_specific_roots", gc_tls, &mut factory, || {
            (upcalls().scan_vm_specific_roots)();
        });
    }

    fn supports_return_barrier() -> bool {
        false
    }

    fn prepare_for_roots_re_scanning() {
        todo!()
    }
}

impl VMScanning {
    const OBJECT_BUFFER_SIZE: usize = 4096;

    fn collect_object_roots_in<F: FnMut()>(
        root_scan_kind: &str,
        gc_tls: &mut GCThreadTLS,
        factory: &mut impl RootsWorkFactory,
        callback: F,
    ) {
        let mut buffer: Vec<ObjectReference> = Vec::new();
        let visit_object = |_, object: ObjectReference| {
//            println!("Closure 2");
            debug!("[{}] Scanning object: {}", root_scan_kind, object);
            if (upcalls().is_parser)(object) {
                println!("Found parser root: {} during {}", object, root_scan_kind);
            }
            buffer.push(object);
            if buffer.len() >= Self::OBJECT_BUFFER_SIZE {
                factory.create_process_node_roots_work(std::mem::take(&mut buffer));
            }
            object
        };
        gc_tls
            .object_closure
            .set_temporarily_and_run_code(visit_object, callback);
        if !buffer.is_empty() {
            factory.create_process_node_roots_work(buffer);
        }
    }
}
