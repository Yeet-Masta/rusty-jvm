package samples.interpreter.framelocal;

// Field, static and array instructions run in the interpreter's frame-local loop once their site
// is resolved. Each site below is warmed in a loop and then hit with a null reference or a bad
// index, which must still throw from the right place, and with wide and reference values, which
// must keep their two words and their tags.
public class FrameLocalHeapAccess {
    static long wideStatic;
    static Object refStatic;
    static int intStatic;

    long wide;
    double real;
    Object ref;
    int plain;
    FrameLocalHeapAccess next;

    static int readPlain(FrameLocalHeapAccess holder) {
        return holder.plain;
    }

    static void writeWide(FrameLocalHeapAccess holder, long value) {
        holder.wide = value;
    }

    static int load(int[] array, int index) {
        return array[index];
    }

    static void store(long[] array, int index, long value) {
        array[index] = value;
    }

    static int length(byte[] array) {
        return array.length;
    }

    public static void main(String[] args) {
        FrameLocalHeapAccess holder = new FrameLocalHeapAccess();
        long sum = 0;
        for (int i = 0; i < 2000; i++) {
            holder.plain = i;
            holder.wide = holder.wide * 31 + i;
            holder.real = holder.real + i * 0.5;
            holder.ref = (i & 1) == 0 ? holder : null;
            holder.next = holder.ref == null ? null : holder;
            sum += readPlain(holder);
            writeWide(holder, holder.wide ^ 0x123456789ABCDEF0L);
            wideStatic += holder.wide;
            refStatic = holder.next;
            intStatic -= i;
        }
        System.out.println("fields " + sum + " " + holder.wide + " " + holder.real + " "
                + (holder.ref == null) + " " + (holder.next == null));
        System.out.println("statics " + wideStatic + " " + (refStatic == null) + " " + intStatic);

        int[] ints = new int[16];
        long[] longs = new long[16];
        double[] doubles = new double[16];
        char[] chars = new char[16];
        short[] shorts = new short[16];
        byte[] bytes = new byte[16];
        boolean[] flags = new boolean[16];
        float[] floats = new float[16];
        Object[] objects = new Object[16];
        for (int i = 0; i < 2000; i++) {
            int k = i & 15;
            ints[k] += i;
            longs[k] = longs[k] * 7 + i;
            doubles[k] += i / 3.0;
            chars[k] = (char) (0xFFF0 + k);
            shorts[k] = (short) (-i);
            bytes[k] = (byte) i;
            flags[k] = (i & 2) != 0;
            floats[k] = floats[k] + 0.25f;
            sum += load(ints, k) + ints.length + length(bytes);
            store(longs, k, longs[k] + 1);
            objects[k] = (i & 1) == 0 ? ints : null;
        }
        long check = 0;
        for (int k = 0; k < 16; k++) {
            check = check * 31 + ints[k] + longs[k] + (long) doubles[k] + chars[k] + shorts[k]
                    + bytes[k] + (flags[k] ? 1 : 0) + (long) floats[k] + (objects[k] == null ? 0 : 1);
        }
        System.out.println("arrays " + sum + " " + check + " " + (int) chars[3] + " " + shorts[5]
                + " " + bytes[15]);

        FrameLocalHeapAccess missing = null;
        try {
            readPlain(missing);
        } catch (NullPointerException e) {
            System.out.println("getfield NPE");
        }
        try {
            writeWide(missing, 1L);
        } catch (NullPointerException e) {
            System.out.println("putfield NPE");
        }
        try {
            load(ints, 16);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println(e.getMessage());
        }
        try {
            load(null, 0);
        } catch (NullPointerException e) {
            System.out.println("iaload NPE");
        }
        try {
            store(longs, -1, 5L);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println(e.getMessage());
        }
        try {
            length(null);
        } catch (NullPointerException e) {
            System.out.println("arraylength NPE");
        }
    }
}
