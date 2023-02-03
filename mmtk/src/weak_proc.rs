use std::sync::Mutex;

use mmtk::{
    scheduler::GCWorker,
    util::ObjectReference,
    vm::{ObjectModel, ObjectTracer, ObjectTracerContext}, memory_manager,
};

use crate::{abi::GCThreadTLS, object_model::VMObjectModel, upcalls, Ruby};

pub struct WeakProcessor {
    /// Objects that needs `obj_free` called when dying.
    /// If it is a bottleneck, replace it with a lock-free data structure,
    /// or add candidates in batch.
    obj_free_candidates: Mutex<Vec<ObjectReference>>,
}

impl Default for WeakProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl WeakProcessor {
    pub fn new() -> Self {
        Self {
            obj_free_candidates: Mutex::new(Vec::new()),
        }
    }

    /// Add an object as a candicate for `obj_free`.
    ///
    /// Multiple mutators can call it concurrently, so it has `&self`.
    pub fn add_obj_free_candidate(&self, object: ObjectReference) {
        let mut obj_free_candidates = self.obj_free_candidates.lock().unwrap();
        obj_free_candidates.push(object);
    }

    pub fn get_all_obj_free_candidates(&self) -> Vec<ObjectReference> {
        let mut obj_free_candidates = self.obj_free_candidates.lock().unwrap();
        std::mem::take(obj_free_candidates.as_mut())
    }

    pub fn process_weak_stuff(
        &self,
        worker: &mut GCWorker<Ruby>,
        tracer_context: impl ObjectTracerContext<Ruby>,
    ) {
        let gc_tls = unsafe { GCThreadTLS::from_vwt_check(worker.tls) };

        // If it blocks, it is a bug.
        let mut obj_free_candidates = self
            .obj_free_candidates
            .try_lock()
            .expect("It's GC time.  No mutators should hold this lock at this time.");

        // Enable tracer in this scope.
        tracer_context.with_tracer(worker, |tracer| {
            // Process obj_free
            let mut new_candidates = Vec::new();

            for object in obj_free_candidates.iter().copied() {
//                info!("Processing obj_free candidate: {}", object);
                if object.is_reachable() {
                    // Forward and add back to the candidate list.
                    let new_object = tracer.trace_object(object);
                    // info!(
                    //     "Forwarding obj_free candidate: {} -> {}",
                    //     object,
                    //     new_object
                    // );
                    new_candidates.push(new_object);
                } else {
//                    info!("  Dead. Call obj_free...: {}", object);
                    (upcalls().call_obj_free)(object);
                }
            }

            *obj_free_candidates = new_candidates;

            // Forward global weak tables
            let forward_object = |_worker, object: ObjectReference, pin: bool| {
                debug_assert!(!pin);
                // debug_assert!(mmtk::memory_manager::is_mmtk_object(
                //     VMObjectModel::ref_to_address(object)
                // ));
                let result = tracer.trace_object(object);
                trace!("Forwarding reference: {} -> {}", object, result);
                result
            };

            gc_tls
                .object_closure
                .set_temporarily_and_run_code(forward_object, || {
                    log::debug!("Updating global weak tables...");
                    (upcalls().update_global_weak_tables)();
                    log::debug!("Finished updating global weak tables.");
                });

            log::info!("Removing dead PPPs...");

            let mut ppp_count = 0;
            let mut retain_count = 0;
            crate::binding().ppp_registry.retain_mut(|obj| {
                ppp_count += 1;
                if obj.is_live() {
                    let forwarded = obj.get_forwarded_object().unwrap_or(*obj);
                    if forwarded.to_raw_address().as_usize() == 0x8 {
                        panic!("8!");
                    }
                    *obj = forwarded;
                    retain_count += 1;
                    true
                } else {
                    log::info!("  PPP removed: {}", *obj);
                    false
                }
            });
            log::info!("Total: {} old PPPs, {} new PPPs.", ppp_count, retain_count,);

            log::info!("Unpinning pinned roots...");
            let mut roots_unpinned = 0;
            {
                let mut objects = crate::binding().pinned_roots.lock().unwrap();
                for object in objects.drain(..) {
                    memory_manager::unpin_object::<Ruby>(object);
                    roots_unpinned += 1;
                }
            }
            log::info!("Finished unpinning roots. {} roots unpinned.", roots_unpinned);

            log::info!("Unpinning pinned PPPs...");
            let mut ppps_unpinned = 0;
            {
                let mut objects = crate::binding().pinned_ppps.borrow_mut();
                for object in objects.drain(..) {
                    memory_manager::unpin_object::<Ruby>(object);
                    ppps_unpinned += 1;
                }
            }
            log::info!("Finished unpinning PPPs. {} PPPs unpinned.", ppps_unpinned);
        });
    }
}
