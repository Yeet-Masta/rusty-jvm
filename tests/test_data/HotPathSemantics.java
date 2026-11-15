package samples.interpreter.hotpaths;

// Paths the interpreter takes without the general machinery once a site is warm: getters run
// without a frame, aastore skips the element type check for a class it already accepted, and
// System.arraycopy copies compatible arrays in bulk. Each must still behave like the slow path.
public class HotPathSemantics {
    private int count;
    private long total;
    private double ratio;
    private String label;

    int count() { return count; }
    long total() { return total; }
    double ratio() { return ratio; }
    String label() { return label; }

    // A getter in the same class as its caller, so the caller's class is already at hand.
    static int sumCounts(HotPathSemantics[] items) {
        int sum = 0;
        for (HotPathSemantics item : items) {
            sum += item.count();
        }
        return sum;
    }

    public static void main(String[] args) {
        getters();
        arrayStores();
        arrayCopies();
    }

    static void getters() {
        HotPathSemantics item = new HotPathSemantics();
        item.count = 7;
        item.total = Long.MIN_VALUE + 3;
        item.ratio = -0.5;
        item.label = "x";
        long checksum = 0;
        for (int i = 0; i < 1000; i++) {
            item.count = i;
            checksum += item.count() + item.total() + (long) item.ratio() + item.label().length();
        }
        System.out.println("getters " + checksum + " " + item.total() + " " + item.ratio() + " " + item.label());

        HotPathSemantics[] items = new HotPathSemantics[10];
        for (int i = 0; i < items.length; i++) {
            items[i] = new HotPathSemantics();
            items[i].count = i;
        }
        System.out.println("same class " + sumCounts(items));

        HotPathSemantics missing = null;
        try {
            missing.count();
            System.out.println("no exception");
        } catch (NullPointerException e) {
            System.out.println("null receiver NPE");
        }
    }

    static void arrayStores() {
        Number[] numbers = new Integer[4];
        for (int i = 0; i < 100; i++) {
            numbers[i & 3] = i;
        }
        numbers[1] = null;
        try {
            numbers[2] = 2.5;
            System.out.println("no exception");
        } catch (ArrayStoreException e) {
            System.out.println("ASE " + e.getMessage());
        }
        System.out.println("numbers " + numbers[0] + " " + numbers[1] + " " + numbers[2] + " " + numbers[3]);

        Object[] objects = new Object[3];
        objects[0] = "s";
        objects[1] = 1;
        objects[2] = new int[] {4};
        System.out.println("objects " + objects[0] + " " + objects[1] + " " + ((int[]) objects[2])[0]);

        Object[][] nested = new Object[2][];
        nested[0] = new String[] {"a"};
        try {
            Object[] wrong = new Integer[1];
            String[][] strings = new String[1][];
            Object[] asObjects = strings;
            asObjects[0] = wrong;
            System.out.println("no exception");
        } catch (ArrayStoreException e) {
            System.out.println("nested ASE");
        }
    }

    static void arrayCopies() {
        int[] ints = {0, 1, 2, 3, 4, 5, 6, 7};
        System.arraycopy(ints, 0, ints, 2, 5);
        System.out.println("ints forward " + java.util.Arrays.toString(ints));
        int[] back = {0, 1, 2, 3, 4, 5, 6, 7};
        System.arraycopy(back, 3, back, 1, 5);
        System.out.println("ints back " + java.util.Arrays.toString(back));

        long[] longs = {Long.MIN_VALUE, -1, Long.MAX_VALUE};
        long[] longCopy = new long[4];
        System.arraycopy(longs, 0, longCopy, 1, 3);
        System.out.println("longs " + java.util.Arrays.toString(longCopy));

        char[] chars = "hello".toCharArray();
        char[] charCopy = new char[5];
        System.arraycopy(chars, 1, charCopy, 0, 4);
        System.out.println("chars " + new String(charCopy, 0, 4));

        String[] strings = {"a", "b", "c"};
        Object[] objects = new Object[3];
        System.arraycopy(strings, 0, objects, 0, 3);
        System.out.println("objects " + java.util.Arrays.toString(objects));

        Object[] mixed = {"a", 1};
        String[] target = new String[2];
        try {
            System.arraycopy(mixed, 0, target, 0, 2);
            System.out.println("no exception");
        } catch (ArrayStoreException e) {
            System.out.println("partial copy " + target[0] + " " + target[1]);
        }
    }
}
