package samples.interpreter.framelocal;

// Calls and returns the frame-local loop does itself once a site is warm: static, special,
// virtual and interface invokes and every return kind. Exceptions thrown in such callees must
// reach the caller's handler with the right stack trace, deep recursion must still end in a
// catchable StackOverflowError, and class initialization and monitors must still happen.
public class FrameLocalCalls {
    interface Shape {
        long area();
    }

    static final class Square implements Shape {
        final int side;
        Square(int side) { this.side = side; }
        public long area() { return (long) side * side; }
    }

    static final class Rect implements Shape {
        final int w, h;
        Rect(int w, int h) { this.w = w; this.h = h; }
        public long area() { return (long) w * h; }
    }

    static class Base {
        int value(int x) { return x + 1; }
        private int hidden(int x) { return x * 2; }
        int callHidden(int x) { return hidden(x); }
    }

    static class Derived extends Base {
        @Override
        int value(int x) { return super.value(x) * 10; }
    }

    static class Lazy {
        static final StringBuilder LOG = new StringBuilder();
        static int counter;
        static {
            LOG.append("init;");
        }
        static int next() { return ++counter; }
    }

    static int add(int a, int b) { return a + b; }
    static long addLong(long a, long b) { return a + b; }
    static float half(float f) { return f / 2; }
    static double twice(double d) { return d * 2; }
    static Object pick(Object a, Object b, boolean first) { return first ? a : b; }
    static void nothing() { }

    static int depth;

    static int recurse(int n) {
        depth = n;
        return recurse(n + 1) + 1;
    }

    static int fails(int n) {
        if (n == 1000) {
            throw new IllegalStateException("at " + n);
        }
        return n;
    }

    static int callsFails(int n) { return fails(n) + 1; }

    static int counter;

    static synchronized void bump() { counter++; }

    public static void main(String[] args) throws Exception {
        long sum = 0;
        float f = 0;
        double d = 0;
        Object last = null;
        for (int i = 0; i < 2000; i++) {
            sum = addLong(sum, add(i, 1));
            f += half(i);
            d += twice(i);
            last = pick("a", "b", (i & 1) == 0);
            nothing();
        }
        System.out.println("statics " + sum + " " + f + " " + d + " " + last);

        Shape[] shapes = {new Square(3), new Rect(2, 5), new Square(4), new Rect(1, 1)};
        long areas = 0;
        for (int i = 0; i < 1000; i++) {
            areas += shapes[i & 3].area();
        }
        System.out.println("interface " + areas);

        Base[] bases = {new Base(), new Derived()};
        long values = 0;
        for (int i = 0; i < 1000; i++) {
            values += bases[i & 1].value(i) + bases[i & 1].callHidden(i);
        }
        System.out.println("virtual " + values);

        System.out.println("before " + Lazy.LOG);
        int lazy = 0;
        for (int i = 0; i < 100; i++) {
            lazy += Lazy.next();
        }
        System.out.println("lazy " + lazy + " " + Lazy.LOG);

        int caught = 0;
        for (int i = 990; i < 1010; i++) {
            try {
                callsFails(i);
            } catch (IllegalStateException e) {
                caught++;
                StackTraceElement[] trace = e.getStackTrace();
                System.out.println("exception " + e.getMessage() + " " + trace[0].getMethodName() + ":"
                        + trace[0].getLineNumber() + " " + trace[1].getMethodName() + ":"
                        + trace[1].getLineNumber() + " " + trace[2].getMethodName());
            }
        }
        System.out.println("caught " + caught);

        try {
            recurse(0);
        } catch (StackOverflowError e) {
            System.out.println("overflow " + (depth > 500));
        }

        Thread[] threads = new Thread[4];
        for (int t = 0; t < threads.length; t++) {
            threads[t] = new Thread(() -> {
                for (int i = 0; i < 2000; i++) {
                    bump();
                }
            });
            threads[t].start();
        }
        for (Thread thread : threads) {
            thread.join();
        }
        System.out.println("synchronized " + counter);
    }
}
