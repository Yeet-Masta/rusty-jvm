use crate::vm::error::Result;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::system_native::method_handle_natives::invocation::{
    invoke_exact, invoke_with_call_site_type,
};
use jdescriptor::MethodDescriptor;

pub(crate) fn method_handle_invoke_exact_wrp(args: &[Slot]) -> Result<RootedSlots> {
    let handle_ref = args[0].value();
    let method_args = &args[1..];
    invoke_exact(handle_ref, method_args)?;
    Ok(RootedSlots::new())
}

pub(crate) fn method_handle_invoke_basic_wrp(args: &[Slot]) -> Result<RootedSlots> {
    method_handle_invoke_exact_wrp(args) // TODO: implement real invokeBasic
}

pub(crate) fn method_handle_invoke_wrp(
    args: &[Slot],
    call_site: &MethodDescriptor,
) -> Result<RootedSlots> {
    let handle_ref = args[0].value();
    invoke_with_call_site_type(handle_ref, &args[1..], call_site)?;
    Ok(RootedSlots::new())
}
