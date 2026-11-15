/** Branchy integer code over small int arrays. */
public final class Fannkuch implements Bench.Workload {
    public long run(int n) {
        int[] perm = new int[n];
        int[] perm1 = new int[n];
        int[] count = new int[n];
        for (int i = 0; i < n; i++) {
            perm1[i] = i;
        }
        int maxFlips = 0;
        int checksum = 0;
        int permCount = 0;
        int r = n;
        while (true) {
            while (r != 1) {
                count[r - 1] = r;
                r--;
            }
            System.arraycopy(perm1, 0, perm, 0, n);
            int flips = 0;
            int k;
            while ((k = perm[0]) != 0) {
                int k2 = (k + 1) >> 1;
                for (int i = 0; i < k2; i++) {
                    int t = perm[i];
                    perm[i] = perm[k - i];
                    perm[k - i] = t;
                }
                flips++;
            }
            maxFlips = Math.max(maxFlips, flips);
            checksum += (permCount % 2 == 0) ? flips : -flips;
            while (true) {
                if (r == n) {
                    return ((long) checksum << 16) | maxFlips;
                }
                int perm0 = perm1[0];
                for (int i = 0; i < r; i++) {
                    perm1[i] = perm1[i + 1];
                }
                perm1[r] = perm0;
                count[r]--;
                if (count[r] > 0) {
                    break;
                }
                r++;
            }
            permCount++;
        }
    }

    public static void main(String[] args) {
        Bench.main(args, new Fannkuch());
    }
}
