package samples.management.threadinfo;

import java.lang.management.LockInfo;
import java.lang.management.ManagementFactory;
import java.lang.management.MonitorInfo;
import java.lang.management.ThreadInfo;
import java.lang.management.ThreadMXBean;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.locks.LockSupport;
import java.util.concurrent.locks.ReentrantLock;

public class ThreadInfoDemo {
    static final ThreadMXBean THREADS = ManagementFactory.getThreadMXBean();

    static class Gate {
    }

    static class Blocker {
    }

    static Thread daemon(String name, Runnable body) {
        Thread thread = new Thread(body, name);
        thread.setDaemon(true);
        thread.start();
        return thread;
    }

    static void await(Thread thread, Thread.State state) throws InterruptedException {
        while (thread.getState() != state) {
            Thread.sleep(1);
        }
    }

    static String lock(LockInfo lock) {
        return lock == null ? "null" : lock.getClassName();
    }

    static String frames(ThreadInfo info, int count) {
        List<String> names = new ArrayList<>();
        StackTraceElement[] trace = info.getStackTrace();
        for (int i = 0; i < Math.min(count, trace.length); i++) {
            names.add(trace[i].getClassName().replaceAll("0x[0-9a-fA-F]+", "0xID") + "." + trace[i].getMethodName());
        }
        return names.toString();
    }

    /** {@code counts}: whether the thread's contention counters are the same on every run. */
    static void describe(ThreadInfo info, int frames, boolean counts) {
        System.out.println(info.getThreadName() + ": state=" + info.getThreadState()
                + " lock=" + lock(info.getLockInfo())
                + " lockName=" + (info.getLockName() == null ? "null" : info.getLockName().replaceAll("@[0-9a-f]+", "@ID"))
                + " owner=" + info.getLockOwnerName()
                + " ownerId=" + (info.getLockOwnerId() == -1 ? "-1" : "set")
                + (counts ? " blocked=" + info.getBlockedCount() + " waited=" + info.getWaitedCount() : "")
                + " times=" + info.getBlockedTime() + "/" + info.getWaitedTime()
                + " native=" + info.isInNative() + " suspended=" + info.isSuspended()
                + " daemon=" + info.isDaemon() + " priority=" + info.getPriority());
        System.out.println("  top=" + frames(info, frames));
    }

    static void describeLocks(ThreadInfo info) {
        List<String> monitors = new ArrayList<>();
        for (MonitorInfo monitor : info.getLockedMonitors()) {
            StackTraceElement frame = monitor.getLockedStackFrame();
            monitors.add(monitor.getClassName() + "@" + monitor.getLockedStackDepth() + ":"
                    + (frame == null ? "null" : frame.getMethodName()));
        }
        List<String> synchronizers = new ArrayList<>();
        for (LockInfo synchronizer : info.getLockedSynchronizers()) {
            synchronizers.add(synchronizer.getClassName());
        }
        System.out.println(info.getThreadName() + ": monitors=" + monitors + " synchronizers=" + synchronizers
                + " depth=" + info.getStackTrace().length);
    }

    static void holdBoth(Object outer, Object inner, CountDownLatch held, CountDownLatch release) {
        synchronized (outer) {
            holdInner(inner, held, release);
        }
    }

    static synchronized void await(CountDownLatch latch) {
        try {
            latch.await();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    static void holdInner(Object inner, CountDownLatch held, CountDownLatch release) {
        synchronized (inner) {
            held.countDown();
            try {
                release.await();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }
    }

    public static void main(String[] args) throws Exception {
        Thread.currentThread().setName("main");
        System.out.println("supported: monitors=" + THREADS.isObjectMonitorUsageSupported()
                + " synchronizers=" + THREADS.isSynchronizerUsageSupported());

        Gate gate = new Gate();
        Object waitOn = new Blocker();
        ReentrantLock reentrant = new ReentrantLock();
        Blocker parkBlocker = new Blocker();

        Thread blocked;
        Thread lockWaiter;
        Thread waiting = daemon("waiting", () -> {
            synchronized (waitOn) {
                try {
                    waitOn.wait();
                } catch (InterruptedException e) {
                    // released by the test
                }
            }
        });
        Thread sleeping = daemon("sleeping", () -> {
            try {
                Thread.sleep(600_000);
            } catch (InterruptedException e) {
                // released by the test
            }
        });
        Thread parked = daemon("parked", () -> LockSupport.park(parkBlocker));
        Thread timedWait = daemon("timed-wait", () -> {
            synchronized (waitOn) {
                try {
                    waitOn.wait(600_000);
                } catch (InterruptedException e) {
                    // released by the test
                }
            }
        });

        reentrant.lock();
        synchronized (gate) {
            blocked = daemon("blocked", () -> {
                synchronized (gate) {
                    System.out.println("blocked thread got the gate");
                }
            });
            lockWaiter = daemon("lock-waiter", () -> {
                reentrant.lock();
                reentrant.unlock();
            });
            await(blocked, Thread.State.BLOCKED);
            await(lockWaiter, Thread.State.WAITING);
            await(waiting, Thread.State.WAITING);
            await(sleeping, Thread.State.TIMED_WAITING);
            await(parked, Thread.State.WAITING);
            await(timedWait, Thread.State.TIMED_WAITING);

            System.out.println("== getThreadInfo ==");
            long[] ids = {blocked.threadId(), lockWaiter.threadId(), waiting.threadId(), sleeping.threadId(),
                    parked.threadId(), timedWait.threadId(), Thread.currentThread().threadId()};
            for (ThreadInfo info : THREADS.getThreadInfo(ids, 3)) {
                // The main thread polls with sleeps, each of which counts as a wait.
                describe(info, 3, !info.getThreadName().equals("main"));
            }
            System.out.println("== depths ==");
            ThreadInfo none = THREADS.getThreadInfo(blocked.threadId());
            ThreadInfo one = THREADS.getThreadInfo(sleeping.threadId(), 1);
            ThreadInfo all = THREADS.getThreadInfo(sleeping.threadId(), Integer.MAX_VALUE);
            System.out.println("default depth=" + none.getStackTrace().length + " one=" + one.getStackTrace().length
                    + " all>1=" + (all.getStackTrace().length > 1)
                    + " bottom=" + all.getStackTrace()[all.getStackTrace().length - 1].getMethodName());
            ThreadInfo[] mixed = THREADS.getThreadInfo(new long[] {Long.MAX_VALUE - 7, blocked.threadId()});
            System.out.println("unknown id: " + THREADS.getThreadInfo(Long.MAX_VALUE - 7) + " " + mixed[0] + " "
                    + mixed[1].getThreadName());
            try {
                THREADS.getThreadInfo(0);
            } catch (IllegalArgumentException e) {
                System.out.println("bad id: " + e.getMessage());
            }

            System.out.println("== dumpAllThreads ==");
            CountDownLatch held = new CountDownLatch(1);
            CountDownLatch release = new CountDownLatch(1);
            Object outer = new Gate();
            Object inner = new Blocker();
            Thread holder = daemon("holder", () -> holdBoth(outer, inner, held, release));
            Thread syncMethod = daemon("sync-method", () -> await(release));
            held.await();
            await(holder, Thread.State.WAITING);
            await(syncMethod, Thread.State.WAITING);
            List<String> wanted = List.of("main", "blocked", "holder", "sync-method", "lock-waiter");
            ThreadInfo[] dump = THREADS.dumpAllThreads(true, true);
            Arrays.sort(dump, (a, b) -> a.getThreadName().compareTo(b.getThreadName()));
            for (ThreadInfo info : dump) {
                if (wanted.contains(info.getThreadName())) {
                    describeLocks(info);
                }
            }
            System.out.println("dump has every live thread: " + (dump.length >= 9));
            ThreadInfo[] plain = THREADS.dumpAllThreads(false, false, 2);
            int deepest = 0;
            for (ThreadInfo info : plain) {
                deepest = Math.max(deepest, info.getStackTrace().length);
                if (info.getLockedMonitors().length != 0 || info.getLockedSynchronizers().length != 0) {
                    System.out.println("unexpected lock details for " + info.getThreadName());
                }
            }
            System.out.println("limited depth: " + deepest);
            ThreadInfo[] some = THREADS.getThreadInfo(new long[] {holder.threadId(), Long.MAX_VALUE - 7}, true, false);
            System.out.println("selected: " + some[0].getLockedMonitors().length + " " + some[1]);
            System.out.println("toString: " + some[0].toString().replaceAll("Id=\\d+", "Id=N").replaceAll("@[0-9a-f]+", "@ID")
                    .replace("\r", "").split("\n\tat java.base")[0]);
            release.countDown();

            System.out.println("== deadlocks ==");
            System.out.println("none: " + Arrays.toString(THREADS.findDeadlockedThreads()) + " "
                    + Arrays.toString(THREADS.findMonitorDeadlockedThreads()));
        }
        reentrant.unlock();
        blocked.join();
        lockWaiter.join();

        Object first = new Gate();
        Object second = new Blocker();
        CountDownLatch bothHold = new CountDownLatch(2);
        Thread monitorA = daemon("monitor-a", () -> deadlock(first, second, bothHold));
        Thread monitorB = daemon("monitor-b", () -> deadlock(second, first, bothHold));
        await(monitorA, Thread.State.BLOCKED);
        await(monitorB, Thread.State.BLOCKED);
        System.out.println("monitor cycle: " + names(THREADS.findMonitorDeadlockedThreads()) + " all: "
                + names(THREADS.findDeadlockedThreads()));
        for (ThreadInfo info : THREADS.getThreadInfo(new long[] {monitorA.threadId(), monitorB.threadId()}, 1)) {
            describe(info, 1, false);
        }

        ReentrantLock lockA = new ReentrantLock();
        ReentrantLock lockB = new ReentrantLock();
        CountDownLatch locksHeld = new CountDownLatch(2);
        Thread ownerA = daemon("lock-a", () -> deadlock(lockA, lockB, locksHeld));
        Thread ownerB = daemon("lock-b", () -> deadlock(lockB, lockA, locksHeld));
        // Each is also WAITING earlier, on the latch: wait until it queues for the other's lock.
        while (!lockB.hasQueuedThread(ownerA) || !lockA.hasQueuedThread(ownerB)) {
            Thread.sleep(1);
        }
        await(ownerA, Thread.State.WAITING);
        await(ownerB, Thread.State.WAITING);
        System.out.println("monitor cycle: " + names(THREADS.findMonitorDeadlockedThreads()) + " all: "
                + names(THREADS.findDeadlockedThreads()));
        for (ThreadInfo info : THREADS.getThreadInfo(new long[] {ownerA.threadId(), ownerB.threadId()}, true, true)) {
            System.out.println(info.getThreadName() + ": state=" + info.getThreadState() + " lock=" + lock(info.getLockInfo())
                    + " owner=" + info.getLockOwnerName() + " synchronizers=" + info.getLockedSynchronizers().length);
        }
        waiting.interrupt();
        sleeping.interrupt();
        timedWait.interrupt();
        LockSupport.unpark(parked);
        waiting.join();
        sleeping.join();
        timedWait.join();
        parked.join();
        System.out.println("after termination: " + THREADS.getThreadInfo(waiting.threadId()));
    }

    static List<String> names(long[] ids) {
        if (ids == null) {
            return null;
        }
        List<String> names = new ArrayList<>();
        for (ThreadInfo info : THREADS.getThreadInfo(ids)) {
            names.add(info.getThreadName());
        }
        names.sort(null);
        return names;
    }

    static void deadlock(Object mine, Object theirs, CountDownLatch bothHold) {
        synchronized (mine) {
            bothHold.countDown();
            try {
                bothHold.await();
            } catch (InterruptedException e) {
                return;
            }
            synchronized (theirs) {
                System.out.println("unreachable");
            }
        }
    }

    static void deadlock(ReentrantLock mine, ReentrantLock theirs, CountDownLatch bothHold) {
        mine.lock();
        bothHold.countDown();
        try {
            bothHold.await();
        } catch (InterruptedException e) {
            return;
        }
        theirs.lock();
        System.out.println("unreachable");
    }
}
