package samples.concurrency.monitorcontention;

import java.util.ArrayDeque;

public class MonitorContentionExample {
    private static final Object LOCK = new Object();
    private static long counter;

    static final class BoundedBuffer {
        private final ArrayDeque<Integer> items = new ArrayDeque<>();
        private final int capacity;

        BoundedBuffer(int capacity) {
            this.capacity = capacity;
        }

        synchronized void put(int value) throws InterruptedException {
            while (items.size() == capacity) {
                wait();
            }
            items.addLast(value);
            // Mix notify and notifyAll so both wake paths run with waiters present.
            if ((value & 1) == 0) {
                notify();
            } else {
                notifyAll();
            }
        }

        synchronized int take() throws InterruptedException {
            while (items.isEmpty()) {
                wait();
            }
            int value = items.removeFirst();
            notifyAll();
            return value;
        }
    }

    private static int reentrantDepth(Object lock, int depth) {
        synchronized (lock) {
            return depth == 0 ? 0 : 1 + reentrantDepth(lock, depth - 1);
        }
    }

    public static void main(String[] args) throws Exception {
        // Contended counter: every increment must be observed exactly once.
        int threads = 8;
        int perThread = 20_000;
        Thread[] workers = new Thread[threads];
        for (int i = 0; i < threads; i++) {
            workers[i] = new Thread(() -> {
                for (int j = 0; j < perThread; j++) {
                    synchronized (LOCK) {
                        counter++;
                    }
                }
            });
            workers[i].start();
        }
        for (Thread worker : workers) {
            worker.join();
        }
        System.out.println("counter: " + counter + " (expected " + (long) threads * perThread + ")");

        // Producers and consumers blocking on a tiny buffer: entrants and wait-set threads share
        // the same monitor, so a lost wakeup would hang or lose items.
        BoundedBuffer buffer = new BoundedBuffer(2);
        int producers = 4;
        int consumers = 4;
        int perProducer = 2_000;
        long[] consumed = new long[consumers];
        Thread[] all = new Thread[producers + consumers];
        for (int p = 0; p < producers; p++) {
            int base = p * perProducer;
            all[p] = new Thread(() -> {
                try {
                    for (int k = 0; k < perProducer; k++) {
                        buffer.put(base + k);
                    }
                } catch (InterruptedException e) {
                    throw new RuntimeException(e);
                }
            });
        }
        int perConsumer = producers * perProducer / consumers;
        for (int c = 0; c < consumers; c++) {
            int index = c;
            all[producers + c] = new Thread(() -> {
                try {
                    long sum = 0;
                    for (int k = 0; k < perConsumer; k++) {
                        sum += buffer.take();
                    }
                    consumed[index] = sum;
                } catch (InterruptedException e) {
                    throw new RuntimeException(e);
                }
            });
        }
        for (Thread t : all) {
            t.start();
        }
        for (Thread t : all) {
            t.join();
        }
        long total = 0;
        for (long sum : consumed) {
            total += sum;
        }
        long n = (long) producers * perProducer;
        System.out.println("consumed sum: " + total + " (expected " + (n * (n - 1) / 2) + ")");

        System.out.println("reentrant depth: " + reentrantDepth(LOCK, 50));
    }
}
