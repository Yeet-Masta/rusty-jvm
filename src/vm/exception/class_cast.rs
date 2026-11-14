use crate::vm::error::Result;
use crate::vm::exception::common::construct_exception_and_throw;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::{clazz_ref, undecorate};
use crate::vm::method_area::namespaces::binary_name;
use crate::vm::method_area::primitives_helper::PRIMITIVE_TYPE_BY_CODE;
use crate::vm::stack::stack_frame::StackFrames;
use crate::vm::stack::stack_value::StackValueKind;
use crate::vm::system_native::string::get_utf8_string_by_ref;
use tracing::warn;

/// Throws the `ClassCastException` for casting an instance of `from` to `to`, both given as
/// internal names (e.g. `java/lang/String` or `[I`), with the same message as HotSpot:
/// `class A cannot be cast to class B (A and B are in module m of loader 'l')`.
pub fn throw_class_cast_exception(
    from: &str,
    to: &str,
    stack_frames: &mut StackFrames,
) -> Result<()> {
    let from_name = binary_name(from).replace('/', ".");
    let to_name = binary_name(to).replace('/', ".");
    // The module details are only a description, so failing to look them up must not stop
    // the exception from being thrown
    let message = match class_cast_description(from, &from_name, to, &to_name) {
        Ok(description) => {
            format!("class {from_name} cannot be cast to class {to_name} ({description})")
        }
        Err(e) => {
            warn!("Failed to describe the modules of {from_name} and {to_name}: {e}");
            format!("class {from_name} cannot be cast to class {to_name}")
        }
    };
    let message = StringPoolHelper::create_string(&message)?;
    construct_exception_and_throw(
        "java/lang/ClassCastException",
        "<init>:(Ljava/lang/String;)V",
        &[StackValueKind::Ref(message[0].value())],
        stack_frames,
    )
}

/// HotSpot's `Klass::class_in_module_of_loader` for a published class: its loader's description
/// and e.g. `p.C is in unnamed module of loader p.L @1b6d3586, parent loader 'app'`.
pub(crate) fn class_in_module_of_loader(
    class: &crate::vm::method_area::java_class::JavaClass,
) -> Result<(String, String)> {
    let location = ClassLocation::of(class.this_class_name())?;
    let mut description = format!("{} is in {}", class.external_name(), location.describe());
    if location.loader_ref != 0
        && crate::vm::method_area::namespaces::namespace_of(location.loader_ref)? != 0
    {
        let parent_ref =
            HEAP.get_object_field_value(location.loader_ref, "java/lang/ClassLoader", "parent")?
                [0];
        description.push_str(", parent loader ");
        description.push_str(&loader_name(parent_ref)?);
    }
    Ok((location.loader_name, description))
}

/// E.g. `'bootstrap'`, `'app'` or `MyLoader @1b6d3586`.
fn loader_name(loader_ref: i32) -> Result<String> {
    if loader_ref == 0 {
        return Ok("'bootstrap'".to_string());
    }
    let name_and_id_ref =
        HEAP.get_object_field_value(loader_ref, "java/lang/ClassLoader", "nameAndId")?[0];
    get_utf8_string_by_ref(name_and_id_ref)
}

fn class_cast_description(from: &str, from_name: &str, to: &str, to_name: &str) -> Result<String> {
    let from_location = ClassLocation::of(from)?;
    let to_location = ClassLocation::of(to)?;

    Ok(if from_location.is_same_module(&to_location) {
        format!(
            "{from_name} and {to_name} are in {}",
            to_location.describe()
        )
    } else {
        format!(
            "{from_name} is in {}; {to_name} is in {}",
            from_location.describe(),
            to_location.describe()
        )
    })
}

/// The module and class loader of a class. An array has those of its innermost element type,
/// and primitives are in `java.base` with the bootstrap loader.
struct ClassLocation {
    module_ref: i32,
    /// `None` for an unnamed module
    module_name: Option<String>,
    loader_ref: i32,
    /// E.g. `'bootstrap'`, `'app'` or `'MyLoader' @1b6d3586`
    loader_name: String,
}

impl ClassLocation {
    fn of(class_name: &str) -> Result<Self> {
        let element = class_name.trim_start_matches('[');
        let owner = if PRIMITIVE_TYPE_BY_CODE.contains_key(element) {
            "java/lang/Object"
        } else {
            undecorate(element)
        };
        let mirror_ref = clazz_ref(owner)?;
        let module_ref = HEAP.get_object_field_value(mirror_ref, "java/lang/Class", "module")?[0];
        let loader_ref =
            HEAP.get_object_field_value(mirror_ref, "java/lang/Class", "classLoader")?[0];

        let module_name_ref = if module_ref == 0 {
            0
        } else {
            HEAP.get_object_field_value(module_ref, "java/lang/Module", "name")?[0]
        };
        let module_name = if module_name_ref == 0 {
            None
        } else {
            Some(get_utf8_string_by_ref(module_name_ref)?)
        };

        let loader_name = loader_name(loader_ref)?;

        Ok(Self {
            module_ref,
            module_name,
            loader_ref,
            loader_name,
        })
    }

    /// Each class loader has its own unnamed module in HotSpot, while rusty-jvm shares one
    /// between all loaders, so classes in it are only in the same module with the same loader.
    fn is_same_module(&self, other: &Self) -> bool {
        self.module_ref == other.module_ref
            && (self.module_name.is_some() || self.loader_ref == other.loader_ref)
    }

    /// E.g. `module java.base of loader 'bootstrap'` or `unnamed module of loader 'app'`.
    /// Unlike HotSpot, this leaves out the version of non-JDK named modules.
    fn describe(&self) -> String {
        match &self.module_name {
            Some(name) => format!("module {name} of loader {}", self.loader_name),
            None => format!("unnamed module of loader {}", self.loader_name),
        }
    }
}
