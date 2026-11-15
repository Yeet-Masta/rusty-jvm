use super::*;
use crate::vm::heap::construction::InstancePlan;
use crate::vm::method_area::loaded_classes::CLASSES;

impl JavaClass {
    /// Inject VM fields while the class is exclusively owned, before Arc publication
    /// or any hierarchy/layout cache can exist.
    pub(super) fn add_vm_fields(&mut self) -> Result<()> {
        let (name, descriptor) = match self.this_class_name.as_str() {
            "java/lang/invoke/ResolvedMethodName" => ("vmtarget", TypeDescriptor::Long),
            "jdk/internal/reflect/ConstantPool" => {
                let name = crate::vm::system_native::constant_pool::CONSTANT_POOL_OOP;
                if self.instance_fields_template.contains_key(name) {
                    return Ok(());
                }
                (name, TypeDescriptor::Object("java/lang/Object".to_string()))
            }
            namespaces::CLASS_LOADER => (namespaces::LOADER_SERIAL_FIELD, TypeDescriptor::Integer),
            _ => return Ok(()),
        };
        if self.fields_info.contains_key(name) {
            return Err(Error::new_execution(&format!(
                "Synthetic field {}.{name} already exists",
                self.this_class_name
            )));
        }
        self.instance_fields_template
            .insert(name.to_string(), FieldValue::new(descriptor.clone())?);
        self.fields_info.insert(
            name.to_string(),
            Arc::new(FieldInfo::new(
                descriptor,
                0,
                self.this_class_name.clone(),
                name.to_string(),
            )),
        );
        Ok(())
    }

    /// All fallible hierarchy/schema work precedes instance publication. Build outside
    /// the OnceCell initializer lock, since hierarchy resolution can execute Java.
    pub(crate) fn new_instance(&self, klass_id: usize) -> Result<InstancePlan> {
        let fields = self.instance_fields_hierarchy()?;
        if self.instance_layout.get().is_none() {
            let mut segments = Vec::new();
            for class_name in fields.keys() {
                let class = CLASSES.get_loaded(class_name).ok_or_else(|| {
                    Error::new_execution("Materialized hierarchy has no published class")
                })?;
                let metadata = class
                    .instance_fields_template
                    .keys()
                    .map(|name| {
                        class.fields_info.get(name).cloned().ok_or_else(|| {
                            Error::new_execution("Missing field descriptor for instance layout")
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                segments.push((class_name.clone(), metadata));
            }
            let layout = InstanceLayout::new(segments)?;
            layout.validate(fields)?;
            // Another builder may win; either value describes the same fixed metadata.
            let _ = self.instance_layout.set(Arc::new(layout));
        }
        let layout = self
            .instance_layout
            .get()
            .ok_or_else(|| Error::new_execution("Instance layout was not published"))?;
        InstancePlan::new(klass_id, fields, Arc::clone(layout))
    }

    /// A published class may not have a mirror yet. Never claim publication, wait,
    /// or initialize the cell from a root scan.
    pub(crate) fn mirror_ref_if_present(&self) -> Option<i32> {
        self.mirror_clazz_ref.get().copied()
    }

    #[cfg(test)]
    pub(crate) fn root_refs(&self) -> Result<Vec<i32>> {
        let mut roots = Vec::new();
        self.extend_root_refs(&mut roots)?;
        Ok(roots)
    }

    /// Appends this class's roots to `roots`, so a scan over every class builds one vector.
    pub(crate) fn extend_root_refs(&self, roots: &mut Vec<i32>) -> Result<()> {
        roots.extend(self.mirror_ref_if_present());
        for value in &self.static_references {
            roots.push(value.reference_snapshot()?);
        }
        roots.extend(
            self.fields_info
                .values()
                .filter_map(|field| field.reflection_ref_if_present()),
        );
        roots.extend(
            self.methods
                .values()
                .filter_map(|method| method.reflection_ref_if_present()),
        );
        self.invoke_dynamic_runner.extend_resolved_refs(roots);
        roots.extend(self.static_fields_init_state.failure_root()?);
        Ok(())
    }
}

impl InitState {
    pub(crate) fn failure_root(&self) -> Result<Option<i32>> {
        // A Safe thread can be reacquiring this lock. Never use safepoint::lock
        // from the collector, which is already Safe and must not park again.
        let state = self.state.try_lock().ok_or_else(|| {
            Error::new_execution("Class initialization state locked during root snapshot")
        })?;
        Ok(match *state {
            InnerState::Failed { cause_root } => Some(cause_root),
            InnerState::Uninitialized
            | InnerState::Initializing { .. }
            | InnerState::Initialized => None,
        })
    }
}

#[cfg(test)]
#[path = "java_class_gc_tests.rs"]
mod tests;
