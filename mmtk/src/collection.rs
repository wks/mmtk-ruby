use crate::abi::GCThreadTLS;

use crate::{mmtk, upcalls, Ruby};
use mmtk::scheduler::*;
use mmtk::util::{ObjectReference, VMMutatorThread, VMThread, VMWorkerThread};
use mmtk::vm::{Collection, GCThreadContext};
use mmtk::{memory_manager, MutatorContext};
use std::borrow::Cow;
use std::thread;

pub struct VMCollection {}

fn show_cstr(cstr: *const libc::c_char) -> Cow<'static, str> {
    if cstr.is_null() {
        return Cow::Borrowed("(null)")
    }
    let type_cstr = unsafe { std::ffi::CStr::from_ptr(cstr) };
    let type_str = String::from_utf8_lossy(type_cstr.to_bytes());
    type_str
}

fn object_type_str(object: ObjectReference) -> Cow<'static, str> {
    show_cstr((upcalls().object_type_str)(object))
}

fn detail_type_str(object: ObjectReference) -> Cow<'static, str> {
    show_cstr((upcalls().detail_type_str)(object))
}

impl Collection<Ruby> for VMCollection {
    const COORDINATOR_ONLY_STW: bool = true;

    fn stop_all_mutators<F>(tls: VMWorkerThread, _mutator_visitor: F)
    where
        F: FnMut(&'static mut mmtk::Mutator<Ruby>),
    {
        (upcalls().stop_the_world)(tls);

        log::info!("The world stopped. Now enumerating ppps...");
        let gc_tls = unsafe { GCThreadTLS::from_vwt_check(tls) };

        let mut ppp_count = 0;
        let mut pinned_count = 0;
        crate::binding().ppp_registry.foreach(|obj| {
            log::info!(
                "  PPP#{}: {} {} {}",
                ppp_count,
                obj,
                object_type_str(obj),
                detail_type_str(obj)
            );
            ppp_count += 1;

            let visit_object = |_worker, target_object: ObjectReference| {
                log::info!(
                    "    -> pins: {} {} {}",
                    target_object,
                    object_type_str(target_object),
                    detail_type_str(target_object)
                );
                pinned_count += 1;
                target_object
            };
            gc_tls
                .object_closure
                .set_temporarily_and_run_code(visit_object, || {
                    (upcalls().scan_object_ruby_style)(obj);
                });
        });
        log::info!(
            "Total: {} PPPs, {} objects pinned.",
            ppp_count,
            pinned_count
        );
    }

    fn resume_mutators(tls: VMWorkerThread) {
        (upcalls().resume_mutators)(tls);
    }

    fn block_for_gc(tls: VMMutatorThread) {
        (upcalls().block_for_gc)(tls);
    }

    fn spawn_gc_thread(_tls: VMThread, ctx: GCThreadContext<Ruby>) {
        match ctx {
            GCThreadContext::Controller(mut controller) => {
                thread::Builder::new()
                    .name("MMTk Controller Thread".to_string())
                    .spawn(move || {
                        debug!("Hello! This is MMTk Controller Thread running!");
                        let ptr_controller = &mut *controller as *mut GCController<Ruby>;
                        let gc_thread_tls =
                            Box::into_raw(Box::new(GCThreadTLS::for_controller(ptr_controller)));
                        (upcalls().init_gc_worker_thread)(gc_thread_tls);
                        memory_manager::start_control_collector(
                            mmtk(),
                            GCThreadTLS::to_vwt(gc_thread_tls),
                            &mut controller,
                        )
                    })
                    .unwrap();
            }
            GCThreadContext::Worker(mut worker) => {
                thread::Builder::new()
                    .name("MMTk Worker Thread".to_string())
                    .spawn(move || {
                        debug!("Hello! This is MMTk Worker Thread running!");
                        let ptr_worker = &mut *worker as *mut GCWorker<Ruby>;
                        let gc_thread_tls =
                            Box::into_raw(Box::new(GCThreadTLS::for_worker(ptr_worker)));
                        (upcalls().init_gc_worker_thread)(gc_thread_tls);
                        memory_manager::start_worker(
                            mmtk(),
                            GCThreadTLS::to_vwt(gc_thread_tls),
                            &mut worker,
                        )
                    })
                    .unwrap();
            }
        }
    }

    fn prepare_mutator<T: MutatorContext<Ruby>>(
        _tls_worker: VMWorkerThread,
        _tls_mutator: VMMutatorThread,
        _m: &T,
    ) {
        // do nothing
    }
}
