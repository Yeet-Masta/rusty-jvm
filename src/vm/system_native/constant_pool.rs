use crate::vm::error::{Error, Result};
use crate::vm::exception::pending_helpers::java_exception;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::klass;
use crate::vm::method_area::java_class::JavaClass;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::stack::rooted_slots::RootedSlots;
use jclassmodel::constant_pool::ConstantPoolLookup;
use std::sync::Arc;

/// JDK 25 passes the constant pool holder to every `ConstantPool.*0` native explicitly, while JDK 26+
/// drops that parameter and the natives read it from the (VM-injected) `constantPoolOop` field.
pub(crate) const CONSTANT_POOL_OOP: &str = "constantPoolOop";

/// `jdk.internal.reflect.ConstantPool.getUTF8At0(I)Ljava/lang/String;` (JDK 26+)
pub(crate) fn get_utf8_at0(this: i32, index: i32) -> Result<i32> {
    get_utf8_at0_with_oop(this, constant_pool_oop(this)?, index)
}

/// `jdk.internal.reflect.ConstantPool.getSize0()I` (JDK 26+)
pub(crate) fn get_size0(this: i32) -> Result<i32> {
    get_size0_with_oop(this, constant_pool_oop(this)?)
}

/// `jdk.internal.reflect.ConstantPool.getTagAt0(I)B` (JDK 26+)
pub(crate) fn get_tag_at0(this: i32, index: i32) -> Result<i8> {
    get_tag_at0_with_oop(this, constant_pool_oop(this)?, index)
}

/// `jdk.internal.reflect.ConstantPool.getUTF8At0(Ljava/lang/Object;I)Ljava/lang/String;` (JDK 25)
pub(crate) fn get_utf8_at0_with_oop(_this: i32, oop_ref: i32, index: i32) -> Result<i32> {
    let klass = extract_java_class(oop_ref)?;
    let constant_pool = klass.constant_pool();
    let utf8 = constant_pool.get_utf8(index as u16).ok_or_else(|| {
        Error::new_constant_pool(&format!(
            "error getting utf8 by cpool index={index} in {}",
            klass.this_class_name()
        ))
    })?;
    let string_ref = StringPoolHelper::get_string(&utf8)?;

    Ok(string_ref)
}

/// `jdk.internal.reflect.ConstantPool.getSize0(Ljava/lang/Object;)I` (JDK 25)
pub(crate) fn get_size0_with_oop(_this: i32, oop_ref: i32) -> Result<i32> {
    let klass = extract_java_class(oop_ref)?;
    Ok(klass.constant_pool().len() as i32)
}

/// `jdk.internal.reflect.ConstantPool.getTagAt0(Ljava/lang/Object;I)B` (JDK 25)
pub(crate) fn get_tag_at0_with_oop(_this: i32, oop_ref: i32, index: i32) -> Result<i8> {
    let klass = extract_java_class(oop_ref)?;

    let tag = klass
        .constant_pool()
        .tag_at(index as usize)
        .ok_or_else(|| {
            Error::new_constant_pool(&format!(
                "error getting tag by cpool index={index} in {}",
                klass.this_class_name()
            ))
        })?;

    Ok(tag.as_u8() as i8)
}

fn cp_error(klass: &JavaClass, what: &str, index: i32) -> Error {
    // HotSpot throws IllegalArgumentException("Wrong type at constant pool index").
    let _ = klass;
    java_exception(
        "java/lang/IllegalArgumentException",
        &format!("Wrong type at constant pool index {index} (expected {what})"),
    )
}

/// `jdk.internal.reflect.ConstantPool.getIntAt0(Ljava/lang/Object;I)I` (JDK 25)
pub(crate) fn get_int_at0(_this: i32, oop_ref: i32, index: i32) -> Result<i32> {
    let klass = extract_java_class(oop_ref)?;
    let value = klass.constant_pool().get_integer(index as u16);
    value.ok_or_else(|| cp_error(&klass, "Integer", index))
}

/// `jdk.internal.reflect.ConstantPool.getLongAt0(Ljava/lang/Object;I)J` (JDK 25)
pub(crate) fn get_long_at0(_this: i32, oop_ref: i32, index: i32) -> Result<i64> {
    let klass = extract_java_class(oop_ref)?;
    let value = klass.constant_pool().get_long(index as u16);
    value.ok_or_else(|| cp_error(&klass, "Long", index))
}

/// `jdk.internal.reflect.ConstantPool.getFloatAt0(Ljava/lang/Object;I)F` (JDK 25)
pub(crate) fn get_float_at0(_this: i32, oop_ref: i32, index: i32) -> Result<f32> {
    let klass = extract_java_class(oop_ref)?;
    let value = klass.constant_pool().get_float(index as u16);
    value.ok_or_else(|| cp_error(&klass, "Float", index))
}

/// `jdk.internal.reflect.ConstantPool.getDoubleAt0(Ljava/lang/Object;I)D` (JDK 25)
pub(crate) fn get_double_at0(_this: i32, oop_ref: i32, index: i32) -> Result<f64> {
    let klass = extract_java_class(oop_ref)?;
    let value = klass.constant_pool().get_double(index as u16);
    value.ok_or_else(|| cp_error(&klass, "Double", index))
}

/// `jdk.internal.reflect.ConstantPool.getStringAt0(Ljava/lang/Object;I)Ljava/lang/String;` (JDK 25)
pub(crate) fn get_string_at0(_this: i32, oop_ref: i32, index: i32) -> Result<i32> {
    let klass = extract_java_class(oop_ref)?;
    let value = klass.constant_pool().get_string(index as u16);
    let value = value.ok_or_else(|| cp_error(&klass, "String", index))?;
    StringPoolHelper::get_string(&value)
}

/// `jdk.internal.reflect.ConstantPool.getClassAt0(Ljava/lang/Object;I)Ljava/lang/Class;` (JDK 25)
pub(crate) fn get_class_at0(_this: i32, oop_ref: i32, index: i32) -> Result<i32> {
    let klass = extract_java_class(oop_ref)?;
    let name = klass.constant_pool().get_class_name(index as u16);
    let name = name.ok_or_else(|| cp_error(&klass, "Class", index))?;
    CLASSES.class_mirror_ref(&name)
}

/// `jdk.internal.reflect.ConstantPool.getClassAtIfLoaded0(Ljava/lang/Object;I)Ljava/lang/Class;` (JDK 25)
pub(crate) fn get_class_at_if_loaded0(_this: i32, oop_ref: i32, index: i32) -> Result<i32> {
    let klass = extract_java_class(oop_ref)?;
    let name = klass.constant_pool().get_class_name(index as u16);
    let name = name.ok_or_else(|| cp_error(&klass, "Class", index))?;
    if CLASSES.is_loaded(&name) {
        CLASSES.class_mirror_ref(&name)
    } else {
        Ok(0)
    }
}

/// `jdk.internal.reflect.ConstantPool.getMemberRefInfoAt0(Ljava/lang/Object;I)[Ljava/lang/String;`
/// (JDK 25): `{class, name, descriptor}` of a field, method or interface method reference.
pub(crate) fn get_member_ref_info_at0(_this: i32, oop_ref: i32, index: i32) -> Result<i32> {
    let klass = extract_java_class(oop_ref)?;
    let cp = klass.constant_pool();
    let (class, name, descriptor) = cp
        .get_full_field_info(index as u16)
        .or_else(|| cp.get_full_method_info(index as u16))
        .or_else(|| cp.get_full_interfacemethodref_info(index as u16))
        .ok_or_else(|| cp_error(&klass, "Fieldref or Methodref", index))?;
    string_array(&[class, name, descriptor])
}

/// `jdk.internal.reflect.ConstantPool.getNameAndTypeRefInfoAt0(Ljava/lang/Object;I)[Ljava/lang/String;`
/// (JDK 25): `{name, descriptor}`.
pub(crate) fn get_name_and_type_ref_info_at0(_this: i32, oop_ref: i32, index: i32) -> Result<i32> {
    let klass = extract_java_class(oop_ref)?;
    let (name, descriptor) = klass
        .constant_pool()
        .get_name_and_type(index as u16)
        .ok_or_else(|| cp_error(&klass, "NameAndType", index))?;
    string_array(&[name, descriptor])
}

fn string_array(values: &[String]) -> Result<i32> {
    // Interned strings stay reachable from the string table while the array is built.
    let refs = values
        .iter()
        .map(|value| StringPoolHelper::get_string(value))
        .collect::<Result<Vec<_>>>()?;
    let mut destination = RootedSlots::new();
    HEAP.create_array_with_values("[Ljava/lang/String;", &refs, &mut destination)
}

fn constant_pool_oop(this: i32) -> Result<i32> {
    Ok(HEAP.get_object_field_value(
        this,
        "jdk/internal/reflect/ConstantPool",
        CONSTANT_POOL_OOP,
    )?[0])
}

fn extract_java_class(constant_pool_oop_ref: i32) -> Result<Arc<JavaClass>> {
    let clazz_ref = constant_pool_oop_ref; // oop_ref is actually clazz_ref (so far)
    klass(clazz_ref)
}
