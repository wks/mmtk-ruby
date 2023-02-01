use crate::abi::GCThreadTLS;

use crate::{mmtk, upcalls, Ruby};
use mmtk::scheduler::*;
use mmtk::util::{ObjectReference, VMMutatorThread, VMThread, VMWorkerThread};
use mmtk::vm::{Collection, GCThreadContext};
use mmtk::{memory_manager, MutatorContext};
use std::borrow::Cow;
use std::collections::HashSet;
use std::thread;

pub struct VMCollection {}

pub fn show_cstr(cstr: *const libc::c_char) -> Cow<'static, str> {
    if cstr.is_null() {
        return Cow::Borrowed("(null)")
    }
    let type_cstr = unsafe { std::ffi::CStr::from_ptr(cstr) };
    let type_str = String::from_utf8_lossy(type_cstr.to_bytes());
    type_str
}

pub fn object_type_str(object: ObjectReference) -> Cow<'static, str> {
    show_cstr((upcalls().object_type_str)(object))
}

pub fn detail_type_str(object: ObjectReference) -> String {
    let orig = show_cstr((upcalls().detail_type_str)(object));
    let one_line = orig.replace("\n", "");
    let max_len = 50;
    if one_line.len() > max_len {
        format!("{}...", one_line.chars().take(max_len).collect::<String>())
    } else {
        one_line
    }
}

fn is_exivar(object: ObjectReference) -> bool {
    (upcalls().is_exivar)(object)
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
        let mut edges_count = 0;
        let mut pinning_edges_count = 0;
        let mut pin_set = HashSet::<ObjectReference>::new();
        let mut pinned_ppps = crate::binding().pinned_ppps.borrow_mut();
        let mut objects_pinned = 0;
        crate::binding().ppp_registry.foreach(|obj| {
            log::trace!(
                "  PPP#{}: {}{} {} {}",
                ppp_count,
                obj,
                if is_exivar(obj) { "|FL_EXIVAR" } else { "" },
                object_type_str(obj),
                detail_type_str(obj),
            );
            ppp_count += 1;

            let visit_object = |_worker, target_object: ObjectReference, pin| {
                log::trace!(
                    "    -> {} {} {} {}",
                    if pin { "(pin)" } else { "     " },
                    target_object,
                    object_type_str(target_object),
                    detail_type_str(target_object),
                );
                edges_count += 1;
                if pin {
                    if memory_manager::pin_object::<Ruby>(target_object) {
                        pinned_ppps.push(target_object);
                        objects_pinned += 1;
                    }

                    pinning_edges_count += 1;
                    pin_set.insert(target_object);
                }
                target_object
            };
            gc_tls
                .object_closure
                .set_temporarily_and_run_code(visit_object, || {
                    (upcalls().scan_object_ruby_style)(obj);
                });
        });
        log::trace!(
            "Total: {} PPPs, {} edges, {} pinning edges, {} unique objects pinned.",
            ppp_count,
            edges_count,
            pinning_edges_count,
            pin_set.iter().len(),
        );
        log::trace!(
            "{} objects actually pinned by MMTk.",
            objects_pinned,
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
