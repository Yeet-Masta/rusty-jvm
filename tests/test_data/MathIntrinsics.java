package samples.interpreter.intrinsics;

// Math, StrictMath, Integer and Long methods the interpreter computes natively. Each result is
// printed as raw bits, so a wrong NaN, a lost negative zero or a rounding difference shows.
// Every call is made twice: the first one resolves the site, the second runs warm.
public class MathIntrinsics {
    static final double[] DOUBLES = {
        0.0, -0.0, 1.0, -1.0, 0.5, -0.5, 1.5, -1.5, 2.5, -2.5, 0.49999999999999994,
        -0.49999999999999994, 4503599627370497.0, 1e300, -1e300, Double.MIN_VALUE,
        Double.MAX_VALUE, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, Double.NaN,
        Double.longBitsToDouble(0x7ff0000000000123L), Double.longBitsToDouble(0xfff8000000000000L),
        9.223372036854776E18, -9.223372036854776E18, 2.0, 1e-310
    };
    static final float[] FLOATS = {
        0.0f, -0.0f, 1.0f, -1.0f, 0.5f, -0.5f, 2.5f, -2.5f, 0.49999997f, 8388609.0f,
        Float.MIN_VALUE, Float.MAX_VALUE, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY,
        Float.NaN, Float.intBitsToFloat(0x7fc00123), 3.0e9f, -3.0e9f
    };
    static final int[] INTS = {0, 1, -1, 7, Integer.MIN_VALUE, Integer.MAX_VALUE, 0x00f0_0000};
    static final long[] LONGS = {0, 1, -1, 7, Long.MIN_VALUE, Long.MAX_VALUE, 0x0000_f000_0000_0000L};

    static long h = 17;

    static void mix(long bits) {
        h = h * 31 + bits;
    }

    static void d(double v) {
        mix(Double.doubleToRawLongBits(v));
    }

    static void f(float v) {
        mix(Float.floatToRawIntBits(v));
    }

    static long doubles() {
        h = 17;
        for (double a : DOUBLES) {
            d(Math.sqrt(a));
            d(StrictMath.sqrt(a));
            d(Math.abs(a));
            d(Math.floor(a));
            d(Math.ceil(a));
            d(Math.rint(a));
            mix(Math.round(a));
            d(StrictMath.floor(a));
            for (double b : DOUBLES) {
                d(Math.min(a, b));
                d(Math.max(a, b));
                d(Math.fma(a, b, 1.0));
                d(Math.fma(a, 3.0, b));
            }
        }
        return h;
    }

    static long floats() {
        h = 17;
        for (float a : FLOATS) {
            f(Math.abs(a));
            mix(Math.round(a));
            for (float b : FLOATS) {
                f(Math.min(a, b));
                f(Math.max(a, b));
                f(Math.fma(a, b, 1.0f));
                f(Math.fma(a, 3.0f, b));
            }
        }
        return h;
    }

    static long integers() {
        h = 17;
        for (int a : INTS) {
            mix(Math.abs(a));
            mix(Integer.numberOfLeadingZeros(a));
            mix(Integer.numberOfTrailingZeros(a));
            mix(Integer.bitCount(a));
            for (int b : INTS) {
                mix(Math.min(a, b));
                mix(Math.max(a, b));
            }
        }
        for (long a : LONGS) {
            mix(Math.abs(a));
            mix(Long.numberOfLeadingZeros(a));
            mix(Long.numberOfTrailingZeros(a));
            mix(Long.bitCount(a));
            for (long b : LONGS) {
                mix(Math.min(a, b));
                mix(Math.max(a, b));
            }
        }
        return h;
    }

    public static void main(String[] args) {
        for (int i = 0; i < 2; i++) {
            System.out.println("doubles " + doubles());
            System.out.println("floats " + floats());
            System.out.println("integers " + integers());
        }
        System.out.println("sqrt " + Math.sqrt(2.0) + " " + Math.round(-2.5) + " " + Math.rint(2.5)
                + " " + Math.min(0.0, -0.0) + " " + Math.max(-0.0f, 0.0f) + " " + Math.fma(0.1, 10.0, -1.0));
    }
}
