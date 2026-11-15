/** Interface and virtual calls at monomorphic, bimorphic and megamorphic sites. */
public final class Dispatch implements Bench.Workload {
    interface Shape {
        long area(int k);
    }

    static final class Square implements Shape {
        public long area(int k) { return (long) k * k; }
    }

    static final class Rect implements Shape {
        public long area(int k) { return (long) k * (k + 1); }
    }

    static final class Tri implements Shape {
        public long area(int k) { return (long) k * k / 2; }
    }

    static final class Circle implements Shape {
        public long area(int k) { return (long) k * k * 3; }
    }

    static long sum(Shape[] shapes, int n) {
        long total = 0;
        for (int i = 0; i < n; i++) {
            total += shapes[i & (shapes.length - 1)].area(i & 1023);
        }
        return total;
    }

    public long run(int n) {
        Shape[] mono = {new Square(), new Square(), new Square(), new Square()};
        Shape[] bi = {new Square(), new Rect(), new Square(), new Rect()};
        Shape[] mega = {new Square(), new Rect(), new Tri(), new Circle()};
        return sum(mono, n) + 3 * sum(bi, n) + 7 * sum(mega, n);
    }

    public static void main(String[] args) {
        Bench.main(args, new Dispatch());
    }
}
