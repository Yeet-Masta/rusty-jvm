package samples.javacore.strings.concat;

// String concatenation sites of every argument kind. Sites whose arguments are primitives other
// than float and double, or Strings, are built natively; the rest go through
// StringConcatFactory's method handles. Both must give what String.valueOf gives.
public class StringConcatSites {
    public static void main(String[] args) {
        int i = -42;
        long l = Long.MIN_VALUE;
        short s = -7;
        byte b = 120;
        char c = 'Ж';
        boolean t = true;
        boolean f = false;
        String none = null;
        String ascii = "abc";
        String wide = "日本😀";
        // Built at run time: a constant-pool string cannot hold a lone surrogate here yet.
        char high = (char) (0xD800 + args.length);
        String lone = "x" + high + "y";

        System.out.println("i=" + i + " l=" + l + " s=" + s + " b=" + b);
        System.out.println("c=" + c + " t=" + t + " f=" + f + " none=" + none);
        System.out.println(ascii + wide + ascii);
        System.out.println(lone.length() + " " + (lone + lone).length() + " " + (int) (lone + "").charAt(1));
        System.out.println("" + i);
        System.out.println(i + "");
        System.out.println("" + ascii + "");
        System.out.println(c + "" + c);
        System.out.println(Long.MAX_VALUE + "|" + Integer.MIN_VALUE + "|" + (char) 0 + "|");

        // A literal holding the recipe's tag characters makes javac pass constants.
        System.out.println("tag\u0001arg=" + i + "\u0002end");
        System.out.println(("\u0001" + i + "\u0002").length());

        // Fallback sites: floating point and arbitrary objects.
        float fl = 1.0E10f;
        double d = 0.1 + 0.2;
        Object o = new StringBuilder("sb");
        Object n = null;
        System.out.println("fl=" + fl + " d=" + d + " o=" + o + " n=" + n + " i=" + i);
        System.out.println("mixed " + ascii + c + l + t + 3.5);

        // A site run many times, with values that change width.
        long sum = 0;
        for (int k = -1000; k < 1000; k += 7) {
            String text = "k" + k + ":" + (k * 1_000_000_007L) + (k % 3 == 0 ? wide : ascii);
            sum += text.length() * 31L + text.hashCode();
        }
        System.out.println("loop " + sum);
    }
}
