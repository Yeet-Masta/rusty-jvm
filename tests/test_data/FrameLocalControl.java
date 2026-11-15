package samples.interpreter.framelocal;

// Instructions the frame-local loop runs once warm: switches, float/double comparisons, the
// dup_x2 family, resolved ldc constants, and checkcast/instanceof at sites that have already
// checked the object's class. Each loop runs long enough for the sites to warm up.
public class FrameLocalControl {
    long wide;

    static int dense(int value) {
        switch (value) {
            case -2: return 10;
            case -1: return 11;
            case 0: return 12;
            case 1: return 13;
            case 2: return 14;
            case 3: return 15;
            default: return 99;
        }
    }

    static int sparse(int value) {
        switch (value) {
            case Integer.MIN_VALUE: return 1;
            case -1000: return 2;
            case 7: return 3;
            case 4096: return 4;
            case Integer.MAX_VALUE: return 5;
            default: return 0;
        }
    }

    static String describe(Object value) {
        if (value instanceof Integer) return "int";
        if (value instanceof String) return "str";
        if (value instanceof Number) return "num";
        return value == null ? "null" : "obj";
    }

    public static void main(String[] args) {
        StringBuilder out = new StringBuilder();
        int[] probes = {Integer.MIN_VALUE, -1000, -3, -2, -1, 0, 1, 2, 3, 4, 7, 4096, Integer.MAX_VALUE};
        for (int round = 0; round < 3; round++) {
            out.setLength(0);
            for (int probe : probes) {
                out.append(dense(probe)).append('/').append(sparse(probe)).append(' ');
            }
        }
        System.out.println("switch " + out.toString().trim());

        float[] floats = {1f, 2f, Float.NaN, -0f, 0f};
        double[] doubles = {1d, 2d, Double.NaN, -0d, 0d};
        out.setLength(0);
        for (int i = 0; i < floats.length; i++) {
            for (int j = 0; j < floats.length; j++) {
                float a = floats[i], b = floats[j];
                double c = doubles[i], d = doubles[j];
                out.append(a < b ? 'L' : a > b ? 'G' : a == b ? 'E' : 'N');
                out.append(c < d ? 'l' : c > d ? 'g' : c == d ? 'e' : 'n');
            }
        }
        System.out.println("compare " + out);

        int[] ints = new int[4];
        long[] longs = new long[4];
        FrameLocalControl holder = new FrameLocalControl();
        long acc = 0;
        for (int i = 0; i < 1000; i++) {
            int x = ints[i & 3] = i;          // dup_x2
            long y = holder.wide = i * 3L;    // dup2_x1
            long z = longs[i & 3] = -y;       // dup2_x2
            acc += x + y + z;
        }
        System.out.println("dups " + acc + " " + ints[3] + " " + holder.wide + " " + longs[3]);

        long constants = 0;
        Class<?> type = null;
        for (int i = 0; i < 1000; i++) {
            String s = "constant";
            constants += s.length() + 123456789L + (long) 2.5f + (long) 1e10;
            type = String.class;
        }
        System.out.println("ldc " + constants + " " + type.getName());

        Object[] values = {1, "s", 2.5, null, new Object(), 3, "t"};
        out.setLength(0);
        for (int round = 0; round < 50; round++) {
            out.setLength(0);
            for (Object value : values) {
                out.append(describe(value)).append(' ');
            }
        }
        System.out.println("instanceof " + out.toString().trim());

        Object[] numbers = {1, 2, 3};
        int sum = 0;
        for (int round = 0; round < 100; round++) {
            for (Object number : numbers) {
                sum += (Integer) number;
            }
        }
        Object notInteger = "x";
        try {
            sum += (Integer) notInteger;
            System.out.println("no exception");
        } catch (ClassCastException e) {
            System.out.println("cast " + sum + " CCE");
        }

        // A call that runs without a frame (an empty constructor here) must not leave its pc as
        // the frame's throw site: the next throw is caught by its own handler.
        Object made = new Object();
        try {
            int[] missing = args.length > 99 ? new int[1] : null;
            missing[0] = 2;
            System.out.println("no exception");
        } catch (NullPointerException e) {
            System.out.println("after frameless call NPE " + (made != null));
        }
    }
}
