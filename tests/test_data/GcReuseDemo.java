package samples.gc;

import java.lang.invoke.MethodType;
import java.lang.ref.WeakReference;

// References of collected objects are issued again. Nothing that outlives an object may then see
// the object that takes its reference: a live object keeps its contents and identity hash, a weak
// reference to a collected object is cleared, and a cached MethodType is not mistaken for another.
public final class GcReuseDemo {
    static final class Box {
        final int id;
        final long check;

        Box(int id) {
            this.id = id;
            this.check = id * 31L + 7;
        }
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    public static void main(String[] args) {
        Box[] live = new Box[64];
        int[] hashes = new int[live.length];
        for (int i = 0; i < live.length; i++) {
            live[i] = new Box(i);
            hashes[i] = System.identityHashCode(live[i]);
        }
        int cleared = 0;
        for (int round = 0; round < 200; round++) {
            WeakReference<Box> weak = new WeakReference<>(new Box(-1 - round));
            MethodType type = MethodType.methodType(round % 2 == 0 ? int.class : long.class, String.class);
            // Garbage, so collections run and free references for reuse.
            for (int i = 0; i < 500; i++) {
                Box garbage = new Box(1_000_000 + i);
                require(garbage.check == garbage.id * 31L + 7, "fresh object corrupt");
            }
            Box referent = weak.get();
            if (referent == null) {
                cleared++;
            } else {
                require(referent.id == -1 - round, "weak reference sees another object");
            }
            require(type.returnType() == (round % 2 == 0 ? int.class : long.class), "method type changed");
            require(type.parameterType(0) == String.class, "method type parameters changed");
            for (int i = 0; i < live.length; i++) {
                require(live[i].id == i && live[i].check == i * 31L + 7, "live object changed");
                require(System.identityHashCode(live[i]) == hashes[i], "identity hash changed");
            }
            live[round % live.length] = new Box(round % live.length);
            hashes[round % live.length] = System.identityHashCode(live[round % live.length]);
        }
        require(cleared > 0, "no weak reference was cleared");
        System.out.println("reuse-ok");
    }
}
