package samples.loader.isolation;

import java.io.IOException;
import java.io.InputStream;
import java.io.UncheckedIOException;
import java.lang.invoke.MethodHandles;
import java.lang.reflect.Array;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.file.Path;
import java.util.function.Supplier;

public class LoaderIsolation {
    static final String PREFIX = "samples.loader.isolation.LoaderIsolation$Iso";
    static final String COUNTER = PREFIX + "Counter";
    static final String SHARED = "samples.loader.isolation.LoaderIsolation$Shared";

    /** Never matches {@link #PREFIX}, so every delegating loader sees the application's copy. */
    public interface Shared {
        int bump();

        String describe();
    }

    public static class IsoException extends RuntimeException {
        public IsoException(String message) {
            super(message);
        }
    }

    public static class IsoCounter implements Shared {
        static int count;

        @Override
        public int bump() {
            return ++count;
        }

        @Override
        public String describe() {
            Supplier<String> loader = () -> String.valueOf(IsoCounter.class.getClassLoader().getName());
            IsoCounter[] array = new IsoCounter[1];
            array[0] = this;
            Object self = array[0];
            String caught;
            try {
                throw new IsoException("own exception");
            } catch (IsoException e) {
                caught = e.getMessage() + " from " + e.getClass().getClassLoader().getName();
            }
            return "loader=" + loader.get()
                    + " array=" + array.getClass().getName()
                    + " instanceof=" + (self instanceof IsoCounter)
                    + " lambdaHidden=" + loader.getClass().isHidden()
                    + " lambdaLoader=" + loader.getClass().getClassLoader().getName()
                    + " caught=" + caught;
        }
    }

    static class Hid {
        static String hello() {
            return "hello from hidden";
        }
    }

    /** Defines the {@code Iso*} classes itself and delegates everything else to its parent. */
    static class IsolatingLoader extends ClassLoader {
        IsolatingLoader(String name, ClassLoader parent) {
            super(name, parent);
        }

        @Override
        protected Class<?> loadClass(String name, boolean resolve) throws ClassNotFoundException {
            synchronized (getClassLoadingLock(name)) {
                if (name.startsWith(PREFIX)) {
                    Class<?> loaded = findLoadedClass(name);
                    return loaded != null ? loaded : define(name);
                }
                return super.loadClass(name, resolve);
            }
        }

        Class<?> define(String name) {
            return defineAs(name, name);
        }

        Class<?> defineAs(String name, String bytesOf) {
            byte[] bytes = classBytes(bytesOf);
            return defineClass(name, bytes, 0, bytes.length);
        }

        boolean hasLoaded(String name) {
            return findLoadedClass(name) != null;
        }
    }

    static byte[] classBytes(String name) {
        String resource = name.substring(name.lastIndexOf('.') + 1) + ".class";
        try (InputStream in = LoaderIsolation.class.getResourceAsStream(resource)) {
            return in.readAllBytes();
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    static String stable(String message) {
        return message.replaceAll("@[0-9a-f]+", "@ID").replaceAll("0x[0-9a-fA-F]+", "0xID");
    }

    public static void main(String[] args) throws Exception {
        ClassLoader app = LoaderIsolation.class.getClassLoader();
        IsolatingLoader loaderA = new IsolatingLoader("isoA", app);
        IsolatingLoader loaderB = new IsolatingLoader("isoB", app);

        Class<?> appCounter = IsoCounter.class;
        Class<?> counterA = loaderA.loadClass(COUNTER);
        Class<?> counterB = loaderB.loadClass(COUNTER);

        System.out.println("== isolation ==");
        System.out.println("distinct classes: " + (counterA != counterB && counterA != appCounter && counterB != appCounter));
        System.out.println("same name: " + counterA.getName().equals(appCounter.getName()) + " " + counterA.getName());
        System.out.println("loaders: " + counterA.getClassLoader().getName() + " " + counterB.getClassLoader().getName()
                + " " + appCounter.getClassLoader().getName());
        System.out.println("loadClass is stable: " + (loaderA.loadClass(COUNTER) == counterA));
        System.out.println("forName through loader: " + (Class.forName(COUNTER, true, loaderA) == counterA));

        Shared a = (Shared) counterA.getDeclaredConstructor().newInstance();
        Shared b = (Shared) counterB.getDeclaredConstructor().newInstance();
        Shared mine = new IsoCounter();
        a.bump();
        a.bump();
        a.bump();
        b.bump();
        System.out.println("static state: a=" + a.bump() + " b=" + b.bump() + " app=" + mine.bump());
        System.out.println("shared interface: " + (counterA.getInterfaces()[0] == Shared.class));
        System.out.println("superclass: " + counterA.getSuperclass().getName());
        System.out.println("instanceof app copy: " + (a instanceof IsoCounter) + " " + appCounter.isInstance(a)
                + " " + counterA.isInstance(a) + " " + counterB.isInstance(a));
        try {
            IsoCounter cast = (IsoCounter) a;
            System.out.println("cast succeeded: " + cast);
        } catch (ClassCastException e) {
            System.out.println("cast: " + stable(e.getMessage()));
        }
        System.out.println("a: " + a.describe());
        System.out.println("b: " + b.describe());
        System.out.println("app: " + mine.describe());

        System.out.println("== arrays, modules, nests ==");
        Class<?> arrayA = Array.newInstance(counterA, 2).getClass();
        System.out.println("array: " + arrayA.getName() + " loader=" + arrayA.getClassLoader().getName()
                + " component=" + (arrayA.getComponentType() == counterA) + " distinct=" + (arrayA != IsoCounter[].class));
        System.out.println("array forName: " + (Class.forName("[L" + COUNTER + ";", false, loaderA) == arrayA));
        System.out.println("module: " + (counterA.getModule() == loaderA.getUnnamedModule()) + " "
                + (counterA.getModule() != appCounter.getModule()) + " named=" + counterA.getModule().isNamed());
        System.out.println("array module: " + (arrayA.getModule() == counterA.getModule()));
        System.out.println("nest host: app=" + appCounter.getNestHost().getSimpleName()
                + " isolated is own host=" + (counterA.getNestHost() == counterA));
        System.out.println("simple name: " + appCounter.getSimpleName() + " declaring="
                + appCounter.getDeclaringClass().getSimpleName());
        try {
            // The isolated copy's outer class resolves to the application's, whose own inner
            // class of that name is a different class.
            System.out.println("isolated declaring: " + counterA.getDeclaringClass());
        } catch (IncompatibleClassChangeError e) {
            System.out.println(e.getClass().getName() + ": " + e.getMessage());
        }
        System.out.println("method types: " + counterA.getMethod("describe").getReturnType().getName());

        System.out.println("== findLoadedClass ==");
        System.out.println("defined: " + loaderA.hasLoaded(COUNTER));
        System.out.println("initiated: " + loaderA.hasLoaded(SHARED));
        System.out.println("untouched: " + loaderA.hasLoaded("java.util.BitSet"));

        System.out.println("== duplicate definition ==");
        try {
            loaderA.define(COUNTER);
            System.out.println("no error");
        } catch (LinkageError e) {
            System.out.println(e.getClass().getName() + ": " + stable(e.getMessage()));
        }
        try {
            loaderA.defineAs("some.other.Name", COUNTER);
            System.out.println("no error");
        } catch (NoClassDefFoundError e) {
            System.out.println(e.getClass().getName() + ": " + e.getMessage());
        }

        System.out.println("== null parent ==");
        ClassLoader orphan = new ClassLoader(null) {
            @Override
            protected Class<?> findClass(String name) throws ClassNotFoundException {
                if (name.startsWith("samples.loader.isolation.")) {
                    byte[] bytes = classBytes(name);
                    return defineClass(name, bytes, 0, bytes.length);
                }
                throw new ClassNotFoundException(name);
            }
        };
        Class<?> orphanCounter = orphan.loadClass(COUNTER);
        System.out.println("orphan: " + (orphanCounter.getClassLoader() == orphan) + " "
                + (orphanCounter != appCounter));
        Class<?> orphanShared = orphanCounter.getInterfaces()[0];
        System.out.println("orphan interface: " + orphanShared.getName() + " own copy=" + (orphanShared != Shared.class)
                + " loader=" + (orphanShared.getClassLoader() == orphan));
        Object orphanInstance = orphanCounter.getDeclaredConstructor().newInstance();
        System.out.println("orphan instance: " + (orphanInstance instanceof Shared) + " " + orphanShared.isInstance(orphanInstance));
        System.out.println("orphan describe: " + orphanShared.getMethod("describe").invoke(orphanInstance));
        System.out.println("orphan sees bootstrap: " + (orphan.loadClass("java.lang.String") == String.class));
        try {
            orphan.loadClass("no.such.Type");
        } catch (ClassNotFoundException e) {
            System.out.println("orphan miss: " + e.getMessage());
        }

        URL classes = Path.of(System.getProperty("java.class.path").split(java.io.File.pathSeparator)[0]).toUri().toURL();
        try (URLClassLoader urls = new URLClassLoader(new URL[] {classes}, null)) {
            Class<?> urlCounter = urls.loadClass(COUNTER);
            System.out.println("url loader: " + (urlCounter.getClassLoader() == urls) + " " + (urlCounter != appCounter)
                    + " " + (urls.loadClass(COUNTER) == urlCounter));
            System.out.println("url interface: " + (urlCounter.getInterfaces()[0].getClassLoader() == urls));
            System.out.println("url platform class: " + (urls.loadClass("java.util.ArrayList").getClassLoader() == null));
        }
        try {
            Class.forName(COUNTER, false, null);
            System.out.println("bootstrap found an application class");
        } catch (ClassNotFoundException e) {
            System.out.println("bootstrap: " + e.getMessage());
        }

        System.out.println("== hidden ==");
        MethodHandles.Lookup lookup = MethodHandles.lookup();
        byte[] hidBytes = classBytes("samples.loader.isolation.LoaderIsolation$Hid");
        Class<?> hidden = lookup.defineHiddenClass(hidBytes, true).lookupClass();
        Class<?> nestmate = lookup.defineHiddenClass(hidBytes, true, MethodHandles.Lookup.ClassOption.NESTMATE).lookupClass();
        Runnable lambda = () -> { };
        System.out.println("isHidden: " + hidden.isHidden() + " " + nestmate.isHidden() + " " + lambda.getClass().isHidden()
                + " " + Hid.class.isHidden() + " " + LoaderIsolation.class.isHidden() + " " + int[].class.isHidden());
        System.out.println("name: " + stable(hidden.getName()) + " distinct=" + (hidden != nestmate && hidden != Hid.class));
        System.out.println("array name: " + stable(Array.newInstance(hidden, 0).getClass().getName()));
        System.out.println("loader and module: " + (hidden.getClassLoader() == app) + " "
                + (hidden.getModule() == LoaderIsolation.class.getModule()));
        System.out.println("nest host: plain is own=" + (hidden.getNestHost() == hidden) + " nestmate="
                + nestmate.getNestHost().getSimpleName());
        var hello = hidden.getDeclaredMethod("hello");
        hello.setAccessible(true);
        System.out.println("invoke: " + hello.invoke(null));
        try {
            Class.forName(hidden.getName());
            System.out.println("hidden class found by name");
        } catch (ClassNotFoundException e) {
            System.out.println("not discoverable: " + stable(e.getMessage()));
        }
    }
}
