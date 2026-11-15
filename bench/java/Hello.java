/** Startup: VM boot, class loading and one println. Run with one iteration. */
public final class Hello implements Bench.Workload {
    public long run(int size) {
        System.out.println("Hello, world");
        return size;
    }

    public static void main(String[] args) {
        Bench.main(args, new Hello());
    }
}
