/** Boolean and int array stores and loads in simple counted loops. */
public final class Sieve implements Bench.Workload {
    public long run(int n) {
        boolean[] composite = new boolean[n + 1];
        long sum = 0;
        int count = 0;
        for (int i = 2; i <= n; i++) {
            if (!composite[i]) {
                count++;
                sum += i;
                for (long j = (long) i * i; j <= n; j += i) {
                    composite[(int) j] = true;
                }
            }
        }
        return sum * 31 + count;
    }

    public static void main(String[] args) {
        Bench.main(args, new Sieve());
    }
}
