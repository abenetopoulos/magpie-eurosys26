use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::Once;

#[macro_export]
macro_rules! resolve_object {
    ($args:ident, $idx:expr, $err_msg:expr, $rt:ty, $le:expr) => {{
        match $args.get($idx).expect(&$err_msg) {
            ResolvedNandoArgument::Object(o) => match o {
                ObjectArgument::RWObject(o) => {
                    $le.borrow_mut()
                        .add_object_to_read_set(o.get_id(), o.get_version());
                    let inner = unsafe {
                        o.read_into::<$rt>(None)
                            .expect("failed to resolve RW object")
                            .as_mut()
                    }
                    .unwrap();
                    o.maybe_set_inner_allocator(inner);
                    inner
                }
                _ => panic!("Could not resolve nando object arg {}", $idx),
            },
            o @ _ => panic!("Could not resolve nando object arg {}", $idx),
        }
    }};
}

#[macro_export]
macro_rules! resolve_maybe_object {
    ($args:ident, $idx:expr, $err_msg:expr, $rt:ty, $le:expr) => {{
        match $args.get($idx).expect(&$err_msg) {
            ResolvedNandoArgument::Value(ScalarValue::Nil) => None,
            ResolvedNandoArgument::Object(_) => {
                Some(resolve_object!($args, $idx, $err_msg, $rt, $le))
            }
            o @ _ => panic!("Could not resolve nando object arg {}", $idx),
        }
    }};
}

#[macro_export]
macro_rules! resolve_read_only_object {
    ($args:ident, $idx:expr, $err_msg:expr, $rt:ty, $le:expr) => {{
        match $args.get($idx).expect(&$err_msg) {
            ResolvedNandoArgument::Object(o) => match o {
                ObjectArgument::RWObject(o) => {
                    $le.borrow_mut()
                        .add_object_to_read_set(o.get_id(), o.get_version());
                    unsafe {
                        &*o.read_into::<$rt>(None)
                            .expect("failed to resolve RW object as RO object")
                    }
                }
                ObjectArgument::ROObject(o) => {
                    $le.borrow_mut()
                        .add_object_to_read_set(o.get_id(), o.get_version());
                    o.read_into::<$rt>()
                }
                _ => panic!("Could not resolve nando object arg {}", $idx),
            },
            o @ _ => panic!("Could not resolve read-only nando object arg {}", $idx),
        }
    }};
}

#[macro_export]
macro_rules! resolve_read_only_maybe_object {
    ($args:ident, $idx:expr, $err_msg:expr, $rt:ty, $le:expr) => {{
        match $args.get($idx).expect(&$err_msg) {
            ResolvedNandoArgument::Value(ScalarValue::Nil) => None,
            ResolvedNandoArgument::Object(_) => {
                Some(resolve_read_only_object!($args, $idx, $err_msg, $rt, $le))
            }
            o @ _ => panic!("Could not resolve nando object arg {}", $idx),
        }
    }};
}

#[macro_export]
macro_rules! resolve_objects {
    ($args:ident, $idx:expr, $err_msg:expr, $rt:ty, $le:expr) => {{
        match $args.get($idx).expect(&$err_msg) {
            ResolvedNandoArgument::Objects(ref os) => {
                let mut resolved_objects = Vec::with_capacity(os.len());
                for o in os {
                    match o {
                        ObjectArgument::RWObject(o) => {
                            $le.borrow_mut()
                                .add_object_to_read_set(o.get_id(), o.get_version());
                            let inner = unsafe {
                                o.read_into::<$rt>(None)
                                    .expect("failed to resolve RW object")
                                    .as_mut()
                            }
                            .unwrap();
                            o.maybe_set_inner_allocator(inner);
                            resolved_objects.push(inner);
                        }
                        _ => panic!("Could not resolve vec ref arg {}: {:?}", $idx, o),
                    }
                }
                resolved_objects
            }
            o @ _ => panic!(
                "Could not resolve vec ref arg, not vec ref {}: {:?}",
                $idx, o
            ),
        }
    }};
}

#[macro_export]
macro_rules! resolve_read_only_objects {
    ($args:ident, $idx:expr, $err_msg:expr, $rt:ty, $le:expr) => {{
        match $args.get($idx).expect(&$err_msg) {
            ResolvedNandoArgument::Objects(ref os) => {
                let mut resolved_objects = Vec::with_capacity(os.len());
                for o in os {
                    let resolved_object = match o {
                        ObjectArgument::RWObject(o) => {
                            $le.borrow_mut()
                                .add_object_to_read_set(o.get_id(), o.get_version());
                            unsafe {
                                &*o.read_into::<$rt>(None)
                                    .expect("failed to resolve RW object as RO object")
                            }
                        }
                        ObjectArgument::ROObject(o) => {
                            $le.borrow_mut()
                                .add_object_to_read_set(o.get_id(), o.get_version());
                            o.read_into::<$rt>()
                        }
                        _ => panic!("Could not resolve vec ref arg {}: {:?}", $idx, o),
                    };
                    resolved_objects.push(resolved_object);
                }

                resolved_objects
            }
            o @ _ => panic!(
                "Could not resolve vec ref arg, not vec ref {}: {:?}",
                $idx, o
            ),
        }
    }};
}

pub fn get_name_cache() -> &'static mut HashMap<String, String> {
    static mut INSTANCE: MaybeUninit<HashMap<String, String>> = MaybeUninit::uninit();
    static mut ONCE: Once = Once::new();
    unsafe {
        ONCE.call_once(|| {
            INSTANCE.as_mut_ptr().write(HashMap::with_capacity(64));
        });
    }
    unsafe { &mut *INSTANCE.as_mut_ptr() }
}

pub fn strip_trailing_generics(intent_name: &str) -> String {
    let name_cache = get_name_cache();

    match name_cache.get(intent_name) {
        Some(s) => s.clone(),
        None => {
            let stripped = match intent_name.contains("::") {
                false => intent_name.to_string(),
                true => match intent_name.ends_with('>') {
                    false => intent_name.to_string(),
                    true => {
                        let split_intent_name: Vec<&str> = intent_name.split("::").collect();
                        split_intent_name[0..split_intent_name.len() - 1].join("::")
                    }
                },
            };

            name_cache.insert(intent_name.to_string(), stripped.clone());
            stripped
        }
    }
}

#[macro_export]
macro_rules! format_intent_name {
    ($id:literal) => {{
        let base_id = nando_support::strip_trailing_generics($id);
        let namespace = object_lib::tls::get_current_namespace();
        match namespace.len() {
            0 => base_id.to_string(),
            _ => format!("{}::{}", namespace, base_id),
        }
    }};
}

#[macro_export]
macro_rules! nando_spawn {
    ($id:literal, $($arg:expr),*) => {{
        let mut spawn_intent = $crate::NandoActivationIntent {
            host_idx: None, name: $id.into(), args: vec![
                $($arg.into(),)*
            ],
        };
        object_lib::tls::add_new_pending_intent(spawn_intent)
    }};
}

#[macro_export]
macro_rules! nando_spawn_polymorphic {
    ($id:literal, $($arg:tt),* $(,)?) => {{
        let namespaced_name = format_intent_name!($id);
        let mut spawn_intent = $crate::NandoActivationIntent {
            host_idx: None, name: namespaced_name, args: vec![
                $($arg.into(),)*
            ],
        };
        object_lib::tls::add_new_pending_intent(spawn_intent)
    }};
}

#[macro_export]
macro_rules! nando_yield {
    ($id:literal, $($arg:expr),*) => {{
        let mut continuation_intent = $crate::NandoActivationIntent {
            host_idx: None, name: $id.into(), args: vec![
                $($arg.into(),)*
            ],
        };
        object_lib::tls::add_continuation_intent(continuation_intent)
    }};
}

#[macro_export]
macro_rules! nando_yield_polymorphic {
    ($id:literal, $($arg:tt),*) => {{
        let namespaced_name = format_intent_name!($id);
        let mut continuation_intent = $crate::NandoActivationIntent {
            host_idx: None, name: namespaced_name, args: vec![
                $($arg.into(),)*
            ],
        };
        object_lib::tls::add_continuation_intent(continuation_intent)
    }};
}

#[macro_export]
macro_rules! nando_spawn_sink {
    ($id:literal, $($arg:tt),*) => {{
        let mut spawn_intent = $crate::NandoActivationIntent {
            host_idx: None, name: $id.into(), args: vec![
                $($arg.into(),)*
            ],
        };
        object_lib::tls::add_pending_with_control_dependencies(spawn_intent)
    }};
}

#[macro_export]
macro_rules! nando_yield_sink {
    ($id:literal, $deps:expr, $($arg:tt),*) => {{
        let mut spawn_intent = $crate::NandoActivationIntent {
            host_idx: None, name: $id.into(), args: vec![
                $($arg.into(),)*
            ],
        };
        object_lib::tls::add_continuation_with_control_dependencies(spawn_intent, $deps)
    }};
}

#[macro_export]
macro_rules! nando_yield_sink_polymorphic {
    ($id:literal, $deps:expr, $($arg:tt),*) => {{
        let namespaced_name = format_intent_name!($id);
        let mut spawn_intent = $crate::NandoActivationIntent {
            host_idx: None, name: namespaced_name, args: vec![
                $($arg.into(),)*
            ],
        };
        object_lib::tls::add_continuation_with_control_dependencies(spawn_intent, $deps)
    }};
}

#[macro_export]
macro_rules! fold_polymorphic {
    ($id:literal, $($arg:tt),*) => {{
        let spawn_res = nando_spawn_polymorphic!($id, $($arg,)*);

        // FIXME better mechanism for this
        object_lib::tls::mark_last_task_as_fold();

        spawn_res
    }};
}

#[macro_export]
macro_rules! bump_if_changed {
    ($obj:ident, $log_entry:expr) => {{
        match $obj {
            ObjectArgument::RWObject(o) => {
                let mut log_entry = $log_entry.borrow_mut();
                if log_entry.object_was_modified(o.get_id()) {
                    o.bump_version().expect("failed to bump object version");

                    #[cfg(debug_assertions)]
                    println!(
                        "bumped object {} to version {}",
                        o.get_id(),
                        o.get_version()
                    );

                    log_entry.add_object_to_write_set(o.get_id(), o.get_version());
                } else {
                    // Assume object was only read, update read counter. This only works for "real" objects
                    // -- for `MaterializedObject` instances, the backing object's read-access counter is
                    // updated at resolution time.
                    o.mark_read_access();
                }
            },
            _ => {},
        }
    }};
}

#[macro_export]
macro_rules! allocate_and_init {
    ($ot:ident, $ty:ty) => {{
        let obj = $ot
            .allocate(mem_size_of::<$ty>(), None)
            .expect("failed to allocate object");
        let inner_obj = obj.allocate::<$ty>().unwrap();
        obj.set_inner_allocator(inner_obj);

        #[cfg(debug_assertions)]
        println!(
            "[DEBUG] just allocated {} with type {}",
            obj.id,
            std::any::type_name::<$ty>()
        );

        obj
    }};
    ($ot:ident, $ty:ty, $id:expr) => {{
        let obj = $ot
            .allocate(mem_size_of::<$ty>(), $id)
            .expect("failed to allocate object with id {:?}", $id);
        let inner_obj = obj.allocate::<$ty>().unwrap();
        obj.set_inner_allocator(inner_obj);

        #[cfg(debug_assertions)]
        println!(
            "[DEBUG] just allocated {} (supplied id) with type {}",
            obj.id,
            std::any::type_name::<$ty>()
        );
        obj
    }};
}

#[macro_export]
macro_rules! iptr_of_ref {
    ($re:expr) => {{
        object_lib_tls::iptr_of(unit_ptr_of!($re)).unwrap()
    }};
}

#[macro_export]
macro_rules! register_initial {
    ($obj:ident, $obj_tracker:ident) => {{
        let _ = $obj.bump_version();
        if $obj.object_is_mv_enabled() {
            $obj_tracker.push_initial_version($obj.id, (&*$obj).into());
        }
        ownership_tracker_tls::mark_object_owned($obj.id);
    }};
}
