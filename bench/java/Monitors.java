/** Uncontended and contended synchronized blocks across platform threads. */
public final class Monitors implements Bench.Workload {
    private long counter;

    private synchronized void increment() {
        counter++;
    }

    public long run(int n) {
        counter = 0;
        for (int i = 0; i < n; i++) {
            increment();
        }
        Thread[] threads = new Thread[4];
        for (int t = 0; t < threads.length; t++) {
            threads[t] = new Thread(new Runnable() {
                public void run() {
                    for (int i = 0; i < n / 4; i++) {
                        increment();
                    }
                }
            });
            threads[t].start();
        }
        for (Thread thread : threads) {
            try {
                thread.join();
            } catch (InterruptedException e) {
                throw new RuntimeException(e);
            }
        }
        return counter;
    }

    public static void main(String[] args) {
        Bench.main(args, new Monitors());
    }
}
