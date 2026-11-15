/** Recursive calls: invocation and return overhead. */
public final class Fib implements Bench.Workload {
    static int fib(int n) {
        return n < 2 ? n : fib(n - 1) + fib(n - 2);
    }

    public long run(int size) {
        return fib(size);
    }

    public static void main(String[] args) {
        Bench.main(args, new Fib());
    }
}
