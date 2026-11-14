package samples.reflection.reflectarrayaccessors;

import java.lang.reflect.Array;
import java.util.Arrays;

public class ReflectArrayAccessorsExample {
    public static void main(String[] args) {
        boolean[] flags = new boolean[2];
        Array.setBoolean(flags, 1, true);
        System.out.println("boolean: " + Array.getBoolean(flags, 1) + " " + Arrays.toString(flags));

        byte[] bytes = new byte[2];
        Array.setByte(bytes, 0, (byte) -7);
        System.out.println("byte: " + Array.getByte(bytes, 0) + " widened to long " + Array.getLong(bytes, 0));

        char[] chars = new char[2];
        Array.setChar(chars, 0, 'Z');
        System.out.println("char: " + Array.getChar(chars, 0) + " as int " + Array.getInt(chars, 0));

        short[] shorts = new short[2];
        Array.setShort(shorts, 1, (short) 300);
        System.out.println("short: " + Array.getShort(shorts, 1) + " as float " + Array.getFloat(shorts, 1));

        long[] longs = new long[1];
        Array.setLong(longs, 0, 1L << 40);
        Array.setInt(new int[1], 0, 5);
        System.out.println("long: " + Array.getLong(longs, 0) + " as double " + Array.getDouble(longs, 0));

        float[] floats = new float[1];
        Array.setFloat(floats, 0, 2.5f);
        System.out.println("float: " + Array.getFloat(floats, 0));

        double[] doubles = new double[2];
        Array.setDouble(doubles, 0, 1.25);
        Array.setInt(doubles, 1, 3);
        System.out.println("double: " + Arrays.toString(doubles));

        // Widening on set: a byte stored into a long[].
        Array.setByte(longs, 0, (byte) 9);
        System.out.println("byte into long[]: " + longs[0]);

        // Narrowing is rejected.
        try {
            Array.setLong(new int[1], 0, 1L);
        } catch (IllegalArgumentException e) {
            System.out.println("setLong on int[]: IllegalArgumentException");
        }
        try {
            Array.getByte(new int[1], 0);
        } catch (IllegalArgumentException e) {
            System.out.println("getByte on int[]: IllegalArgumentException");
        }
        try {
            Array.getInt(new int[1], 3);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("out of bounds: " + e.getMessage());
        }

        // Multi-dimensional creation.
        int[][][] cube = (int[][][]) Array.newInstance(int.class, 2, 3, 4);
        cube[1][2][3] = 42;
        System.out.println("cube: " + cube.length + "x" + cube[0].length + "x" + cube[0][0].length + " corner=" + cube[1][2][3]);

        String[][] grid = (String[][]) Array.newInstance(String.class, 2, 2);
        grid[0][1] = "hi";
        System.out.println("grid: " + Arrays.deepToString(grid));

        long[][][] nested = (long[][][]) Array.newInstance(long[].class, 2, 1);
        System.out.println("nested: " + nested.length + " " + nested[0].length + " " + nested[0][0]);

        try {
            Array.newInstance(int.class, 2, -1);
        } catch (NegativeArraySizeException e) {
            System.out.println("negative: " + e.getMessage());
        }
    }
}
