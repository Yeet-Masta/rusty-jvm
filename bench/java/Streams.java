import java.util.stream.Collectors;
import java.util.stream.IntStream;

/** Lambdas and streams: invokedynamic call sites, megamorphic functional interfaces. */
public final class Streams implements Bench.Workload {
    public long run(int n) {
        long a = IntStream.range(0, n)
                .filter(i -> i % 3 != 0)
                .mapToLong(i -> (long) i * i)
                .sum();
        long b = IntStream.range(0, n / 10)
                .boxed()
                .collect(Collectors.groupingBy(i -> i % 17, Collectors.counting()))
                .values()
                .stream()
                .mapToLong(Long::longValue)
                .reduce(0, (x, y) -> x * 31 + y);
        return a ^ b;
    }

    public static void main(String[] args) {
        Bench.main(args, new Streams());
    }
}
