/*
 * The C side of the variadic JNI calls (Call<Type>Method, Call<Type>MethodV, NewObject, ...).
 *
 * Rust cannot portably define C-variadic functions or read a va_list on the supported toolchains,
 * so this shim does both and hands each argument to Rust, which resolves the method descriptor
 * and delegates to the corresponding Call<Type>MethodA entry point.
 */
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>

/*
 * The few JNI types the shim needs, with the sizes and representations jni.h and jni_md.h give
 * them on every supported (64-bit) platform. Declaring them here keeps the VM build independent of
 * the JDK headers, so it needs neither JAVA_HOME nor a full JDK.
 */
typedef uint8_t jboolean;
typedef int8_t jbyte;
typedef uint16_t jchar;
typedef int16_t jshort;
typedef int32_t jint;
typedef int64_t jlong;
typedef float jfloat;
typedef double jdouble;
typedef struct _jobject *jobject;
typedef jobject jclass;
typedef struct _jmethodID *jmethodID;
typedef union jvalue {
    jboolean z;
    jbyte b;
    jchar c;
    jshort s;
    jint i;
    jlong j;
    jfloat f;
    jdouble d;
    jobject l;
} jvalue;
/* Only ever passed through to Rust, which knows its layout */
typedef const struct JNINativeInterface_ *JNIEnv;

#if defined(_WIN32)
#define JNICALL __stdcall
#else
#define JNICALL
#endif

/* Crosses to Rust as a jint; must match `InvocationKind` in variadic.rs */
enum InvocationKind { VIRTUAL = 0, NONVIRTUAL = 1, STATIC = 2, CONSTRUCTOR = 3 };

extern jvalue rusty_jvm_invoke_variadic(JNIEnv *, jobject, jclass, jmethodID, void *, jint);

void rusty_jvm_next_argument(void *arguments, char tag, jvalue *output) {
    va_list *list = (va_list *)arguments;
    switch (tag) {
        /* Like HotSpot (jni.cpp), truncate to jboolean before normalizing: 256 is false */
        case 'Z': output->z = (jboolean)va_arg(*list, int) != 0; break;
        case 'B': output->b = (jbyte)va_arg(*list, int); break;
        case 'C': output->c = (jchar)va_arg(*list, int); break;
        case 'S': output->s = (jshort)va_arg(*list, int); break;
        case 'I': output->i = va_arg(*list, jint); break;
        case 'J': output->j = va_arg(*list, jlong); break;
        case 'F': output->f = (jfloat)va_arg(*list, jdouble); break;
        case 'D': output->d = va_arg(*list, jdouble); break;
        default: output->l = va_arg(*list, jobject); break;
    }
}

#define DEFINE_CALL(Name, Type, Member, Parameters, Receiver, Target, Kind) \
Type JNICALL rusty_jvm_##Name##V Parameters { \
    va_list copy; \
    va_copy(copy, arguments); \
    jvalue result = rusty_jvm_invoke_variadic(env, Receiver, Target, method, &copy, Kind); \
    va_end(copy); \
    return result.Member; \
}

#define DEFINE_VIRTUAL(Name, Type, Member) \
DEFINE_CALL(Call##Name##Method, Type, Member, \
    (JNIEnv *env, jobject receiver, jmethodID method, va_list arguments), receiver, NULL, VIRTUAL); \
Type rusty_jvm_Call##Name##Method(JNIEnv *env, jobject receiver, jmethodID method, ...) { \
    va_list arguments; \
    va_start(arguments, method); \
    Type result = rusty_jvm_Call##Name##MethodV(env, receiver, method, arguments); \
    va_end(arguments); \
    return result; \
}

#define DEFINE_NONVIRTUAL(Name, Type, Member) \
DEFINE_CALL(CallNonvirtual##Name##Method, Type, Member, \
    (JNIEnv *env, jobject receiver, jclass target, jmethodID method, va_list arguments), receiver, target, NONVIRTUAL); \
Type rusty_jvm_CallNonvirtual##Name##Method(JNIEnv *env, jobject receiver, jclass target, jmethodID method, ...) { \
    va_list arguments; \
    va_start(arguments, method); \
    Type result = rusty_jvm_CallNonvirtual##Name##MethodV(env, receiver, target, method, arguments); \
    va_end(arguments); \
    return result; \
}

#define DEFINE_STATIC(Name, Type, Member) \
DEFINE_CALL(CallStatic##Name##Method, Type, Member, \
    (JNIEnv *env, jclass target, jmethodID method, va_list arguments), NULL, target, STATIC); \
Type rusty_jvm_CallStatic##Name##Method(JNIEnv *env, jclass target, jmethodID method, ...) { \
    va_list arguments; \
    va_start(arguments, method); \
    Type result = rusty_jvm_CallStatic##Name##MethodV(env, target, method, arguments); \
    va_end(arguments); \
    return result; \
}

DEFINE_VIRTUAL(Object, jobject, l)
DEFINE_NONVIRTUAL(Object, jobject, l)
DEFINE_STATIC(Object, jobject, l)

DEFINE_VIRTUAL(Boolean, jboolean, z)
DEFINE_NONVIRTUAL(Boolean, jboolean, z)
DEFINE_STATIC(Boolean, jboolean, z)

DEFINE_VIRTUAL(Byte, jbyte, b)
DEFINE_NONVIRTUAL(Byte, jbyte, b)
DEFINE_STATIC(Byte, jbyte, b)

DEFINE_VIRTUAL(Char, jchar, c)
DEFINE_NONVIRTUAL(Char, jchar, c)
DEFINE_STATIC(Char, jchar, c)

DEFINE_VIRTUAL(Short, jshort, s)
DEFINE_NONVIRTUAL(Short, jshort, s)
DEFINE_STATIC(Short, jshort, s)

DEFINE_VIRTUAL(Int, jint, i)
DEFINE_NONVIRTUAL(Int, jint, i)
DEFINE_STATIC(Int, jint, i)

DEFINE_VIRTUAL(Long, jlong, j)
DEFINE_NONVIRTUAL(Long, jlong, j)
DEFINE_STATIC(Long, jlong, j)

DEFINE_VIRTUAL(Float, jfloat, f)
DEFINE_NONVIRTUAL(Float, jfloat, f)
DEFINE_STATIC(Float, jfloat, f)

DEFINE_VIRTUAL(Double, jdouble, d)
DEFINE_NONVIRTUAL(Double, jdouble, d)
DEFINE_STATIC(Double, jdouble, d)

void JNICALL rusty_jvm_CallVoidMethodV(JNIEnv *env, jobject receiver, jmethodID method, va_list arguments) {
    va_list copy;
    va_copy(copy, arguments);
    rusty_jvm_invoke_variadic(env, receiver, NULL, method, &copy, VIRTUAL);
    va_end(copy);
}
void rusty_jvm_CallVoidMethod(JNIEnv *env, jobject receiver, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    rusty_jvm_CallVoidMethodV(env, receiver, method, arguments);
    va_end(arguments);
}

void JNICALL rusty_jvm_CallNonvirtualVoidMethodV(JNIEnv *env, jobject receiver, jclass target, jmethodID method, va_list arguments) {
    va_list copy;
    va_copy(copy, arguments);
    rusty_jvm_invoke_variadic(env, receiver, target, method, &copy, NONVIRTUAL);
    va_end(copy);
}
void rusty_jvm_CallNonvirtualVoidMethod(JNIEnv *env, jobject receiver, jclass target, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    rusty_jvm_CallNonvirtualVoidMethodV(env, receiver, target, method, arguments);
    va_end(arguments);
}

void JNICALL rusty_jvm_CallStaticVoidMethodV(JNIEnv *env, jclass target, jmethodID method, va_list arguments) {
    va_list copy;
    va_copy(copy, arguments);
    rusty_jvm_invoke_variadic(env, NULL, target, method, &copy, STATIC);
    va_end(copy);
}
void rusty_jvm_CallStaticVoidMethod(JNIEnv *env, jclass target, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    rusty_jvm_CallStaticVoidMethodV(env, target, method, arguments);
    va_end(arguments);
}

jobject JNICALL rusty_jvm_NewObjectV(JNIEnv *env, jclass target, jmethodID method, va_list arguments) {
    va_list copy;
    va_copy(copy, arguments);
    jvalue result = rusty_jvm_invoke_variadic(env, NULL, target, method, &copy, CONSTRUCTOR);
    va_end(copy);
    return result.l;
}
jobject rusty_jvm_NewObject(JNIEnv *env, jclass target, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    jobject result = rusty_jvm_NewObjectV(env, target, method, arguments);
    va_end(arguments);
    return result;
}

