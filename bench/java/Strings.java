/** StringBuilder appends, indy string concatenation, hashing and equality. */
public final class Strings implements Bench.Workload {
    public long run(int n) {
        long h = 0;
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < n; i++) {
            sb.setLength(0);
            sb.append("item-").append(i).append(':').append(i * 7L);
            String a = sb.toString();
            String b = "item-" + i + ":" + (i * 7L);
            if (a.equals(b)) {
                h = h * 31 + a.hashCode();
            }
            h += a.indexOf(':') + b.length();
        }
        return h;
    }

    public static void main(String[] args) {
        Bench.main(args, new Strings());
    }
}
