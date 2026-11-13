#include <jni.h>
#include <stdarg.h>
#include <stdio.h>

#define ARGUMENTS JNI_TRUE, -101, 0xabcd, -23456, 123456, (jlong)0x1122334455667788LL, (jdouble)1.25, (jdouble)-2.5, receiver, 1, 2, 3, 4, 5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0
#define SIGNATURE "(ZBCSIJFDLjava/lang/Object;IIIIIDDDDDDDD)"

#define DEFINE_LIST_CALL(Name, Type) \
static Type call_##Name##_list(JNIEnv *env, jobject receiver, jmethodID method, ...) { \
    va_list arguments; \
    va_start(arguments, method); \
    Type result = (*env)->Call##Name##MethodV(env, receiver, method, arguments); \
    va_end(arguments); \
    return result; \
} \
static Type nonvirtual_##Name##_list(JNIEnv *env, jobject receiver, jclass target, jmethodID method, ...) { \
    va_list arguments; \
    va_start(arguments, method); \
    Type result = (*env)->CallNonvirtual##Name##MethodV(env, receiver, target, method, arguments); \
    va_end(arguments); \
    return result; \
} \
static Type static_##Name##_list(JNIEnv *env, jclass target, jmethodID method, ...) { \
    va_list arguments; \
    va_start(arguments, method); \
    Type result = (*env)->CallStatic##Name##MethodV(env, target, method, arguments); \
    va_end(arguments); \
    return result; \
}

DEFINE_LIST_CALL(Object, jobject)
DEFINE_LIST_CALL(Boolean, jboolean)
DEFINE_LIST_CALL(Byte, jbyte)
DEFINE_LIST_CALL(Char, jchar)
DEFINE_LIST_CALL(Short, jshort)
DEFINE_LIST_CALL(Int, jint)
DEFINE_LIST_CALL(Long, jlong)
DEFINE_LIST_CALL(Float, jfloat)
DEFINE_LIST_CALL(Double, jdouble)

static void call_Void_list(JNIEnv *env, jobject receiver, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    (*env)->CallVoidMethodV(env, receiver, method, arguments);
    va_end(arguments);
}
static void nonvirtual_Void_list(JNIEnv *env, jobject receiver, jclass target, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    (*env)->CallNonvirtualVoidMethodV(env, receiver, target, method, arguments);
    va_end(arguments);
}
static void static_Void_list(JNIEnv *env, jclass target, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    (*env)->CallStaticVoidMethodV(env, target, method, arguments);
    va_end(arguments);
}
static jobject new_object_list(JNIEnv *env, jclass target, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    jobject result = (*env)->NewObjectV(env, target, method, arguments);
    va_end(arguments);
    return result;
}
static jint twice_list(JNIEnv *env, jobject receiver, jmethodID method, ...) {
    va_list arguments;
    va_start(arguments, method);
    jint first = (*env)->CallIntMethodV(env, receiver, method, arguments);
    jint second = (*env)->CallIntMethodV(env, receiver, method, arguments);
    va_end(arguments);
    return first == 654321 && second == first;
}
/* Prints which check failed, so a failure needs no counting of checks in this file */
#define CHECK(failed, description) \
    do { \
        if (failed) { \
            fprintf(stderr, "JNI check failed: %s (variadic_demo.c:%d)\n", description, __LINE__); \
            return 1; \
        } \
    } while (0)

jint jni_variadic_demo(JNIEnv *env, jobject receiver, jclass base) {
    jmethodID method;
    jmethodID static_method;
    method = (*env)->GetMethodID(env, base, "valueObject", SIGNATURE "Ljava/lang/Object;");
    static_method = (*env)->GetStaticMethodID(env, base, "staticObject", SIGNATURE "Ljava/lang/Object;");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    CHECK(!(*env)->IsSameObject(env, (*env)->CallObjectMethod(env, receiver, method, ARGUMENTS), receiver), "CallObjectMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallObjectMethod");
    CHECK(!(*env)->IsSameObject(env, call_Object_list(env, receiver, method, ARGUMENTS), receiver), "CallObjectMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallObjectMethodV");
    CHECK(!(*env)->IsSameObject(env, (*env)->CallNonvirtualObjectMethod(env, receiver, base, method, ARGUMENTS), receiver), "CallNonvirtualObjectMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualObjectMethod");
    CHECK(!(*env)->IsSameObject(env, nonvirtual_Object_list(env, receiver, base, method, ARGUMENTS), receiver), "CallNonvirtualObjectMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualObjectMethodV");
    CHECK(!(*env)->IsSameObject(env, (*env)->CallStaticObjectMethod(env, base, static_method, ARGUMENTS), receiver), "CallStaticObjectMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticObjectMethod");
    CHECK(!(*env)->IsSameObject(env, static_Object_list(env, base, static_method, ARGUMENTS), receiver), "CallStaticObjectMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticObjectMethodV");
    method = (*env)->GetMethodID(env, base, "valueBoolean", SIGNATURE "Z");
    static_method = (*env)->GetStaticMethodID(env, base, "staticBoolean", SIGNATURE "Z");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    CHECK((*env)->CallBooleanMethod(env, receiver, method, ARGUMENTS) != JNI_TRUE, "CallBooleanMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallBooleanMethod");
    CHECK(call_Boolean_list(env, receiver, method, ARGUMENTS) != JNI_TRUE, "CallBooleanMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallBooleanMethodV");
    CHECK((*env)->CallNonvirtualBooleanMethod(env, receiver, base, method, ARGUMENTS) != JNI_TRUE, "CallNonvirtualBooleanMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualBooleanMethod");
    CHECK(nonvirtual_Boolean_list(env, receiver, base, method, ARGUMENTS) != JNI_TRUE, "CallNonvirtualBooleanMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualBooleanMethodV");
    CHECK((*env)->CallStaticBooleanMethod(env, base, static_method, ARGUMENTS) != JNI_TRUE, "CallStaticBooleanMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticBooleanMethod");
    CHECK(static_Boolean_list(env, base, static_method, ARGUMENTS) != JNI_TRUE, "CallStaticBooleanMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticBooleanMethodV");
    method = (*env)->GetMethodID(env, base, "valueByte", SIGNATURE "B");
    static_method = (*env)->GetStaticMethodID(env, base, "staticByte", SIGNATURE "B");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    CHECK((*env)->CallByteMethod(env, receiver, method, ARGUMENTS) != -101, "CallByteMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallByteMethod");
    CHECK(call_Byte_list(env, receiver, method, ARGUMENTS) != -101, "CallByteMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallByteMethodV");
    CHECK((*env)->CallNonvirtualByteMethod(env, receiver, base, method, ARGUMENTS) != -101, "CallNonvirtualByteMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualByteMethod");
    CHECK(nonvirtual_Byte_list(env, receiver, base, method, ARGUMENTS) != -101, "CallNonvirtualByteMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualByteMethodV");
    CHECK((*env)->CallStaticByteMethod(env, base, static_method, ARGUMENTS) != -101, "CallStaticByteMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticByteMethod");
    CHECK(static_Byte_list(env, base, static_method, ARGUMENTS) != -101, "CallStaticByteMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticByteMethodV");
    method = (*env)->GetMethodID(env, base, "valueChar", SIGNATURE "C");
    static_method = (*env)->GetStaticMethodID(env, base, "staticChar", SIGNATURE "C");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    CHECK((*env)->CallCharMethod(env, receiver, method, ARGUMENTS) != 0xabcd, "CallCharMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallCharMethod");
    CHECK(call_Char_list(env, receiver, method, ARGUMENTS) != 0xabcd, "CallCharMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallCharMethodV");
    CHECK((*env)->CallNonvirtualCharMethod(env, receiver, base, method, ARGUMENTS) != 0xabcd, "CallNonvirtualCharMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualCharMethod");
    CHECK(nonvirtual_Char_list(env, receiver, base, method, ARGUMENTS) != 0xabcd, "CallNonvirtualCharMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualCharMethodV");
    CHECK((*env)->CallStaticCharMethod(env, base, static_method, ARGUMENTS) != 0xabcd, "CallStaticCharMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticCharMethod");
    CHECK(static_Char_list(env, base, static_method, ARGUMENTS) != 0xabcd, "CallStaticCharMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticCharMethodV");
    method = (*env)->GetMethodID(env, base, "valueShort", SIGNATURE "S");
    static_method = (*env)->GetStaticMethodID(env, base, "staticShort", SIGNATURE "S");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    CHECK((*env)->CallShortMethod(env, receiver, method, ARGUMENTS) != -23456, "CallShortMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallShortMethod");
    CHECK(call_Short_list(env, receiver, method, ARGUMENTS) != -23456, "CallShortMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallShortMethodV");
    CHECK((*env)->CallNonvirtualShortMethod(env, receiver, base, method, ARGUMENTS) != -23456, "CallNonvirtualShortMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualShortMethod");
    CHECK(nonvirtual_Short_list(env, receiver, base, method, ARGUMENTS) != -23456, "CallNonvirtualShortMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualShortMethodV");
    CHECK((*env)->CallStaticShortMethod(env, base, static_method, ARGUMENTS) != -23456, "CallStaticShortMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticShortMethod");
    CHECK(static_Short_list(env, base, static_method, ARGUMENTS) != -23456, "CallStaticShortMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticShortMethodV");
    method = (*env)->GetMethodID(env, base, "valueInt", SIGNATURE "I");
    static_method = (*env)->GetStaticMethodID(env, base, "staticInt", SIGNATURE "I");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    CHECK((*env)->CallIntMethod(env, receiver, method, ARGUMENTS) != 654321, "CallIntMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallIntMethod");
    CHECK(call_Int_list(env, receiver, method, ARGUMENTS) != 654321, "CallIntMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallIntMethodV");
    CHECK((*env)->CallNonvirtualIntMethod(env, receiver, base, method, ARGUMENTS) != 123456, "CallNonvirtualIntMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualIntMethod");
    CHECK(nonvirtual_Int_list(env, receiver, base, method, ARGUMENTS) != 123456, "CallNonvirtualIntMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualIntMethodV");
    CHECK((*env)->CallStaticIntMethod(env, base, static_method, ARGUMENTS) != 123456, "CallStaticIntMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticIntMethod");
    CHECK(static_Int_list(env, base, static_method, ARGUMENTS) != 123456, "CallStaticIntMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticIntMethodV");
    method = (*env)->GetMethodID(env, base, "valueLong", SIGNATURE "J");
    static_method = (*env)->GetStaticMethodID(env, base, "staticLong", SIGNATURE "J");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    CHECK((*env)->CallLongMethod(env, receiver, method, ARGUMENTS) != (jlong)0x1122334455667788LL, "CallLongMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallLongMethod");
    CHECK(call_Long_list(env, receiver, method, ARGUMENTS) != (jlong)0x1122334455667788LL, "CallLongMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallLongMethodV");
    CHECK((*env)->CallNonvirtualLongMethod(env, receiver, base, method, ARGUMENTS) != (jlong)0x1122334455667788LL, "CallNonvirtualLongMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualLongMethod");
    CHECK(nonvirtual_Long_list(env, receiver, base, method, ARGUMENTS) != (jlong)0x1122334455667788LL, "CallNonvirtualLongMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualLongMethodV");
    CHECK((*env)->CallStaticLongMethod(env, base, static_method, ARGUMENTS) != (jlong)0x1122334455667788LL, "CallStaticLongMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticLongMethod");
    CHECK(static_Long_list(env, base, static_method, ARGUMENTS) != (jlong)0x1122334455667788LL, "CallStaticLongMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticLongMethodV");
    method = (*env)->GetMethodID(env, base, "valueFloat", SIGNATURE "F");
    static_method = (*env)->GetStaticMethodID(env, base, "staticFloat", SIGNATURE "F");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    CHECK((*env)->CallFloatMethod(env, receiver, method, ARGUMENTS) != 1.25f, "CallFloatMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallFloatMethod");
    CHECK(call_Float_list(env, receiver, method, ARGUMENTS) != 1.25f, "CallFloatMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallFloatMethodV");
    CHECK((*env)->CallNonvirtualFloatMethod(env, receiver, base, method, ARGUMENTS) != 1.25f, "CallNonvirtualFloatMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualFloatMethod");
    CHECK(nonvirtual_Float_list(env, receiver, base, method, ARGUMENTS) != 1.25f, "CallNonvirtualFloatMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualFloatMethodV");
    CHECK((*env)->CallStaticFloatMethod(env, base, static_method, ARGUMENTS) != 1.25f, "CallStaticFloatMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticFloatMethod");
    CHECK(static_Float_list(env, base, static_method, ARGUMENTS) != 1.25f, "CallStaticFloatMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticFloatMethodV");
    method = (*env)->GetMethodID(env, base, "valueDouble", SIGNATURE "D");
    static_method = (*env)->GetStaticMethodID(env, base, "staticDouble", SIGNATURE "D");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    CHECK((*env)->CallDoubleMethod(env, receiver, method, ARGUMENTS) != -2.5, "CallDoubleMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallDoubleMethod");
    CHECK(call_Double_list(env, receiver, method, ARGUMENTS) != -2.5, "CallDoubleMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallDoubleMethodV");
    CHECK((*env)->CallNonvirtualDoubleMethod(env, receiver, base, method, ARGUMENTS) != -2.5, "CallNonvirtualDoubleMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualDoubleMethod");
    CHECK(nonvirtual_Double_list(env, receiver, base, method, ARGUMENTS) != -2.5, "CallNonvirtualDoubleMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualDoubleMethodV");
    CHECK((*env)->CallStaticDoubleMethod(env, base, static_method, ARGUMENTS) != -2.5, "CallStaticDoubleMethod");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticDoubleMethod");
    CHECK(static_Double_list(env, base, static_method, ARGUMENTS) != -2.5, "CallStaticDoubleMethodV");
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticDoubleMethodV");
    method = (*env)->GetMethodID(env, base, "valueVoid", SIGNATURE "V");
    static_method = (*env)->GetStaticMethodID(env, base, "staticVoid", SIGNATURE "V");
    CHECK(!method || !static_method, "GetMethodID and GetStaticMethodID");
    (*env)->CallVoidMethod(env, receiver, method, ARGUMENTS);
    CHECK((*env)->ExceptionCheck(env), "no exception from CallVoidMethod");
    call_Void_list(env, receiver, method, ARGUMENTS);
    CHECK((*env)->ExceptionCheck(env), "no exception from CallVoidMethodV");
    (*env)->CallNonvirtualVoidMethod(env, receiver, base, method, ARGUMENTS);
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualVoidMethod");
    nonvirtual_Void_list(env, receiver, base, method, ARGUMENTS);
    CHECK((*env)->ExceptionCheck(env), "no exception from CallNonvirtualVoidMethodV");
    (*env)->CallStaticVoidMethod(env, base, static_method, ARGUMENTS);
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticVoidMethod");
    static_Void_list(env, base, static_method, ARGUMENTS);
    CHECK((*env)->ExceptionCheck(env), "no exception from CallStaticVoidMethodV");
    jfieldID count = (*env)->GetStaticFieldID(env, base, "voidCalls", "I");
    CHECK((*env)->GetStaticIntField(env, base, count) != 6, "each void call ran exactly once");
    method = (*env)->GetMethodID(env, base, "valueInt", SIGNATURE "I");
    CHECK(!twice_list(env, receiver, method, ARGUMENTS), "one va_list reused by two CallIntMethodV calls");
    method = (*env)->GetMethodID(env, base, "<init>", SIGNATURE "V");
    jfieldID marker = (*env)->GetFieldID(env, base, "marker", "I");
    jobject first = (*env)->NewObject(env, base, method, ARGUMENTS);
    jobject second = new_object_list(env, base, method, ARGUMENTS);
    CHECK(!first || !second || (*env)->ExceptionCheck(env), "NewObject and NewObjectV construct");
    CHECK((*env)->GetIntField(env, first, marker) != 42 || (*env)->GetIntField(env, second, marker) != 42, "NewObject and NewObjectV pass the constructor arguments");
    method = (*env)->GetMethodID(env, base, "zero", "()I");
    CHECK((*env)->CallIntMethod(env, receiver, method) != 11, "CallIntMethod without arguments");
    CHECK(call_Int_list(env, receiver, method) != 11, "CallIntMethodV without arguments");
    CHECK((*env)->CallNonvirtualIntMethod(env, receiver, base, method) != 9, "CallNonvirtualIntMethod without arguments");
    CHECK(nonvirtual_Int_list(env, receiver, base, method) != 9, "CallNonvirtualIntMethodV without arguments");
    method = (*env)->GetStaticMethodID(env, base, "staticZero", "()I");
    CHECK((*env)->CallStaticIntMethod(env, base, method) != 10 || static_Int_list(env, base, method) != 10, "CallStaticIntMethod and CallStaticIntMethodV without arguments");
    method = (*env)->GetMethodID(env, base, "throwing", "()I");
    CHECK((*env)->CallIntMethod(env, receiver, method) != 0 || !(*env)->ExceptionCheck(env), "CallIntMethod throws");
    (*env)->ExceptionClear(env);
    CHECK(call_Int_list(env, receiver, method) != 0 || !(*env)->ExceptionCheck(env), "CallIntMethodV throws");
    (*env)->ExceptionClear(env);
    method = (*env)->GetMethodID(env, base, "throwing", "()I");
    CHECK((*env)->CallNonvirtualIntMethod(env, receiver, base, method) != 0 || !(*env)->ExceptionCheck(env), "CallNonvirtualIntMethod throws");
    (*env)->ExceptionClear(env);
    CHECK(nonvirtual_Int_list(env, receiver, base, method) != 0 || !(*env)->ExceptionCheck(env), "CallNonvirtualIntMethodV throws");
    (*env)->ExceptionClear(env);
    method = (*env)->GetStaticMethodID(env, base, "staticThrowing", "()I");
    CHECK((*env)->CallStaticIntMethod(env, base, method) != 0 || !(*env)->ExceptionCheck(env), "CallStaticIntMethod throws");
    (*env)->ExceptionClear(env);
    CHECK(static_Int_list(env, base, method) != 0 || !(*env)->ExceptionCheck(env), "CallStaticIntMethodV throws");
    (*env)->ExceptionClear(env);
    method = (*env)->GetMethodID(env, base, "<init>", "(I)V");
    CHECK((*env)->NewObject(env, base, method, -1) != NULL || !(*env)->ExceptionCheck(env), "NewObject throws");
    (*env)->ExceptionClear(env);
    CHECK(new_object_list(env, base, method, -1) != NULL || !(*env)->ExceptionCheck(env), "NewObjectV throws");
    (*env)->ExceptionClear(env);
    return 0;
}
