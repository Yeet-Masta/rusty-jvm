use crate::vm::error::{Error, Result};
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::{clazz_ref, clazz_ref_seen_by, ref_arg_chunks, undecorate};
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_frame::{ExceptionTable, FrameCode, StackFrame};
use crate::vm::stack::stack_value::StackValueKind;
use crate::vm::system_native::dispatcher::invoke::NativeLink;
use derive_new::new;
use getset::{CopyGetters, Getters};
use jclassmodel::attributes::LocalVariableInfo;
use jclassmodel::modifiers::MethodModifier;
use jdescriptor::MethodDescriptor;
use once_cell::sync::OnceCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct JavaMethod {
    method_descriptor: MethodDescriptor,
    class_name: Arc<String>,
    name_signature: Arc<String>,
    code_context: Option<CodeContext>,
    native: bool,

    reflection_ref: OnceCell<i32>,
    exceptions: Vec<String>,
    access_flags: i32,
    name: String,
    annotation_default_raw: Option<Vec<u8>>,
    annotations_raw: Option<Vec<u8>>,
    runtime_visible_annotations: HashSet<String>,
    slot: i32,
    /// Generic signature (`Signature` attribute) reported to reflection, if any.
    generic_signature: Option<String>,
    /// `MethodParameters` entries as `(name, access_flags)`; `None` without that attribute.
    parameters: Option<Vec<(Option<String>, u16)>>,
    /// Which of this method's argument chunks are references — see [`JavaMethod::arg_ref_chunks`].
    arg_ref_chunks: OnceCell<Vec<bool>>,
    /// Shared metadata for this method's frames, built on first call (after `with_class_name`).
    /// Built on the method's first frame and never freed, as the method itself is not.
    frame_code: OnceCell<&'static FrameCode>,
    /// What calling this native method needs that no call changes, resolved on its first call.
    native_link: OnceCell<NativeLink>,
    /// The intrinsic the interpreter runs for this method, if any, looked up on its first call.
    intrinsic: OnceCell<Option<crate::vm::execution_engine::intrinsics::Intrinsic>>,
}

#[derive(Debug, new, Getters, CopyGetters)]
pub(crate) struct CodeContext {
    #[get_copy = "pub"]
    max_stack: u16,
    #[get_copy = "pub"]
    max_locals: u16,
    #[get = "pub"]
    bytecode: Arc<Vec<u8>>,
    #[get = "pub"]
    line_numbers: Arc<BTreeMap<u16, u16>>,
    #[get = "pub"]
    exception_table: Arc<ExceptionTable>,
    /// `LocalVariableTable` entries (empty unless compiled with `javac -g`), used to render source
    /// variable names in helpful `NullPointerException` messages (JEP 358).
    #[get = "pub"]
    local_variable_table: Arc<Vec<LocalVariableInfo>>,
}

/// `method` for the life of the VM, without a reference count.
///
/// Sound because class unloading is disabled: every `JavaMethod` that runs belongs to a loaded
/// class, which the method registry holds for good (the invariant `Throwable.backtrace` and stack
/// walking rely on), and so does every call-site cache that resolved to it.
#[inline]
pub(crate) fn immortal(method: &Arc<JavaMethod>) -> &'static JavaMethod {
    // SAFETY: see above; the pointee is never freed while the VM runs.
    unsafe { &*Arc::as_ptr(method) }
}

impl JavaMethod {
    /// This method for the life of the VM; see [`immortal`]. Only for a method that runs, which
    /// belongs to a loaded class.
    fn immortal_self(&self) -> &'static JavaMethod {
        // SAFETY: as for `immortal`: a method that runs is owned by the permanent registry.
        unsafe { &*std::ptr::from_ref(self) }
    }

    /// The intrinsic the interpreter may run instead of this method; see
    /// [`intrinsics`](crate::vm::execution_engine::intrinsics).
    #[inline]
    pub(crate) fn intrinsic(&self) -> Option<crate::vm::execution_engine::intrinsics::Intrinsic> {
        *self.intrinsic.get_or_init(|| {
            crate::vm::execution_engine::intrinsics::lookup(&self.class_name, &self.name_signature)
        })
    }

    /// Replaces the class-name handle with one shared by the whole class.
    pub(crate) fn with_class_name(mut self, class_name: Arc<String>) -> Self {
        debug_assert_eq!(*self.class_name, *class_name);
        self.class_name = class_name;
        self
    }

    pub fn new(
        method_descriptor: MethodDescriptor,
        class_name: &str,
        name_signature: &str,
        code_context: Option<CodeContext>,
        native: bool,
        exceptions: Vec<String>,
        access_flags: i32,
        name: &str,
        annotation_default_raw: Option<Vec<u8>>,
        annotations_raw: Option<Vec<u8>>,
        runtime_visible_annotations: HashSet<String>,
        slot: i32,
    ) -> Self {
        Self {
            method_descriptor,
            class_name: Arc::new(class_name.to_string()),
            name_signature: Arc::new(name_signature.to_string()),
            code_context,
            native,
            reflection_ref: OnceCell::new(),
            exceptions,
            access_flags,
            name: name.to_string(),
            annotation_default_raw,
            annotations_raw,
            runtime_visible_annotations,
            slot,
            generic_signature: None,
            parameters: None,
            arg_ref_chunks: OnceCell::new(),
            frame_code: OnceCell::new(),
            intrinsic: OnceCell::new(),
            native_link: OnceCell::new(),
        }
    }

    /// Attaches the reflection-only metadata that the interpreter itself never reads.
    pub(crate) fn with_reflection_metadata(
        mut self,
        generic_signature: Option<String>,
        parameters: Option<Vec<(Option<String>, u16)>>,
    ) -> Self {
        self.generic_signature = generic_signature;
        self.parameters = parameters;
        self
    }

    /// Index of this method in its class's method table (`Method.slot`, `vmtarget`).
    pub(crate) fn slot(&self) -> i32 {
        self.slot
    }

    /// `MethodParameters` entries as `(name, access_flags)`, or `None` without that attribute.
    pub(crate) fn parameters(&self) -> Option<&[(Option<String>, u16)]> {
        self.parameters.as_deref()
    }

    pub fn new_stack_frame(&self) -> Result<StackFrame> {
        let context = self.code_context.as_ref().ok_or_else(|| {
            Error::new_execution(&format!(
                "Code context is missing for {}.{}",
                self.class_name, self.name_signature
            ))
        })?;
        let code = *self.frame_code.get_or_init(|| {
            Box::leak(Box::new(FrameCode {
                method_name: Arc::clone(&self.name_signature),
                bytecode: Arc::clone(context.bytecode()),
                class_name: Arc::clone(&self.class_name),
                line_numbers: Arc::clone(context.line_numbers()),
                exception_table: Arc::clone(context.exception_table()),
                klass: Default::default(),
                method: Some(self.immortal_self()),
            }))
        });
        let mut frame = StackFrame::from_code(
            code,
            context.max_locals() as usize,
            context.max_stack() as usize,
        );
        frame.set_synchronized_method(self.is_synchronized(), self.is_static());
        Ok(frame)
    }

    /// Builds a synthetic stack frame for this native method so it appears on the thread's stack
    /// chain while it executes (native methods carry no `code_context`, hence no `new_stack_frame`),
    /// holding `args` so its reference arguments stay visible as roots for the duration.
    pub fn new_native_stack_frame(&self, args: Vec<Slot>) -> StackFrame {
        let code = *self.frame_code.get_or_init(|| {
            Box::leak(Box::new(FrameCode {
                method: Some(self.immortal_self()),
                ..FrameCode::native(
                    Arc::clone(&self.name_signature),
                    Arc::clone(&self.class_name),
                )
            }))
        });
        StackFrame::native_from_code(code, args)
    }

    pub fn get_method_descriptor(&self) -> &MethodDescriptor {
        &self.method_descriptor
    }

    /// The method's code (bytecode, local-variable table, …), or `None` for native/abstract methods.
    pub fn code_context(&self) -> Option<&CodeContext> {
        self.code_context.as_ref()
    }

    pub(crate) fn native_link(&self) -> &OnceCell<NativeLink> {
        &self.native_link
    }

    pub fn is_native(&self) -> bool {
        self.native
    }

    pub fn class_name(&self) -> &str {
        &self.class_name
    }

    pub fn reflection_ref(&self) -> Result<i32> {
        if self.reflection_ref.get().is_none() {
            let reference = self.init_reflection_ref()?;
            let _ = self.reflection_ref.set(reference);
        }
        Ok(*self
            .reflection_ref
            .get()
            .expect("published reflection method"))
    }

    pub(crate) fn reflection_ref_if_present(&self) -> Option<i32> {
        self.reflection_ref.get().copied()
    }

    fn init_reflection_ref(&self) -> Result<i32> {
        if self.name == "<init>" {
            self.construct_constructor()
        } else {
            self.construct_method()
        }
    }

    /// Invokes
    /// ```java
    /// Method(Class<?> declaringClass,
    ///     String name,
    ///     Class<?>[] parameterTypes,
    ///     Class<?> returnType,
    ///     Class<?>[] exceptionTypes,
    ///     int modifiers,
    ///     int slot,
    ///     String signature,
    ///     byte[] annotations,
    ///     byte[] parameterAnnotations,
    ///     byte[] annotationDefault);
    /// ```
    fn construct_method(&self) -> Result<i32> {
        let mut roots = JavaThread::hold_temp_roots(vec![]);
        let mut destination = crate::vm::stack::rooted_slots::RootedSlots::new();
        let declaring_class_ref = clazz_ref(self.class_name())?;
        roots.add_root(Slot::Ref(declaring_class_ref));

        let mut name_signature_split = self.name_signature.split(':'); //fixme: crete separate field with name of get it from here
        let name = name_signature_split.next().unwrap();
        let name_ref = StringPoolHelper::get_string(name)?;
        roots.add_root(Slot::Ref(name_ref));

        let parameter_type_clazz_refs = self
            .method_descriptor
            .parameter_types()
            .iter()
            .map(|t| t.to_string())
            .map(|t| {
                let reference = clazz_ref_seen_by(self.class_name(), undecorate(&t))?;
                roots.add_root(Slot::Ref(reference));
                Ok(reference)
            })
            .collect::<Result<Vec<_>>>()?;
        let parameter_type_refs = HEAP.create_array_with_values(
            "[Ljava/lang/Class;",
            &parameter_type_clazz_refs,
            &mut destination,
        )?;
        roots.add_root(Slot::Ref(parameter_type_refs));

        let return_type = self.method_descriptor.return_type().to_string();
        let return_type_ref = clazz_ref_seen_by(self.class_name(), &return_type)?;
        roots.add_root(Slot::Ref(return_type_ref));

        let exception_type_clazz_refs = self
            .exceptions
            .iter()
            .map(|exception_type| {
                let reference = clazz_ref_seen_by(self.class_name(), exception_type)?;
                roots.add_root(Slot::Ref(reference));
                Ok(reference)
            })
            .collect::<Result<Vec<_>>>()?;
        let exception_type_refs = HEAP.create_array_with_values(
            "[Ljava/lang/Class;",
            &exception_type_clazz_refs,
            &mut destination,
        )?;
        roots.add_root(Slot::Ref(exception_type_refs));

        let modifiers = self.access_flags;

        let slot = self.slot;

        // `Executable.signature` is the *generic* signature (null when there is none), not the
        // erased descriptor: `getGenericReturnType()` etc. parse it.
        let _ = name_signature_split.next();
        let signature_ref = match self.generic_signature.as_deref() {
            Some(signature) => StringPoolHelper::get_string(signature)?,
            None => 0,
        };
        roots.add_root(Slot::Ref(signature_ref));

        let annotations = match self.annotations_raw.as_deref() {
            Some(raw) => HEAP.create_byte_array(raw, &mut destination)?,
            None => 0,
        };
        roots.add_root(Slot::Ref(annotations));

        let parameter_annotations = 0; // todo: implement me
        let annotation_default = match self.annotation_default_raw.as_deref() {
            Some(raw) => HEAP.create_byte_array(raw, &mut destination)?,
            None => 0,
        };
        roots.add_root(Slot::Ref(annotation_default));

        let args = &[
            StackValueKind::Ref(declaring_class_ref),
            StackValueKind::Ref(name_ref),
            StackValueKind::Ref(parameter_type_refs),
            StackValueKind::Ref(return_type_ref),
            StackValueKind::Ref(exception_type_refs),
            modifiers.into(),
            slot.into(),
            StackValueKind::Ref(signature_ref),
            StackValueKind::Ref(annotations),
            StackValueKind::Ref(parameter_annotations),
            StackValueKind::Ref(annotation_default),
        ];

        let method_ref = Executor::invoke_args_constructor(
            "java/lang/reflect/Method",
            "<init>:(Ljava/lang/Class;Ljava/lang/String;[Ljava/lang/Class;Ljava/lang/Class;[Ljava/lang/Class;IILjava/lang/String;[B[B[B)V",
            args,
            None
        )?;

        Ok(method_ref[0].value())
    }

    /// Invokes
    /// ```java
    /// Constructor(Class<T> declaringClass,
    ///     Class<?>[] parameterTypes,
    ///     Class<?>[] checkedExceptions,
    ///     int modifiers,
    ///     int slot,
    ///     String signature,
    ///     byte[] annotations,
    ///     byte[] parameterAnnotations);
    /// ```
    fn construct_constructor(&self) -> Result<i32> {
        let mut roots = JavaThread::hold_temp_roots(vec![]);
        let mut destination = crate::vm::stack::rooted_slots::RootedSlots::new();
        let declaring_class_ref = clazz_ref(self.class_name())?;
        roots.add_root(Slot::Ref(declaring_class_ref));

        let mut name_signature_split = self.name_signature.split(':'); //fixme: crete separate field with name of get it from here

        let parameter_type_clazz_refs = self
            .method_descriptor
            .parameter_types()
            .iter()
            .map(|t| t.to_string())
            .map(|t| {
                let reference = clazz_ref_seen_by(self.class_name(), undecorate(&t))?;
                roots.add_root(Slot::Ref(reference));
                Ok(reference)
            })
            .collect::<Result<Vec<_>>>()?;
        let parameter_type_refs = HEAP.create_array_with_values(
            "[Ljava/lang/Class;",
            &parameter_type_clazz_refs,
            &mut destination,
        )?;
        roots.add_root(Slot::Ref(parameter_type_refs));

        let exception_type_clazz_refs = self
            .exceptions
            .iter()
            .map(|exception_type| {
                let reference = clazz_ref_seen_by(self.class_name(), exception_type)?;
                roots.add_root(Slot::Ref(reference));
                Ok(reference)
            })
            .collect::<Result<Vec<_>>>()?;
        let checked_exception_type_refs = HEAP.create_array_with_values(
            "[Ljava/lang/Class;",
            &exception_type_clazz_refs,
            &mut destination,
        )?;
        roots.add_root(Slot::Ref(checked_exception_type_refs));

        let modifiers = self.access_flags;

        let slot = self.slot;

        // `Executable.signature` is the *generic* signature (null when there is none), not the
        // erased descriptor: `getGenericReturnType()` etc. parse it.
        let _ = name_signature_split.next();
        let signature_ref = match self.generic_signature.as_deref() {
            Some(signature) => StringPoolHelper::get_string(signature)?,
            None => 0,
        };
        roots.add_root(Slot::Ref(signature_ref));

        let annotations = match self.annotations_raw.as_deref() {
            Some(raw) => HEAP.create_byte_array(raw, &mut destination)?,
            None => 0,
        };
        roots.add_root(Slot::Ref(annotations));

        let parameter_annotations = 0; // todo: implement me

        let args = &[
            StackValueKind::Ref(declaring_class_ref),
            StackValueKind::Ref(parameter_type_refs),
            StackValueKind::Ref(checked_exception_type_refs),
            modifiers.into(),
            slot.into(),
            StackValueKind::Ref(signature_ref),
            StackValueKind::Ref(annotations),
            StackValueKind::Ref(parameter_annotations),
        ];

        let method_ref = Executor::invoke_args_constructor(
            "java/lang/reflect/Constructor",
            "<init>:(Ljava/lang/Class;[Ljava/lang/Class;[Ljava/lang/Class;IILjava/lang/String;[B[B)V",
            args,
            None
        )?;

        Ok(method_ref[0].value())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn access_flags(&self) -> i32 {
        self.access_flags
    }

    pub fn name_signature(&self) -> &str {
        &self.name_signature
    }

    /// Which of this method's argument chunks are references, in the order a caller collects them:
    /// receiver first for an instance method, then each parameter, `long` and `double` contributing
    /// two chunks each.
    ///
    /// Cached because it is a fixed property of the descriptor, and every call would otherwise walk
    /// the parameter list again to rediscover it.
    ///
    /// Polymorphic declarations are erased placeholders. Their callers must supply an effective
    /// MethodType to the invoker rather than asking the declaration to classify arguments.
    pub fn arg_ref_chunks(&self) -> Result<&[bool]> {
        self.arg_ref_chunks
            .get_or_try_init(|| {
                if self.is_polymorphic_signature() {
                    Err(Error::new_execution(
                        "polymorphic arguments require an effective MethodType",
                    ))
                } else {
                    ref_arg_chunks(&self.method_descriptor, !self.is_static())
                }
            })
            .map(Vec::as_slice)
    }

    pub fn is_static(&self) -> bool {
        MethodModifier::from_bits_truncate(self.access_flags as u16)
            .contains(MethodModifier::Static)
    }

    /// For a getter whose whole body is `aload_0; getfield #index; <x>return`: `index`. Such a
    /// call can read the field in the caller's frame, as HotSpot's accessor entries do.
    #[inline]
    pub(crate) fn accessor_field_index(&self) -> Option<u16> {
        const ALOAD_0: u8 = 0x2a;
        const GETFIELD: u8 = 0xb4;
        if self.is_static() || self.is_synchronized() {
            return None;
        }
        match self.code_context.as_ref()?.bytecode().as_slice() {
            // ireturn, lreturn, freturn, dreturn, areturn
            [ALOAD_0, GETFIELD, high, low, 0xac..=0xb0] => Some(u16::from_be_bytes([*high, *low])),
            _ => None,
        }
    }

    /// The method's class, once one of its frames has resolved it.
    #[inline]
    pub(crate) fn resolved_class(
        &self,
    ) -> Option<&Arc<crate::vm::method_area::java_class::JavaClass>> {
        self.frame_code.get()?.klass.get()
    }

    /// Whether a call to the method, holding no monitor, has no effect beyond consuming its
    /// arguments: its body is a lone `return` (`Object.<init>`, the end of most constructor
    /// chains), or a constructor that only calls such a no-argument super constructor
    /// (`aload_0; invokespecial; return`), once that `invokespecial` resolved its target.
    #[inline]
    pub(crate) fn is_empty_void(&self) -> bool {
        const ALOAD_0: u8 = 0x2a;
        const INVOKESPECIAL: u8 = 0xb7;
        const RETURN: u8 = 0xb1;
        if self.is_synchronized() {
            return false;
        }
        let Some(context) = self.code_context.as_ref() else {
            return false;
        };
        match *context.bytecode().as_slice() {
            [RETURN] => true,
            [ALOAD_0, INVOKESPECIAL, high, low, RETURN] => self
                .resolved_class()
                .and_then(|klass| {
                    klass.resolved_member_ref(
                        crate::vm::method_area::java_class::MemberRefKind::Method,
                        u16::from_be_bytes([high, low]),
                    )
                })
                .and_then(|member| member.special_target.get())
                .is_some_and(|target| {
                    // The receiver alone: anything else would leave the call's arguments to it.
                    target.arg_ref_chunks().is_ok_and(|chunks| chunks.len() == 1)
                        && target.is_empty_void()
                }),
            _ => false,
        }
    }

    pub fn is_synchronized(&self) -> bool {
        MethodModifier::from_bits_truncate(self.access_flags as u16)
            .contains(MethodModifier::Synchronized)
    }

    /// Whether this is a `@PolymorphicSignature` intrinsic (`MethodHandle.invoke*`, `VarHandle`
    /// accessors).
    ///
    /// Such a method's declared descriptor is a placeholder — `(Object[])Object` — rather than the
    /// types at any particular call site, so it must not be used to reason about argument or return
    /// values. Only the resolved `MethodType` knows those.
    pub fn is_polymorphic_signature(&self) -> bool {
        // Signature polymorphic methods are native by definition (JVMS 2.9.3); checking that
        // first keeps every other call from hashing the annotation name.
        self.native
            && self.has_annotation("Ljava/lang/invoke/MethodHandle$PolymorphicSignature;")
    }

    /// Whether the method carries the runtime-visible annotation of type `descriptor`
    /// (`Ljdk/internal/reflect/CallerSensitive;`).
    pub fn has_annotation(&self, descriptor: &str) -> bool {
        self.runtime_visible_annotations.contains(descriptor)
    }
}

#[cfg(test)]
mod gc_tests {
    use super::*;

    #[test]
    fn gc_roots_method_and_constructor_reflection_caches_are_non_initializing() {
        for name in ["method", "<init>"] {
            let method = JavaMethod::new(
                "()V".parse().unwrap(),
                "Unloaded",
                &format!("{name}:()V"),
                None,
                false,
                vec![],
                0,
                name,
                None,
                None,
                HashSet::new(),
                0,
            );
            assert_eq!(method.reflection_ref_if_present(), None);
            method.reflection_ref.set(83).unwrap();
            assert_eq!(method.reflection_ref_if_present(), Some(83));
        }
    }
}
