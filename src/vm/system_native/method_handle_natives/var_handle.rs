//! Signature-polymorphic `VarHandle` access modes (`get`, `setRelease`, `getAndAdd`, ...).
//!
//! Every direct `VarHandle` implementation in `java.lang.invoke` (`VarHandleInts$FieldInstanceReadWrite`,
//! `VarHandleReferences$Array`, `VarHandleByteArrayAsLongs$ArrayHandle`, ...) declares one static
//! method per supported access mode, named after the mode and taking the handle followed by the
//! coordinates and values: `static int getAndAdd(VarHandle ob, Object holder, int value)`. This is
//! the method HotSpot links through `VarForm.getMemberName`.
//!
//! When the call site's types match that method slot for slot, it is invoked directly. Otherwise
//! (an indirect or lazily initialized handle, or a call site that needs boxing, widening or a
//! discarded result) the call goes through `toMethodHandle(mode).asType(callSiteType)`, which
//! performs the same conversions `VarHandle` does in HotSpot.

use crate::vm::error::{Error, Result};
use crate::vm::exception::pending_helpers::java_exception;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::invoker::{invoke, SignatureOrigin};
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::method_area::java_method::JavaMethod;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::method_area::lookup::lookup_instance_method;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_value::StackValueKind;
use crate::vm::system_native::method_handle_natives::invocation::invoke_with_call_site_type;
use dashmap::DashMap;
use jdescriptor::{MethodDescriptor, TypeDescriptor};
use once_cell::sync::Lazy;
use std::sync::Arc;

const VAR_HANDLE: &str = "java/lang/invoke/VarHandle";
const VAR_HANDLE_DESCRIPTOR: &str = "Ljava/lang/invoke/VarHandle;";

/// `(implementation class, access mode)` -> the static method implementing it, or `None` when the
/// class has none (indirect handles, unsupported modes).
static ACCESS_METHODS: Lazy<DashMap<(String, String), Option<Arc<JavaMethod>>>> =
    Lazy::new(DashMap::new);

/// Every access mode of `java.lang.invoke.VarHandle.AccessMode`, by method name.
pub(crate) fn is_access_mode(method_name: &str) -> bool {
    matches!(
        method_name,
        "get"
            | "set"
            | "getVolatile"
            | "setVolatile"
            | "getAcquire"
            | "setRelease"
            | "getOpaque"
            | "setOpaque"
            | "compareAndSet"
            | "compareAndExchange"
            | "compareAndExchangeAcquire"
            | "compareAndExchangeRelease"
            | "weakCompareAndSetPlain"
            | "weakCompareAndSet"
            | "weakCompareAndSetAcquire"
            | "weakCompareAndSetRelease"
            | "getAndSet"
            | "getAndSetAcquire"
            | "getAndSetRelease"
            | "getAndAdd"
            | "getAndAddAcquire"
            | "getAndAddRelease"
            | "getAndBitwiseOr"
            | "getAndBitwiseOrRelease"
            | "getAndBitwiseOrAcquire"
            | "getAndBitwiseAnd"
            | "getAndBitwiseAndRelease"
            | "getAndBitwiseAndAcquire"
            | "getAndBitwiseXor"
            | "getAndBitwiseXorRelease"
            | "getAndBitwiseXorAcquire"
    )
}

/// Invokes access mode `mode` of the `VarHandle` in `args[0]` with the remaining `args`, typed by
/// `call_site` (which does not include the handle itself).
///
/// Returns the result slots when the VM computed them directly, or an empty destination when a
/// Java frame was pushed that will return the result to the caller itself.
pub(crate) fn invoke_access_mode(
    mode: &str,
    args: &[Slot],
    call_site: &MethodDescriptor,
) -> Result<RootedSlots> {
    let handle = args
        .first()
        .filter(|slot| slot.is_ref())
        .map(|slot| slot.value())
        .ok_or_else(|| Error::new_execution("VarHandle access mode invoked without a handle"))?;
    if handle == 0 {
        return Err(java_exception(
            "java/lang/NullPointerException",
            &format!(
                "Cannot invoke \"java.lang.invoke.VarHandle.{mode}\" because the handle is null"
            ),
        ));
    }
    let class_name = HEAP.get_instance_name(handle)?;

    if let Some(method) = access_method(&class_name, mode)? {
        match fast_path(method.get_method_descriptor(), call_site) {
            FastPath::Exact => {
                let class_name = method.class_name().to_owned();
                JavaThread::with_top_frames_mut(|stack_frames| {
                    invoke(
                        stack_frames,
                        method.name_signature(),
                        SignatureOrigin::EffectiveMethodType(method.get_method_descriptor()),
                        args,
                        Arc::clone(&method),
                        &class_name,
                    )
                })??;
                return Ok(RootedSlots::new());
            }
            FastPath::DiscardResult => {
                let values = args
                    .iter()
                    .map(|&slot| slot.into())
                    .collect::<Vec<StackValueKind>>();
                Executor::invoke_static_method(
                    method.class_name(),
                    method.name_signature(),
                    &values,
                )?;
                return Ok(RootedSlots::new());
            }
            FastPath::No => {}
        }
    }

    via_method_handle(handle, &class_name, mode, &args[1..], call_site)
}

/// Looks up the static method implementing `mode` on `class_name` or its superclasses.
fn access_method(class_name: &str, mode: &str) -> Result<Option<Arc<JavaMethod>>> {
    let key = (class_name.to_owned(), mode.to_owned());
    if let Some(found) = ACCESS_METHODS.get(&key) {
        return Ok(found.clone());
    }
    let mut current = Some(class_name.to_owned());
    let mut found = None;
    while let Some(name) = current {
        let class = CLASSES.get(&name)?;
        found = class
            .get_methods()
            .map(|(_, method)| method)
            .find(|method| {
                method.is_static()
                    && method.name() == mode
                    && method
                        .get_method_descriptor()
                        .parameter_types()
                        .first()
                        .is_some_and(|first| first.to_string() == VAR_HANDLE_DESCRIPTOR)
            })
            .cloned();
        if found.is_some() || name == VAR_HANDLE {
            break;
        }
        current = class.parent().clone();
    }
    ACCESS_METHODS.insert(key, found.clone());
    Ok(found)
}

#[derive(Debug, PartialEq)]
enum FastPath {
    /// Slots and result line up; push the implementation's frame.
    Exact,
    /// Arguments line up and the call site returns `void`; run it and drop the result.
    DiscardResult,
    /// A conversion is needed.
    No,
}

fn fast_path(target: &MethodDescriptor, call_site: &MethodDescriptor) -> FastPath {
    let target_params = &target.parameter_types()[1..];
    let call_params = call_site.parameter_types();
    if target_params.len() != call_params.len()
        || !target_params
            .iter()
            .zip(call_params)
            .all(|(target, call)| slot_compatible(target, call))
    {
        return FastPath::No;
    }
    let (target_ret, call_ret) = (target.return_type(), call_site.return_type());
    if slot_compatible(target_ret, call_ret) {
        FastPath::Exact
    } else if *call_ret == TypeDescriptor::Void {
        FastPath::DiscardResult
    } else {
        FastPath::No
    }
}

/// Whether a value typed `call` at the call site can be passed or returned unchanged as `target`.
/// References pass through: the implementation methods check casts themselves.
fn slot_compatible(target: &TypeDescriptor, call: &TypeDescriptor) -> bool {
    let is_ref =
        |t: &TypeDescriptor| matches!(t, TypeDescriptor::Object(_) | TypeDescriptor::Array(..));
    (is_ref(target) && is_ref(call)) || target == call
}

/// `handle.toMethodHandle(AccessMode.valueFromMethodName(mode)).asType(callSite).invokeExact(args)`.
fn via_method_handle(
    handle: i32,
    class_name: &str,
    mode: &str,
    args: &[Slot],
    call_site: &MethodDescriptor,
) -> Result<RootedSlots> {
    let mut roots = JavaThread::hold_temp_roots(args.to_vec());
    roots.add_root(Slot::Ref(handle));

    let mode_name = StringPoolHelper::get_string(mode)?;
    let access_mode = Executor::invoke_static_method(
        "java/lang/invoke/VarHandle$AccessMode",
        "valueFromMethodName:(Ljava/lang/String;)Ljava/lang/invoke/VarHandle$AccessMode;",
        &[StackValueKind::Ref(mode_name)],
    )?;
    let to_method_handle =
        "toMethodHandle:(Ljava/lang/invoke/VarHandle$AccessMode;)Ljava/lang/invoke/MethodHandle;";
    let method = lookup_instance_method(class_name, to_method_handle)?
        .ok_or_else(|| Error::new_execution(&format!("{class_name} has no {to_method_handle}")))?;
    let method_handle = Executor::invoke_non_static_method(
        method.class_name(),
        to_method_handle,
        handle,
        &[StackValueKind::Ref(access_mode[0].value())],
    )?;
    let method_handle = method_handle
        .first()
        .map(|slot| slot.value())
        .ok_or_else(|| Error::new_execution("VarHandle.toMethodHandle returned nothing"))?;
    roots.add_root(Slot::Ref(method_handle));
    invoke_with_call_site_type(method_handle, args, call_site)?;
    Ok(RootedSlots::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(descriptor: &str) -> MethodDescriptor {
        descriptor.parse().unwrap()
    }

    #[test]
    fn exact_match_uses_the_implementation_frame() {
        let target = md("(Ljava/lang/invoke/VarHandle;Ljava/lang/Object;I)I");
        assert_eq!(fast_path(&target, &md("(LFoo;I)I")), FastPath::Exact);
    }

    #[test]
    fn statement_call_discards_the_result() {
        let target = md("(Ljava/lang/invoke/VarHandle;Ljava/lang/Object;I)I");
        assert_eq!(
            fast_path(&target, &md("(LFoo;I)V")),
            FastPath::DiscardResult
        );
    }

    #[test]
    fn boxing_or_widening_goes_through_as_type() {
        let target = md("(Ljava/lang/invoke/VarHandle;Ljava/lang/Object;)I");
        assert_eq!(
            fast_path(&target, &md("(LFoo;)Ljava/lang/Object;")),
            FastPath::No
        );
        assert_eq!(fast_path(&target, &md("(LFoo;)J")), FastPath::No);
        let setter = md("(Ljava/lang/invoke/VarHandle;Ljava/lang/Object;J)V");
        assert_eq!(fast_path(&setter, &md("(LFoo;I)V")), FastPath::No);
    }

    #[test]
    fn every_access_mode_is_recognised() {
        for mode in ["get", "setRelease", "getAndAdd", "getAndBitwiseXorAcquire"] {
            assert!(is_access_mode(mode));
        }
        assert!(!is_access_mode("toMethodHandle"));
    }
}
