package samples.jni.invocation;

/**
 * The JNI invocation interface: a library's JNI_OnLoad using GetEnv and RegisterNatives, and
 * native threads that attach to the VM, call into Java and detach, with the error codes these
 * functions return.
 */
public class JniInvocationDemo {
    static {
        System.loadLibrary("jni_test_lib");
    }

    /** What the library's JNI_OnLoad observed. */
    private static native String onLoadReport();

    /** Bound by RegisterNatives in JNI_OnLoad, not by its JNI symbol name. */
    private static native int registeredByOnLoad(int value);

    /** GetEnv, AttachCurrentThread and DetachCurrentThread on this Java thread. */
    private static native String javaThreadReport();

    /** Runs a native thread that attaches, calls {@link #callback} and detaches. */
    private static native String attachNativeThread(boolean daemon, String name);

    private static volatile String calledBack;

    static void callback(String message) {
        Thread current = Thread.currentThread();
        calledBack = message + " on " + current.getName()
                + " daemon=" + current.isDaemon()
                + " group=" + current.getThreadGroup().getName()
                + " alive=" + current.isAlive()
                + " virtual=" + current.isVirtual();
    }

    public static void main(String[] args) {
        System.out.println(onLoadReport());
        System.out.println("registered: " + registeredByOnLoad(20));
        System.out.println(javaThreadReport());
        System.out.println(attachNativeThread(false, "native-worker"));
        System.out.println(calledBack);
        System.out.println(attachNativeThread(true, null));
        System.out.println(calledBack);
    }
}
