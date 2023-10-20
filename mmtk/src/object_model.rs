use std::ptr::copy_nonoverlapping;

use crate::abi::{RubyObjectAccess, MIN_OBJ_ALIGN, OBJREF_OFFSET};
use crate::{abi, Ruby};
use mmtk::util::constants::BITS_IN_BYTE;
use mmtk::util::copy::{CopySemantics, GCWorkerCopyContext};
use mmtk::util::{Address, ObjectReference};
use mmtk::vm::*;

pub struct VMObjectModel {}

impl VMObjectModel {
    const OBJREF_OFFSET: usize = abi::OBJREF_OFFSET;
}

impl ObjectModel<Ruby> for VMObjectModel {
    type VMForwardingDataType = usize;

    const GLOBAL_LOG_BIT_SPEC: VMGlobalLogBitSpec = VMGlobalLogBitSpec::side_first();

    // We overwrite the prepended word which were used to hold object sizes.
    const LOCAL_FORWARDING_POINTER_SPEC: VMLocalForwardingPointerSpec =
        VMLocalForwardingPointerSpec::in_header(-((OBJREF_OFFSET * BITS_IN_BYTE) as isize));

    const LOCAL_FORWARDING_BITS_SPEC: VMLocalForwardingBitsSpec =
        VMLocalForwardingBitsSpec::side_first();

    const LOCAL_MARK_BIT_SPEC: VMLocalMarkBitSpec =
        VMLocalMarkBitSpec::side_after(Self::LOCAL_FORWARDING_BITS_SPEC.as_spec());

    const LOCAL_PINNING_BIT_SPEC: VMLocalPinningBitSpec =
        VMLocalPinningBitSpec::side_after(Self::LOCAL_MARK_BIT_SPEC.as_spec());

    const LOCAL_LOS_MARK_NURSERY_SPEC: VMLocalLOSMarkNurserySpec =
        VMLocalLOSMarkNurserySpec::side_after(Self::LOCAL_PINNING_BIT_SPEC.as_spec());

    const UNIFIED_OBJECT_REFERENCE_ADDRESS: bool = false;
    const OBJECT_REF_OFFSET_LOWER_BOUND: isize = Self::OBJREF_OFFSET as isize;

    const NEED_VO_BITS_DURING_TRACING: bool = true;

    fn copy(
        from: ObjectReference,
        semantics: CopySemantics,
        copy_context: &mut GCWorkerCopyContext<Ruby>,
        vm_data: Self::VMForwardingDataType,
    ) -> ObjectReference {
        let from_acc = RubyObjectAccess::from_objref(from);
        let maybe_givtbl = from_acc.get_original_givtbl();
        let from_start = from_acc.obj_start();
        let object_size = from_acc.object_size();
        let to_start = copy_context.alloc_copy(from, object_size, MIN_OBJ_ALIGN, 0, semantics);
        let to_payload = to_start.add(OBJREF_OFFSET);
        unsafe {
            copy_nonoverlapping::<u8>(from_start.to_ptr(), to_start.to_mut_ptr(), object_size);
        }

        // The `flags` field of the from-space copy is overwritten to `T_MOVED`.
        // Reconstruct the `flags` of the to-space copy.
        unsafe {
            to_payload.store::<usize>(vm_data);
        }

        let to_obj = ObjectReference::from_raw_address(to_payload);
        copy_context.post_copy(to_obj, object_size, semantics);
        trace!("Copied object from {} to {}", from, to_obj);

        #[cfg(feature = "clear_old_copy")]
        {
            trace!(
                "Clearing old copy {} ({}-{})",
                from,
                from_start,
                from_start + object_size
            );
            // For debug purpose, we clear the old copy so that if the Ruby VM reads from the old
            // copy again, it will likely result in an error.
            unsafe { std::ptr::write_bytes::<u8>(from_start.to_mut_ptr(), 0, object_size) }
        }

        if let Some(givtbl) = maybe_givtbl {
            {
                let mut moved_givtbl = crate::binding().moved_givtbl.lock().unwrap();
                moved_givtbl.insert(
                    to_obj,
                    crate::binding::MovedGIVTblEntry {
                        old_objref: from,
                        gen_ivtbl: givtbl,
                    },
                );
            }
            let to_acc = RubyObjectAccess::from_objref(to_obj);
            to_acc.set_has_moved_givtbl();
        }

        to_obj
    }

    fn copy_to(_from: ObjectReference, _to: ObjectReference, _region: Address) -> Address {
        unimplemented!(
            "This function cannot be called because we do not support MarkCompact for Ruby."
        )
    }

    fn get_reference_when_copied_to(_from: ObjectReference, _to: Address) -> ObjectReference {
        unimplemented!(
            "This function cannot be called because we do not support MarkCompact for Ruby."
        )
    }

    fn get_current_size(object: ObjectReference) -> usize {
        RubyObjectAccess::from_objref(object).object_size()
    }

    fn get_type_descriptor(_reference: ObjectReference) -> &'static [i8] {
        todo!()
    }

    fn ref_to_address(object: ObjectReference) -> Address {
        object.to_raw_address()
    }

    fn ref_to_object_start(object: ObjectReference) -> Address {
        RubyObjectAccess::from_objref(object).obj_start()
    }

    fn ref_to_header(object: ObjectReference) -> Address {
        RubyObjectAccess::from_objref(object).payload_addr()
    }

    fn address_to_ref(addr: Address) -> ObjectReference {
        ObjectReference::from_raw_address(addr)
    }

    fn get_size_when_copied(object: ObjectReference) -> usize {
        Self::get_current_size(object)
    }

    fn get_align_when_copied(_object: ObjectReference) -> usize {
        todo!()
    }

    fn get_align_offset_when_copied(_object: ObjectReference) -> usize {
        todo!()
    }

    fn dump_object(_object: ObjectReference) {
        todo!()
    }

    fn attempt_to_forward(object: ObjectReference) -> Option<Self::VMForwardingDataType> {
        RubyObjectAccess::from_objref(object).attempt_to_forward()
    }

    fn write_forwarding_state_and_forwarding_pointer(
        object: ObjectReference,
        new_object: ObjectReference,
    ) {
        RubyObjectAccess::from_objref(object)
            .write_forwarding_state_and_forwarding_pointer(new_object)
    }

    fn revert_forwarding_state(object: ObjectReference, vm_data: usize) {
        RubyObjectAccess::from_objref(object).revert_forwarding_state(vm_data)
    }

    fn spin_and_get_forwarded_object(object: ObjectReference) -> ObjectReference {
        RubyObjectAccess::from_objref(object).spin_and_get_forwarded_object()
    }

    fn is_forwarded(object: ObjectReference) -> bool {
        RubyObjectAccess::from_objref(object).is_forwarded()
    }

    fn read_forwarding_pointer(object: ObjectReference) -> ObjectReference {
        RubyObjectAccess::from_objref(object).load_forwarding_pointer()
    }
}
