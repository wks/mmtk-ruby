use crate::abi::GCThreadTLS;

use crate::{upcalls, Ruby, RubyEdge};
use mmtk::scheduler::GCWorker;
use mmtk::util::{ObjectReference, VMWorkerThread};
use mmtk::vm::{EdgeVisitor, ObjectTracer, RootsWorkFactory, Scanning};
use mmtk::{memory_manager, Mutator, MutatorContext};

pub struct VMScanning {}

impl Scanning<Ruby> for VMScanning {
    const SINGLE_THREAD_MUTATOR_SCANNING: bool = false;

    fn support_edge_enqueuing(_tls: VMWorkerThread, _object: ObjectReference) -> bool {
        false
    }

    fn scan_object<EV: EdgeVisitor<RubyEdge>>(
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
        debug_assert!(
            mmtk::memory_manager::is_mmtk_object(object.to_raw_address()),
            "Not an MMTk object: {}",
            object,
        );
        let gc_tls = unsafe { GCThreadTLS::from_vwt_check(tls) };
        let visit_object = |_worker, target_object: ObjectReference, pin| {
            trace!("Tracing object: {} -> {}", object, target_object);
            // debug_assert!(
            //     mmtk::memory_manager::is_mmtk_object(object.to_raw_address()),
            //     "Source is not an MMTk object. Src: {} dst: {}",
            //     object,
            //     target_object
            // );
            // debug_assert!(
            //     true || mmtk::memory_manager::is_mmtk_object(target_object.to_raw_address()),
            //     "Destination is not an MMTk object. Src: {} dst: {}",
            //     object,
            //     target_object
            // );
            let forwarded = object_tracer.trace_object(target_object);
            if pin {
                debug_assert_eq!(forwarded, target_object);
            }
            forwarded
        };
        gc_tls
            .object_closure
            .set_temporarily_and_run_code(visit_object, || {
                (upcalls().scan_object_ruby_style)(object);
            });
    }

    fn notify_initial_thread_scan_complete(_partial_scan: bool, _tls: VMWorkerThread) {
        // Do nothing
    }

    fn scan_thread_roots(_tls: VMWorkerThread, _factory: impl RootsWorkFactory<RubyEdge>) {
        unreachable!();
    }

    fn scan_thread_root(
        tls: VMWorkerThread,
        mutator: &'static mut Mutator<Ruby>,
        mut factory: impl RootsWorkFactory<RubyEdge>,
    ) {
        let gc_tls = unsafe { GCThreadTLS::from_vwt_check(tls) };
        Self::collect_object_roots_in("scan_thread_root", gc_tls, &mut factory, || {
            (upcalls().scan_thread_root)(mutator.get_tls(), tls);
        });
    }

    fn scan_vm_specific_roots(tls: VMWorkerThread, mut factory: impl RootsWorkFactory<RubyEdge>) {
        let gc_tls = unsafe { GCThreadTLS::from_vwt_check(tls) };
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

    fn process_weak_refs(
        worker: &mut GCWorker<Ruby>,
        tracer_context: impl mmtk::vm::ObjectTracerContext<Ruby>,
    ) -> bool {
        crate::binding()
            .weak_proc
            .process_weak_stuff(worker, tracer_context);

        {
            let gc_tls = unsafe { GCThreadTLS::from_vwt_check(worker.tls) };

            info!("Verifying if any root points to from-space object");
            let all_roots: Vec<ObjectReference> = {
                let mut root_set = crate::binding().root_set.lock().unwrap();
                std::mem::take(root_set.as_mut())
            };
            for obj in all_roots {
                if !memory_manager::is_mmtk_object(obj.to_address::<Ruby>()) {
                    error!("Root doesn't point to mmtk object: {}", obj);
                }
                if obj.is_live() {
                    // if obj.is_forwarded::<Ruby>() {
                    //     error!("Root is forwarded: {}", obj);
                    // }
                } else {
                    error!("Root is dead: {}", obj);
                }

            }

            if false {
                info!("Verifying if any object points to from-space object");
                let mut all_objects = crate::binding().all_objects.lock().unwrap();
                let mut new_all_objects = vec![];
                for obj in all_objects.drain(..) {
                    if obj.is_live() {
                        // if obj.is_forwarded::<Ruby>() {
                        //     info!("  {} is forwarded", obj);
                        // }
                        let maybe_fwd = obj.get_forwarded_object();
                        if let Some(fwd) = maybe_fwd {
                            info!("  {} forwarded to {}", obj, fwd);
                        } else {
                            info!("  {} is not forwarded", obj);
                        }
                        let actual = maybe_fwd.unwrap_or(obj);

                        let visit_object = |_, target: ObjectReference, _pin| {
                            if target.is_forwarded::<Ruby>() {
                                panic!("{} {} points to a forwarded object {}", actual, crate::collection::object_type_str(actual) ,target);
                            }
                            target
                        };

                        gc_tls
                            .object_closure
                            .set_temporarily_and_run_code(visit_object, || {
                                (upcalls().scan_object_ruby_style)(actual);
                            });

                        new_all_objects.push(actual);
                    }
                }
                *all_objects = new_all_objects;
                info!("Verification complete");
            }
        }

        false
    }

    fn forward_weak_refs(
        _worker: &mut GCWorker<Ruby>,
        _tracer_context: impl mmtk::vm::ObjectTracerContext<Ruby>,
    ) {
        panic!("We can't use MarkCompact in Ruby.");
    }
}

impl VMScanning {
    const OBJECT_BUFFER_SIZE: usize = 4096;

    fn collect_object_roots_in<F: FnMut()>(
        root_scan_kind: &str,
        gc_tls: &mut GCThreadTLS,
        factory: &mut impl RootsWorkFactory<RubyEdge>,
        callback: F,
    ) {
        let mut buffer: Vec<ObjectReference> = Vec::new();
        let mut my_roots = vec![];
        let mut my_pinned_roots = vec![];
        let visit_object = |_, object: ObjectReference, _pin| {
            debug!("[{}] Visiting root: {}", root_scan_kind, object);
            my_roots.push(object);
            if memory_manager::pin_object::<Ruby>(object) {
                my_pinned_roots.push(object);
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

        info!("Pinned {} node roots", my_pinned_roots.len());

        {
            let mut pinned_roots = crate::binding().pinned_roots.lock().unwrap();
            pinned_roots.append(&mut my_pinned_roots);
        }

        {
            crate::binding().root_set.lock().unwrap().append(&mut my_roots);
        }
    }
}
