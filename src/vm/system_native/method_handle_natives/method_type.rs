use crate::vm::commons::fast_hash::FastBuildHasher;
use crate::vm::error::Result;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::{decorate, klass};
use dashmap::DashMap;
use getset::Getters;
use jdescriptor::MethodDescriptor;
use std::sync::{Arc, LazyLock};

const METHOD_TYPE: &str = "java/lang/invoke/MethodType";

/// The most `MethodType` objects remembered; past this the cache starts over.
const CACHE_LIMIT: usize = 4096;

/// Decoded `MethodType` objects by reference.
///
/// Every method-handle invocation decodes its handle's type, which walks the parameter classes and
/// parses a descriptor. A `MethodType`'s `ptypes` and `rtype` never change once it is built, and a
/// heap reference is never issued twice, so a reference identifies one decoded type for good.
static CACHE: LazyLock<DashMap<i32, Arc<MethodType>, FastBuildHasher>> =
    LazyLock::new(DashMap::default);

#[derive(Debug, Getters)]
#[get = "pub"]
pub struct MethodType {
    ptype_names: String,
    rtype_name: String,
    #[getset(skip)]
    descriptor: MethodDescriptor,
}

impl MethodType {
    pub(crate) fn descriptor(&self) -> Result<MethodDescriptor> {
        Ok(self.descriptor.clone())
    }

    /// The parsed descriptor, without copying it.
    pub(crate) fn descriptor_ref(&self) -> &MethodDescriptor {
        &self.descriptor
    }

    pub fn new(type_obj_ref: i32) -> Result<Arc<Self>> {
        if let Some(cached) = CACHE.get(&type_obj_ref) {
            return Ok(Arc::clone(&cached));
        }
        let ptype_class_refs =
            HEAP.get_object_field_value(type_obj_ref, METHOD_TYPE, "ptypes")?[0];
        let rtype_class_ref = HEAP.get_object_field_value(type_obj_ref, METHOD_TYPE, "rtype")?[0];

        let ptype_names = generate_parameters(ptype_class_refs)?;
        let rtype_name = generate_return_type(rtype_class_ref)?;
        let descriptor = format!("{ptype_names}{rtype_name}").parse()?;

        let method_type = Arc::new(Self {
            ptype_names,
            rtype_name,
            descriptor,
        });
        if CACHE.len() >= CACHE_LIMIT {
            CACHE.clear();
        }
        CACHE.insert(type_obj_ref, Arc::clone(&method_type));
        Ok(method_type)
    }
}

fn generate_parameters(ptype_class_refs: i32) -> Result<String> {
    let len = HEAP.get_array_len(ptype_class_refs)?;
    let mut ptype_names = Vec::with_capacity(len as usize);
    for i in 0..len {
        let ptype_class_ref = HEAP.get_array_value(ptype_class_refs, i)?[0];
        let ptype_name = klass(ptype_class_ref)?.this_class_name().to_owned();
        ptype_names.push(decorate(ptype_name));
    }
    let ptype_names_string = format!("({})", ptype_names.join(""));
    Ok(ptype_names_string)
}

fn generate_return_type(rtype_class_ref: i32) -> Result<String> {
    let rtype_name = klass(rtype_class_ref)?.this_class_name().to_owned();
    Ok(decorate(rtype_name))
}
