use std::cell::Cell;
use std::cell::RefCell;
use std::sync::Arc;

use crate::IPtr;
use nando_support::{
    activation_intent::{NandoActivationIntent, NandoArgument},
    epic_control::ECB,
    log_entry::TransactionLogEntry,
    ObjectId,
};

thread_local! {
    static IN_NANDO_CTX: Cell<bool> = Cell::new(false);
    static CURRENT_TXN_LOG_ENTRY: RefCell<Arc<RefCell<TransactionLogEntry>>> = RefCell::new(
        Arc::new(RefCell::new(TransactionLogEntry::new(1, None)))
    );
    static CURRENT_TXN_ECB: RefCell<Arc<RefCell<ECB>>> = panic!("no current ecb found in tls");
    // FIXME I think resizing this as part of setting up txn_meta ends up corrupting some memory
    // around it, so for now we just initialize this with a capacity of 64 and won't worry about
    // it.
    static OBJ_INTERVALS: RefCell<Vec<ObjectMapping>> = RefCell::new(Vec::with_capacity(64));
}

#[derive(Clone)]
pub struct ObjectMapping {
    pub id: ObjectId,
    pub start: usize,
    pub end: usize,
}

pub trait IntoObjectMapping {
    fn into_mapping(&self) -> ObjectMapping;
}

pub fn init_txn_context() {
    IN_NANDO_CTX.replace(true);
}

#[inline]
pub fn in_nando_context() -> bool {
    IN_NANDO_CTX.get()
}

pub fn set_txn_meta(
    log_entry_rc: Arc<RefCell<TransactionLogEntry>>,
    object_arg_mappings: &Vec<ObjectMapping>,
    maybe_ecb: Option<Arc<RefCell<ECB>>>,
) {
    if !in_nando_context() {
        // simply ignore
        return;
    }

    let _ = CURRENT_TXN_LOG_ENTRY.replace(log_entry_rc);

    if let Some(ref ecb) = maybe_ecb {
        CURRENT_TXN_ECB.set(Arc::clone(ecb));
    } else {
        #[cfg(debug_assertions)]
        println!("no txn ecb");
    }

    if object_arg_mappings.is_empty() {
        println!("no mappings in beginning of transaction");
        return;
    }

    let _ = OBJ_INTERVALS.with_borrow_mut(|intervals| {
        if intervals.capacity() < object_arg_mappings.len() {
            intervals.reserve(object_arg_mappings.len() - intervals.capacity());
        }

        unsafe {
            object_arg_mappings
                .as_ptr()
                .copy_to_nonoverlapping(intervals.as_mut_ptr(), object_arg_mappings.len());
            intervals.set_len(object_arg_mappings.len());
        };
    });
}

pub fn iptr_of(some_ptr: *const ()) -> Option<IPtr> {
    if !in_nando_context() {
        panic!("iptr_of called while not in transactional context");
    }

    OBJ_INTERVALS.with(|tree| {
        let tree = tree.borrow();
        let Some(mapping) = get_mapping_from_vaddr(&tree, some_ptr) else {
            return None;
        };

        let offset = unsafe { some_ptr.byte_offset_from(mapping.start as *const ()) };
        Some(IPtr::new(mapping.id, offset.try_into().unwrap(), 0))
    })
}

#[cfg(feature = "no-persist")]
pub fn add_new_pre_image(_some_ptr: *const (), _bytes: &[u8]) {}

#[cfg(not(feature = "no-persist"))]
pub fn add_new_pre_image(some_ptr: *const (), bytes: &[u8]) {
    if !in_nando_context() {
        // simply ignore
        return;
    }

    OBJ_INTERVALS.with(|tree| {
        let tree = tree.borrow();
        let Some(mapping) = get_mapping_from_vaddr(&tree, some_ptr) else {
            return;
        };
        let offset = unsafe { some_ptr.byte_offset_from(mapping.start as *const ()) };
        let iptr = IPtr::new(
            mapping.id,
            offset.try_into().unwrap(),
            bytes.len().try_into().unwrap(),
        );

        CURRENT_TXN_LOG_ENTRY.with_borrow(|rc| (*rc.borrow_mut()).add_new_pre_image(&iptr, bytes));
    });
}

#[cfg(feature = "no-persist")]
pub fn add_new_post_image_if_changed(_some_ptr: *const (), _bytes: &[u8]) {}

#[cfg(not(feature = "no-persist"))]
pub fn add_new_post_image_if_changed(some_ptr: *const (), bytes: &[u8]) {
    if !in_nando_context() {
        // simply ignore
        return;
    }

    OBJ_INTERVALS.with(|tree| {
        let tree = tree.borrow();
        let Some(mapping) = get_mapping_from_vaddr(&tree, some_ptr) else {
            return;
        };
        let offset = unsafe { some_ptr.byte_offset_from(mapping.start as *const ()) };
        let iptr = IPtr::new(
            mapping.id,
            offset.try_into().unwrap(),
            bytes.len().try_into().unwrap(),
        );

        CURRENT_TXN_LOG_ENTRY
            .with_borrow(|rc| rc.borrow_mut().add_new_post_image_if_changed(&iptr, bytes));
    });
}

pub fn get_last_unresolved_arg_idx() -> Option<usize> {
    if !in_nando_context() {
        // simply ignore
        return None;
    }

    Some(CURRENT_TXN_ECB.with_borrow(|rc| (*rc.borrow()).get_last_unresolved_arg_idx()))
}

pub fn get_last_unresolved_arg() -> Option<NandoArgument> {
    if !in_nando_context() {
        // simply ignore
        return None;
    }

    CURRENT_TXN_ECB.with_borrow(|rc| (*rc.borrow()).get_last_unresolved_arg())
}

pub fn add_new_pending_intent(pending_intent: NandoActivationIntent) -> NandoArgument {
    if !in_nando_context() {
        // simply ignore
        return NandoArgument::UnresolvedArgument(0);
    }

    CURRENT_TXN_ECB.with_borrow(|rc| (*rc.borrow_mut()).add_new_spawned_task(pending_intent))
}

pub fn add_continuation_intent(continuation_intent: NandoActivationIntent) -> NandoArgument {
    if !in_nando_context() {
        // simply ignore
        return NandoArgument::UnresolvedArgument(0);
    }

    CURRENT_TXN_ECB.with_borrow(|rc| {
        (*rc.borrow_mut()).add_new_spawned_task_and_update_dependencies(continuation_intent)
    })
}

pub fn add_pending_with_control_dependencies(
    pending_intent: NandoActivationIntent,
) -> NandoArgument {
    if !in_nando_context() {
        // simply ignore
        return NandoArgument::UnresolvedArgument(0);
    }

    CURRENT_TXN_ECB.with_borrow(|rc| {
        (*rc.borrow_mut()).add_new_spawned_task_with_control_dependencies(pending_intent)
    })
}

pub fn add_continuation_with_control_dependencies(
    pending_intent: NandoActivationIntent,
    dependencies: &Vec<NandoArgument>,
) -> NandoArgument {
    if !in_nando_context() {
        // simply ignore
        return NandoArgument::UnresolvedArgument(0);
    }

    CURRENT_TXN_ECB.with_borrow(|rc| {
        (*rc.borrow_mut()).add_continuation_with_control_dependencies(pending_intent, dependencies)
    })
}

pub fn mark_last_task_as_fold() {
    if !in_nando_context() {
        // simply ignore
        return;
    }

    CURRENT_TXN_ECB.with_borrow(|rc| (*rc.borrow_mut()).mark_last_task_as_fold());
}

pub fn get_current_namespace() -> String {
    CURRENT_TXN_LOG_ENTRY.with_borrow(|rc| rc.borrow().current_namespace.clone())
}

pub fn set_current_namespace(namespace: &str) {
    CURRENT_TXN_LOG_ENTRY
        .with_borrow(|rc| (*rc.borrow_mut()).current_namespace = namespace.to_string());
}

pub fn add_allocated_object_to_write_set(object_id: ObjectId) {
    if !in_nando_context() {
        // simply ignore
        return;
    }

    // FIXME @hack
    CURRENT_TXN_LOG_ENTRY
        .with_borrow(|rc| (*rc.borrow_mut()).add_object_to_write_set(object_id, 0));
}

fn get_mapping_from_vaddr(
    intervals: &Vec<ObjectMapping>,
    vaddr: *const (),
) -> Option<&'_ ObjectMapping> {
    let vaddr = vaddr as usize;
    intervals
        .iter()
        .find(|oi| oi.start <= vaddr && vaddr <= oi.end)
}
