use crate::vm::error::{Error, Result};
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::system_native::method_handle_natives::var_handle::{
    invoke_access_mode, is_access_mode,
};
use crate::vm::system_native::method_handle_natives::wrappers::{
    method_handle_invoke_basic_wrp, method_handle_invoke_exact_wrp, method_handle_invoke_wrp,
};
use jdescriptor::MethodDescriptor;

/// Dispatches a signature-polymorphic native. `call_site` is the call site's (or adapter's)
/// method type, without the `MethodHandle`/`VarHandle` receiver.
pub(crate) fn invoke_polymorphic(
    class_name: &str,
    method_name: &str,
    args: &[Slot],
    call_site: &MethodDescriptor,
    destination: &mut RootedSlots,
) -> Result<()> {
    let result = match (class_name, method_name) {
        ("java/lang/invoke/MethodHandle", "invokeExact") => method_handle_invoke_exact_wrp(args),
        ("java/lang/invoke/MethodHandle", "invokeBasic") => method_handle_invoke_basic_wrp(args),
        ("java/lang/invoke/MethodHandle", "invoke") => method_handle_invoke_wrp(args, call_site),
        ("java/lang/invoke/VarHandle", mode) if is_access_mode(mode) => {
            invoke_access_mode(mode, args, call_site)
        }
        _ => Err(Error::new_native(&format!(
            "Unknown polymorphic-signature native method: {class_name}:{method_name}"
        ))),
    }?;
    destination.replace(result.to_vec());
    Ok(())
}
