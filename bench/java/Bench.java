/**
 * Shared driver for the benchmark programs.
 *
 * <p>Every benchmark is run as {@code <Main> <iterations> <size>}. It runs its workload
 * {@code iterations} times and prints, for each run, a line {@code iter <index> <nanoseconds>},
 * then a final line {@code checksum <value>}. The runner compares checksums across VMs, so a
 * workload must compute the same value on every VM and every iteration.
 */
public final class Bench {
    public interface Workload {
        long run(int size);
    }

    private Bench() {}

    public static void main(String[] args, Workload workload) {
        int iterations = args.length > 0 ? Integer.parseInt(args[0]) : 5;
        int size = args.length > 1 ? Integer.parseInt(args[1]) : 1;
        long checksum = 0;
        for (int i = 0; i < iterations; i++) {
            long start = System.nanoTime();
            long result = workload.run(size);
            long elapsed = System.nanoTime() - start;
            System.out.println("iter " + i + " " + elapsed);
            if (i == 0) {
                checksum = result;
            } else if (result != checksum) {
                System.out.println("unstable " + i + " " + result + " " + checksum);
            }
        }
        System.out.println("checksum " + checksum);
    }
}
