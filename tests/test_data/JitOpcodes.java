package samples.jit.opcodes;

// Every instruction the baseline compiler turns into machine code, on edge values, in methods
// that run often enough to be compiled. Each result is mixed into a hash as raw bits, and the
// hash is printed once per round: the first rounds run interpreted, the later ones compiled, so
// all the lines of a group are equal only if both agree, and equal to HotSpot's only if both are
// right.
public class JitOpcodes {
    static final int[] INTS = {
        0, 1, -1, 2, -2, 5, 7, -7, 31, 32, 33, 63, 64, 65, 127, 128, 255, 256, 32767, 32768,
        65535, 65536, -128, -129, -32768, -32769, 0x55555555, 0xaaaaaaaa, Integer.MAX_VALUE,
        Integer.MIN_VALUE, Integer.MIN_VALUE + 1, 1000000007
    };
    static final long[] LONGS = {
        0L, 1L, -1L, 2L, -2L, 7L, -7L, 31L, 32L, 63L, 64L, 65L, 0xffffffffL, 0x100000000L,
        -0x100000000L, 0x80000000L, -0x80000001L, 0x5555555555555555L, 0xaaaaaaaaaaaaaaaaL,
        Long.MAX_VALUE, Long.MIN_VALUE, Long.MIN_VALUE + 1, 1000000007L * 1000000009L
    };
    static final float[] FLOATS = {
        0.0f, -0.0f, 1.0f, -1.0f, 0.5f, -0.5f, 1.5f, 2.5f, -2.5f, 3.0f, 1e10f, -1e10f, 1e20f,
        -1e20f, 2147483647.0f, -2147483648.0f, 2147483904.0f, 9.223372e18f, -9.223372e18f,
        16777217.0f, Float.MIN_VALUE, -Float.MIN_VALUE, Float.MIN_NORMAL, Float.MAX_VALUE,
        -Float.MAX_VALUE, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY, Float.NaN,
        Float.intBitsToFloat(0x7fc00123), Float.intBitsToFloat(0xffc00000),
        Float.intBitsToFloat(0x7f800123)
    };
    static final double[] DOUBLES = {
        0.0, -0.0, 1.0, -1.0, 0.5, -0.5, 1.5, 2.5, -2.5, 3.0, 1e10, -1e10, 1e20, -1e20, 1e300,
        -1e300, 2147483647.0, 2147483648.0, -2147483648.0, -2147483649.0, 2147483647.5,
        9.223372036854775807e18, -9.223372036854775808e18, 9007199254740993.0, 1e-310,
        Double.MIN_VALUE, -Double.MIN_VALUE, Double.MIN_NORMAL, Double.MAX_VALUE,
        -Double.MAX_VALUE, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, Double.NaN,
        Double.longBitsToDouble(0x7ff8000000000123L), Double.longBitsToDouble(0xfff8000000000000L),
        Double.longBitsToDouble(0x7ff0000000000123L), 3.4028235e38, 3.5e38, 1e-46
    };
    static final Object[] REFS = {null, "a", "b", new Object(), new int[0]};

    static final int ROUNDS = 4;

    static long h = 17;

    static void mix(long bits) {
        h = h * 31 + bits;
    }

    static void f(float v) {
        mix(Float.floatToRawIntBits(v));
    }

    static void d(double v) {
        mix(Double.doubleToRawLongBits(v));
    }

    // iadd .. ixor, ineg, iinc, i2l .. i2s, the int branches, idiv and irem.
    static void ints(int a, int b) {
        mix(a + b);
        mix(a - b);
        mix(a * b);
        mix(a << b);
        mix(a >> b);
        mix(a >>> b);
        mix(a & b);
        mix(a | b);
        mix(a ^ b);
        mix(-a);
        mix(~a);
        int c = a;
        c++;
        c += 127;
        c -= 128;
        c += 1000;
        c -= 40000;
        mix(c);
        mix((long) a);
        f((float) a);
        d((double) a);
        mix((byte) a);
        mix((char) a);
        mix((short) a);
        int bits = 0;
        if (a == 0) bits |= 1;
        if (a != 0) bits |= 2;
        if (a < 0) bits |= 4;
        if (a >= 0) bits |= 8;
        if (a > 0) bits |= 16;
        if (a <= 0) bits |= 32;
        if (a == b) bits |= 64;
        if (a != b) bits |= 128;
        if (a < b) bits |= 256;
        if (a >= b) bits |= 512;
        if (a > b) bits |= 1024;
        if (a <= b) bits |= 2048;
        mix(bits);
        try {
            mix(a / b);
            mix(a % b);
        } catch (ArithmeticException e) {
            mix(e.getMessage().hashCode());
        }
        try {
            mix(b % a);
            mix(b / a);
        } catch (ArithmeticException e) {
            mix(-3);
        }
    }

    // ladd .. lxor, lneg, lcmp, l2i, l2f, l2d, ldiv and lrem.
    static void longs(long a, long b) {
        mix(a + b);
        mix(a - b);
        mix(a * b);
        mix(a & b);
        mix(a | b);
        mix(a ^ b);
        mix(-a);
        mix(~a);
        mix((int) a);
        f((float) a);
        d((double) a);
        int bits = 0;
        if (a == b) bits |= 1;
        if (a != b) bits |= 2;
        if (a < b) bits |= 4;
        if (a >= b) bits |= 8;
        if (a > b) bits |= 16;
        if (a <= b) bits |= 32;
        mix(bits);
        mix(Long.compare(a, b));
        try {
            mix(a / b);
            mix(a % b);
        } catch (ArithmeticException e) {
            mix(e.getMessage().hashCode());
        }
        try {
            mix(b % a);
            mix(b / a);
        } catch (ArithmeticException e) {
            mix(-3);
        }
    }

    // lshl, lshr, lushr, with an int distance.
    static void longShifts(long a, int b) {
        mix(a << b);
        mix(a >> b);
        mix(a >>> b);
        mix(a << (long) b);
    }

    // fadd .. fdiv, frem, fneg, fcmpl, fcmpg, f2i, f2l, f2d.
    static void floats(float a, float b) {
        f(a + b);
        f(a - b);
        f(a * b);
        f(a / b);
        f(a % b);
        f(-a);
        mix((int) a);
        mix((long) a);
        d((double) a);
        mix((byte) a);
        mix((char) a);
        mix((short) a);
        int bits = 0;
        if (a == b) bits |= 1;
        if (a != b) bits |= 2;
        if (a < b) bits |= 4;
        if (a >= b) bits |= 8;
        if (a > b) bits |= 16;
        if (a <= b) bits |= 32;
        if (!(a < b)) bits |= 64;
        if (!(a > b)) bits |= 128;
        mix(bits);
    }

    // dadd .. ddiv, drem, dneg, dcmpl, dcmpg, d2i, d2l, d2f.
    static void doubles(double a, double b) {
        d(a + b);
        d(a - b);
        d(a * b);
        d(a / b);
        d(a % b);
        d(-a);
        mix((int) a);
        mix((long) a);
        f((float) a);
        mix((byte) a);
        mix((char) a);
        mix((short) a);
        int bits = 0;
        if (a == b) bits |= 1;
        if (a != b) bits |= 2;
        if (a < b) bits |= 4;
        if (a >= b) bits |= 8;
        if (a > b) bits |= 16;
        if (a <= b) bits |= 32;
        if (!(a < b)) bits |= 64;
        if (!(a > b)) bits |= 128;
        mix(bits);
    }

    // aconst_null, iconst_m1 .. iconst_5, lconst, fconst, dconst, bipush, sipush, ldc, ldc_w,
    // ldc2_w.
    static void constants() {
        Object none = null;
        mix(none == null ? 1 : 0);
        mix(-1);
        mix(0);
        mix(1);
        mix(2);
        mix(3);
        mix(4);
        mix(5);
        mix(0L);
        mix(1L);
        f(0.0f);
        f(1.0f);
        f(2.0f);
        d(0.0);
        d(1.0);
        mix(-128);
        mix(127);
        mix(-32768);
        mix(32767);
        mix(-32769);
        mix(32768);
        mix(0x123456789abcdefL);
        f(3.25f);
        f(-0.0f);
        d(-0.0);
        d(1e-310);
        mix("constant".length());
    }

    // The loads and stores of every kind, in their short forms (slots 0 to 3) and their long
    // ones, with two-slot values next to one-slot ones.
    static long locals(long l0, double d2, int i4, float f5, Object a6, long l7, double d9) {
        long x = l0 + l7;
        double y = d2 * d9;
        int i = i4;
        float g = f5;
        Object o = a6;
        long sum = 0;
        for (int k = 0; k < 4; k++) {
            long t = x;
            x = l7 ^ sum;
            l7 = t + k;
            double u = y;
            y = d9 - u;
            d9 = u + g;
            g = g * 1.5f + i;
            i = i * 31 + k;
            Object p = o;
            o = a6;
            a6 = p;
            sum += x + Double.doubleToRawLongBits(y) + i + Float.floatToRawIntBits(g);
        }
        return sum + (o == a6 ? 1 : 2) + l0 + Double.doubleToRawLongBits(d2);
    }

    static int i0(int a, int b, int c, int d) {
        int t = a;
        a = d;
        d = c;
        c = b;
        b = t;
        return a * 1000 + b * 100 + c * 10 + d;
    }

    static float f0(float a, float b, float c, float d) {
        float t = a;
        a = d;
        d = c;
        c = b;
        b = t;
        return a * 1000 + b * 100 + c * 10 + d;
    }

    static long l0(long a, long b) {
        long t = a;
        a = b;
        b = t;
        return a * 3 + b;
    }

    static long l1(int pad, long a, long b) {
        long t = a;
        a = b;
        b = t;
        return a * 3 + b + pad;
    }

    static double d0(double a, double b) {
        double t = a;
        a = b;
        b = t;
        return a * 3 + b;
    }

    static double d1(int pad, double a, double b) {
        double t = a;
        a = b;
        b = t;
        return a * 3 + b + pad;
    }

    static Object a0(Object a, Object b, Object c, Object d) {
        Object t = a;
        a = d;
        d = c;
        c = b;
        b = t;
        return a == null ? b : c == null ? d : a;
    }

    int field;
    long wide;
    static long ignored;

    static long bump() {
        return ++ignored;
    }

    static int bumpInt() {
        return (int) ++ignored;
    }

    // dup, dup_x1, dup_x2, dup2, dup2_x1, dup2_x2, pop and pop2, as javac writes them for
    // assignments used as values and for results nobody reads.
    static void stack(JitOpcodes o, int[] ints, long[] longs, int v, long w) {
        int a;
        int b = a = v + 1;
        long x;
        long y = x = w + 1;
        mix(a + b);
        mix(x + y);
        mix(o.field = v);
        mix(ints[1] = v + 2);
        mix(o.wide = w);
        mix(longs[1] = w + 2);
        ints[0] += v;
        longs[0] += w;
        mix(ints[0]++);
        mix(++ints[0]);
        mix(longs[0]++);
        mix(++longs[0]);
        mix(o.field++);
        mix(++o.field);
        mix(o.wide++);
        mix(++o.wide);
        bump();
        bumpInt();
        mix(o.field + ints[0] + ints[1]);
        mix(o.wide + longs[0] + longs[1]);
    }

    // if_acmpeq, if_acmpne, ifnull, ifnonnull.
    static int refs(Object a, Object b) {
        int bits = 0;
        if (a == b) bits |= 1;
        if (a != b) bits |= 2;
        if (a == null) bits |= 4;
        if (a != null) bits |= 8;
        if (b == null) bits |= 16;
        return bits;
    }

    static int table(int key) {
        switch (key) {
            case -2: return 1;
            case -1: return 2;
            case 0: return 3;
            case 1: return 4;
            case 2: return 5;
            case 4: return 6;
            default: return 7;
        }
    }

    static int tableAtTheEdge(int key) {
        switch (key) {
            case Integer.MAX_VALUE - 2: return 1;
            case Integer.MAX_VALUE - 1: return 2;
            case Integer.MAX_VALUE: return 3;
            default: return 4;
        }
    }

    static int tableAtTheOtherEdge(int key) {
        switch (key) {
            case Integer.MIN_VALUE: return 1;
            case Integer.MIN_VALUE + 1: return 2;
            case Integer.MIN_VALUE + 2: return 3;
            default: return 4;
        }
    }

    static int lookup(int key) {
        switch (key) {
            case Integer.MIN_VALUE: return 1;
            case -1000000: return 2;
            case -7: return 3;
            case 0: return 4;
            case 9: return 5;
            case 65536: return 6;
            case Integer.MAX_VALUE: return 7;
            default: return 8;
        }
    }

    static int lookupOfOne(int key) {
        switch (key) {
            case -5: return 1;
            default: return 2;
        }
    }

    static int lookupOfNone(int key) {
        switch (key) {
            default: return key + 1;
        }
    }

    // A switch that falls through and loops, so the compiled code is entered at its cases.
    static int switchLoop(int n) {
        int state = 0;
        int out = 0;
        for (int i = 0; i < n; i++) {
            switch (state) {
                case 0:
                    out += 1;
                    state = 3;
                    break;
                case 1:
                    out += 10;
                case 2:
                    out += 100;
                    state = 5;
                    break;
                case 3:
                    out ^= i;
                    state = (i & 1) + 1;
                    break;
                default:
                    out = out * 31 + state;
                    state = 0;
            }
        }
        return out;
    }

    static String round() {
        h = 17;
        for (int a : INTS) {
            for (int b : INTS) {
                ints(a, b);
            }
        }
        long ints = h;
        h = 17;
        for (long a : LONGS) {
            for (long b : LONGS) {
                longs(a, b);
            }
            for (int b : INTS) {
                longShifts(a, b);
            }
        }
        long longs = h;
        h = 17;
        for (float a : FLOATS) {
            for (float b : FLOATS) {
                floats(a, b);
            }
        }
        long floats = h;
        h = 17;
        for (double a : DOUBLES) {
            for (double b : DOUBLES) {
                doubles(a, b);
            }
        }
        long doubles = h;
        h = 17;
        JitOpcodes o = new JitOpcodes();
        int[] intArray = new int[2];
        long[] longArray = new long[2];
        for (int i = 0; i < INTS.length; i++) {
            int a = INTS[i];
            long l = LONGS[i % LONGS.length];
            float g = FLOATS[i % FLOATS.length];
            double e = DOUBLES[i];
            for (int k = 0; k < 40; k++) {
                constants();
                mix(locals(l, e, a, g, REFS[k % REFS.length], l + k, e * k));
                mix(i0(a, k, i, 4));
                f(f0(g, k, i, 4));
                mix(l0(l, k));
                mix(l1(a, l, k));
                d(d0(e, k));
                d(d1(a, e, k));
                mix(a0(REFS[k % REFS.length], null, REFS[i % REFS.length], "x") == null ? 1 : 2);
                stack(o, intArray, longArray, a + k, l - k);
                mix(refs(REFS[k % REFS.length], REFS[i % REFS.length]));
            }
            mix(table(a));
            mix(tableAtTheEdge(a));
            mix(tableAtTheOtherEdge(a));
            mix(lookup(a));
            mix(lookupOfOne(a));
            mix(lookupOfNone(a));
            mix(switchLoop(a & 1023));
        }
        for (int key = -600; key < 600; key++) {
            mix(table(key));
            mix(tableAtTheEdge(Integer.MAX_VALUE - 3 + (key & 3)));
            mix(tableAtTheOtherEdge(Integer.MIN_VALUE + (key & 3)));
            mix(lookup(key));
            mix(lookupOfOne(key));
            mix(lookupOfNone(key));
        }
        return "ints " + ints + " longs " + longs + " floats " + floats + " doubles " + doubles
                + " rest " + h;
    }

    public static void main(String[] args) {
        for (int i = 0; i < ROUNDS; i++) {
            System.out.println(round());
        }
    }
}
