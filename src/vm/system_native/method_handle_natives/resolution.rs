use crate::vm::error::{Error, Result};
use crate::vm::exception::pending_helpers::java_exception;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::klass;
use crate::vm::method_area::java_class::JavaClass;
use crate::vm::method_area::java_method::JavaMethod;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::system_native::method_handle_natives::member_name::{
    set_reference_kind, MemberName,
};
use crate::vm::system_native::method_handle_natives::method_type::MethodType;
use crate::vm::system_native::method_handle_natives::resolved_method_name::ResolvedMethodName;
use crate::vm::system_native::method_handle_natives::types::ReferenceKind;
use crate::vm::system_native::method_handle_natives::types::ReferenceKind::*;
use jclassmodel::modifiers::MethodModifier;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

pub fn resolve(
    member_name_ref: i32,
    _caller_class_ref: i32,
    _lookup_mode: i32,
    speculative_resolve: bool,
) -> Result<i32> {
    let mut member_name = MemberName::new(member_name_ref)?;
    let reference_kind = member_name.reference_kind();
    let resolved = match reference_kind {
        REF_invokeVirtual | REF_invokeStatic | REF_invokeSpecial | REF_newInvokeSpecial
        | REF_invokeInterface => resolve_method(&mut member_name)?,
        REF_getField | REF_putField | REF_getStatic | REF_putStatic => {
            resolve_field(&mut member_name)?
        }
    };
    if resolved {
        return Ok(member_name.member_name_ref());
    }
    if speculative_resolve {
        return Ok(0);
    }
    let (error_class, kind) = match reference_kind {
        REF_getField | REF_putField | REF_getStatic | REF_putStatic => {
            ("java/lang/NoSuchFieldError", "field")
        }
        _ => ("java/lang/NoSuchMethodError", "method"),
    };
    Err(java_exception(
        error_class,
        &format!(
            "no such {kind}: {}.{}",
            member_name.class_name().replace('/', "."),
            member_name.name()
        ),
    ))
}

/// The class that declares `signature` as seen from `class_name`, following JVMS 5.4.3.3/5.4.3.4:
/// the class itself, then its superclasses, then its superinterfaces (breadth first).
fn find_method_holder(
    class_name: &str,
    signature: &str,
) -> Result<Option<(Arc<JavaClass>, usize, Arc<JavaMethod>)>> {
    let mut current = Some(class_name.to_owned());
    let mut interfaces = VecDeque::new();
    while let Some(name) = current {
        let class = CLASSES.get(&name)?;
        if let Some((index, method)) = class.get_method_full(signature) {
            return Ok(Some((class, index, method)));
        }
        interfaces.extend(class.interfaces().iter().cloned());
        current = class.parent().clone();
    }
    let mut seen = HashSet::new();
    while let Some(name) = interfaces.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let class = CLASSES.get(&name)?;
        if let Some((index, method)) = class.get_method_full(signature) {
            if !method.is_static() {
                return Ok(Some((class, index, method)));
            }
        }
        interfaces.extend(class.interfaces().iter().cloned());
    }
    Ok(None)
}

/// Sets MemberName.method instance of ResolvedMethodName with injected `long vmtarget` (which is a pointer to the target method's vtable (Method* in HotSpot)).
/// Also sets ResolvedMethodName.vmholder - the class that declares the resolved method.
fn resolve_method(member_name: &mut MemberName) -> Result<bool> {
    let type_obj_ref = member_name.type_obj_ref();
    let method_type = MethodType::new(type_obj_ref)?;
    let method_name = member_name.name();
    let ptype_names = method_type.ptype_names();
    let rtype_name = method_type.rtype_name();
    let full_method_signature = format!("{method_name}:{ptype_names}{rtype_name}");

    let Some((holder, method_index, method)) =
        find_method_holder(member_name.class_name(), &full_method_signature)?
    else {
        return Ok(false);
    };

    let current_flags = member_name.flags();
    let flags_to_enrich_with = method.access_flags();
    let enriched_flags = current_flags | flags_to_enrich_with;
    member_name.propagate_flags(enriched_flags)?;

    let reflection_ref = holder.mirror_clazz_ref()?;
    // Constructors and `invokespecial` keep the class they were looked up in: a constructor is
    // never inherited, and a special call's holder must stay the caller-visible class.
    if !matches!(
        member_name.reference_kind(),
        REF_newInvokeSpecial | REF_invokeSpecial
    ) {
        member_name.propagate_clazz(holder.this_class_name(), reflection_ref)?;
    }

    let resolved_method_name =
        ResolvedMethodName::new_create_instance(reflection_ref, method_index as i64)?;
    member_name.propagate_method(resolved_method_name)?;

    Ok(true)
}

/// Resolves a field through the class, its superinterfaces and superclasses (JVMS 5.4.3.2) and
/// records the declaring class.
fn resolve_field(member_name: &mut MemberName) -> Result<bool> {
    let field_name = member_name.name().to_owned();
    let want_static = matches!(member_name.reference_kind(), REF_getStatic | REF_putStatic);
    let mut pending = VecDeque::from([member_name.class_name().to_owned()]);
    let mut seen = HashSet::new();
    while let Some(name) = pending.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let klass = CLASSES.get(&name)?;
        if let Some(field_info) = klass.field_info(&field_name) {
            let is_static = field_info.flags() & 0x0008 != 0;
            if is_static == want_static {
                let enriched_flags = member_name.flags() | field_info.flags() as i32;
                member_name.propagate_flags(enriched_flags)?;
                if want_static {
                    member_name
                        .propagate_clazz(klass.this_class_name(), klass.mirror_clazz_ref()?)?;
                }
                return Ok(true);
            }
        }
        pending.extend(klass.interfaces().iter().cloned());
        if let Some(parent) = klass.parent() {
            pending.push_back(parent.clone());
        }
    }
    Ok(false)
}

/// Resolves MemberName instance based on `obj_ref` which is expected to be a `java/lang/reflect/Method`, `java/lang/reflect/Constructor`, or `java/lang/reflect/Field`.
/// It initializes the `vmtarget` and `vmindex` fields of the MemberName
pub(crate) fn member_name_init(member_name_ref: i32, obj_ref: i32) -> Result<()> {
    // MethodHandleNatives.Constants
    const MN_IS_METHOD: i32 = 0x00010000; // method (not constructor)
    const MN_IS_CONSTRUCTOR: i32 = 0x00020000; // constructor

    let obj_name = HEAP.get_instance_name(obj_ref)?;

    match obj_name.as_str() {
        "java/lang/reflect/Method" => {
            // fill in vmtarget, vmindex while we have Method ref in hand:
            let modifiers =
                HEAP.get_object_field_value(obj_ref, obj_name.as_str(), "modifiers")?[0];
            let method_flags = MethodModifier::from_bits_truncate(modifiers as u16);
            // Mirrors HotSpot's MethodHandles::init_method_MemberName with dispatch enabled.
            let kind = if method_flags.contains(MethodModifier::Static) {
                REF_invokeStatic
            } else {
                let clazz_ref =
                    HEAP.get_object_field_value(obj_ref, obj_name.as_str(), "clazz")?[0];
                let declaring = klass(clazz_ref)?;
                let statically_bound = method_flags.contains(MethodModifier::Private)
                    || method_flags.contains(MethodModifier::Final)
                    || declaring
                        .class_modifiers()
                        .contains(jclassmodel::modifiers::ClassModifier::Final);
                if declaring.is_interface() {
                    if method_flags.contains(MethodModifier::Private) {
                        REF_invokeSpecial
                    } else {
                        REF_invokeInterface
                    }
                } else if statically_bound {
                    REF_invokeSpecial
                } else {
                    REF_invokeVirtual
                }
            };
            init_from_method_or_constructor(member_name_ref, obj_ref, kind, MN_IS_METHOD)?;
        }
        "java/lang/reflect/Constructor" => {
            // fill in vmtarget, vmindex while we have Constructor ref in hand:
            init_from_method_or_constructor(
                member_name_ref,
                obj_ref,
                REF_newInvokeSpecial,
                MN_IS_CONSTRUCTOR,
            )?;
        }
        "java/lang/reflect/Field" => {
            // Mirrors HotSpot's MethodHandles::init_field_MemberName for a getter: Java's
            // MemberName(Field, boolean) sets name and type itself and flips to a setter kind.
            const MN_IS_FIELD: i32 = 0x00040000;
            let clazz_ref = HEAP.get_object_field_value(obj_ref, obj_name.as_str(), "clazz")?[0];
            let modifiers =
                HEAP.get_object_field_value(obj_ref, obj_name.as_str(), "modifiers")?[0];
            const ACC_STATIC: i32 = 0x0008;
            let kind = if modifiers & ACC_STATIC != 0 {
                REF_getStatic
            } else {
                REF_getField
            };
            let flags = set_reference_kind(modifiers, kind) | MN_IS_FIELD;
            HEAP.set_object_field_value(
                member_name_ref,
                "java/lang/invoke/MemberName",
                "clazz",
                vec![clazz_ref],
            )?;
            HEAP.set_object_field_value(
                member_name_ref,
                "java/lang/invoke/MemberName",
                "flags",
                vec![flags],
            )?;
        }
        _ => {
            return Err(Error::new_execution(&format!(
                "method_handle_natives_init_wrp: Unsupported object type: {obj_name}"
            )));
        }
    }

    Ok(())
}

fn init_from_method_or_constructor(
    member_name_ref: i32,
    obj_ref: i32,
    kind: ReferenceKind,
    internal_flag: i32,
) -> Result<()> {
    let obj_name = HEAP.get_instance_name(obj_ref)?;
    let clazz_ref = HEAP.get_object_field_value(obj_ref, obj_name.as_str(), "clazz")?[0];
    let modifiers = HEAP.get_object_field_value(obj_ref, obj_name.as_str(), "modifiers")?[0];
    let slot = HEAP.get_object_field_value(obj_ref, obj_name.as_str(), "slot")?[0];

    let resolved_method_name = ResolvedMethodName::new_create_instance(clazz_ref, slot as i64)?;
    resolved_method_name.propagate_all()?;
    let method_ref = resolved_method_name.resolved_method_name_ref();

    let enriched_with_kind = set_reference_kind(modifiers, kind);
    let enriched_with_internal_flags = enriched_with_kind | internal_flag;

    HEAP.set_object_field_value(
        member_name_ref,
        "java/lang/invoke/MemberName",
        "clazz",
        vec![clazz_ref],
    )?;
    HEAP.set_object_field_value(
        member_name_ref,
        "java/lang/invoke/MemberName",
        "flags",
        vec![enriched_with_internal_flags],
    )?;
    HEAP.set_object_field_value(
        member_name_ref,
        "java/lang/invoke/MemberName",
        "method",
        vec![method_ref],
    )?;

    Ok(())
}
