/*
 * rusty-jvm's stand-in for HotSpot's jvm.dll / libjvm.so / libjvm.dylib.
 *
 * The JDK's own native libraries (libjava, libnet, libnio, libmanagement, libawt, ...) are JNI
 * libraries that additionally import a set of JVM_* functions from the VM library. Built as a
 * shared library named like HotSpot's and loaded before any of them, this file satisfies those
 * imports so the JDK's native code can run on rusty-jvm:
 *
 *   - a few functions that need no VM state are implemented here directly;
 *   - every other JVM_* export forwards to a function the VM registers at startup with
 *     rusty_jvm_shim_set(). One the VM has not registered reports its name and aborts, which only
 *     happens if JDK native code calls into a VM service rusty-jvm does not provide yet.
 *
 * Forwarders pass eight pointer-sized integer arguments through. On the supported 64-bit ABIs
 * (Windows x64, System V x86-64, AAPCS64) integer and pointer arguments go in registers or caller-
 * owned stack slots, so a callee declared with fewer parameters receives its own correctly, and no
 * JVM_* function takes or returns a floating-point value.
 */
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>

#ifdef _WIN32
#include <windows.h>
#define SHIM_EXPORT __declspec(dllexport)
#else
#include <pthread.h>
#include <time.h>
#include <unistd.h>
#define SHIM_EXPORT __attribute__((visibility("default")))
#endif

typedef uintptr_t (*shim_forward_fn)(uintptr_t, uintptr_t, uintptr_t, uintptr_t, uintptr_t,
                                     uintptr_t, uintptr_t, uintptr_t);

static void shim_missing(const char *name) {
    fprintf(stderr, "rusty-jvm: the JDK native library called %s, which rusty-jvm does not implement\n",
            name);
    fflush(stderr);
    abort();
}

#define JVM_FORWARD(name)                                                                          \
    static shim_forward_fn slot_##name;                                                            \
    SHIM_EXPORT uintptr_t name(uintptr_t a0, uintptr_t a1, uintptr_t a2, uintptr_t a3,             \
                               uintptr_t a4, uintptr_t a5, uintptr_t a6, uintptr_t a7) {           \
        shim_forward_fn target = slot_##name;                                                      \
        if (target == NULL) {                                                                      \
            shim_missing(#name);                                                                   \
        }                                                                                          \
        return target(a0, a1, a2, a3, a4, a5, a6, a7);                                             \
    }
#include "jvm_forwarders.inc"
#undef JVM_FORWARD

struct shim_slot {
    const char *name;
    shim_forward_fn *slot;
};

#define JVM_FORWARD(name) {#name, &slot_##name},
static const struct shim_slot shim_slots[] = {
#include "jvm_forwarders.inc"
    {NULL, NULL}};
#undef JVM_FORWARD

/* Registers the VM's implementation of `name`. Returns 1 if `name` is a forwarded export. */
SHIM_EXPORT int rusty_jvm_shim_set(const char *name, void *function) {
    for (const struct shim_slot *s = shim_slots; s->name != NULL; s++) {
        if (strcmp(s->name, name) == 0) {
            *s->slot = (shim_forward_fn)function;
            return 1;
        }
    }
    return 0;
}

/* Identifies this library as rusty-jvm's shim (and its interface revision). */
SHIM_EXPORT int rusty_jvm_shim_version(void) { return 1; }

/* ---- jio_*: HotSpot's printf family, with guaranteed NUL termination ---- */

SHIM_EXPORT int jio_vsnprintf(char *str, size_t count, const char *fmt, va_list args) {
    if ((intptr_t)count <= 0) {
        return -1;
    }
    int result = vsnprintf(str, count, fmt, args);
    if (result < 0 || (size_t)result >= count) {
        str[count - 1] = '\0';
        result = -1;
    }
    return result;
}

SHIM_EXPORT int jio_snprintf(char *str, size_t count, const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    int result = jio_vsnprintf(str, count, fmt, args);
    va_end(args);
    return result;
}

SHIM_EXPORT int jio_vfprintf(FILE *f, const char *fmt, va_list args) {
    return vfprintf(f, fmt, args);
}

SHIM_EXPORT int jio_fprintf(FILE *f, const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    int result = jio_vfprintf(f, fmt, args);
    va_end(args);
    return result;
}

/* ---- raw monitors (libzip, libinstrument): plain recursive mutexes ---- */

SHIM_EXPORT void *JVM_RawMonitorCreate(void) {
#ifdef _WIN32
    CRITICAL_SECTION *cs = (CRITICAL_SECTION *)malloc(sizeof(CRITICAL_SECTION));
    if (cs != NULL) {
        InitializeCriticalSection(cs);
    }
    return cs;
#else
    pthread_mutex_t *m = (pthread_mutex_t *)malloc(sizeof(pthread_mutex_t));
    if (m != NULL) {
        pthread_mutexattr_t attr;
        pthread_mutexattr_init(&attr);
        pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
        pthread_mutex_init(m, &attr);
        pthread_mutexattr_destroy(&attr);
    }
    return m;
#endif
}

SHIM_EXPORT void JVM_RawMonitorDestroy(void *monitor) {
#ifdef _WIN32
    DeleteCriticalSection((CRITICAL_SECTION *)monitor);
#else
    pthread_mutex_destroy((pthread_mutex_t *)monitor);
#endif
    free(monitor);
}

SHIM_EXPORT int JVM_RawMonitorEnter(void *monitor) {
#ifdef _WIN32
    EnterCriticalSection((CRITICAL_SECTION *)monitor);
#else
    pthread_mutex_lock((pthread_mutex_t *)monitor);
#endif
    return 0;
}

SHIM_EXPORT void JVM_RawMonitorExit(void *monitor) {
#ifdef _WIN32
    LeaveCriticalSection((CRITICAL_SECTION *)monitor);
#else
    pthread_mutex_unlock((pthread_mutex_t *)monitor);
#endif
}

/* ---- small services that need no VM state ---- */

SHIM_EXPORT char *JVM_NativePath(char *path) { return path; }

SHIM_EXPORT unsigned char JVM_IsStaticallyLinked(void) { return 0; }

SHIM_EXPORT unsigned char JVM_IsContainerized(void) { return 0; }

SHIM_EXPORT unsigned char JVM_IsUseContainerSupport(void) { return 0; }

SHIM_EXPORT int64_t JVM_CurrentTimeMillis(void *env, void *ignored) {
    (void)env;
    (void)ignored;
#ifdef _WIN32
    FILETIME ft;
    GetSystemTimePreciseAsFileTime(&ft);
    uint64_t ticks = ((uint64_t)ft.dwHighDateTime << 32) | ft.dwLowDateTime;
    return (int64_t)((ticks - 116444736000000000ULL) / 10000ULL);
#else
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
#endif
}

SHIM_EXPORT int64_t JVM_NanoTime(void *env, void *ignored) {
    (void)env;
    (void)ignored;
#ifdef _WIN32
    LARGE_INTEGER frequency, counter;
    QueryPerformanceFrequency(&frequency);
    QueryPerformanceCounter(&counter);
    return (int64_t)((double)counter.QuadPart * (1000000000.0 / (double)frequency.QuadPart));
#else
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
#endif
}

SHIM_EXPORT int JVM_ActiveProcessorCount(void) {
#ifdef _WIN32
    SYSTEM_INFO info;
    GetSystemInfo(&info);
    return (int)info.dwNumberOfProcessors;
#else
    long count = sysconf(_SC_NPROCESSORS_ONLN);
    return count > 0 ? (int)count : 1;
#endif
}

SHIM_EXPORT unsigned char JVM_RaiseSignal(int sig) { return raise(sig) == 0; }

SHIM_EXPORT unsigned char JVM_IsSupportedJNIVersion(int version) {
    switch (version) {
    case 0x00010001: /* JNI_VERSION_1_1 */
    case 0x00010002:
    case 0x00010004:
    case 0x00010006:
    case 0x00010008:
    case 0x00090000:
    case 0x000a0000:
    case 0x00130000:
    case 0x00140000:
    case 0x00150000:
    case 0x00180000:
        return 1;
    default:
        return 0;
    }
}
