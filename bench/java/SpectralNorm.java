/** Tight loops over double arrays with a computed-index function call. */
public final class SpectralNorm implements Bench.Workload {
    static double a(int i, int j) {
        return 1.0 / ((i + j) * (i + j + 1) / 2 + i + 1);
    }

    static void multiplyAv(int n, double[] v, double[] av) {
        for (int i = 0; i < n; i++) {
            double sum = 0;
            for (int j = 0; j < n; j++) {
                sum += a(i, j) * v[j];
            }
            av[i] = sum;
        }
    }

    static void multiplyAtv(int n, double[] v, double[] atv) {
        for (int i = 0; i < n; i++) {
            double sum = 0;
            for (int j = 0; j < n; j++) {
                sum += a(j, i) * v[j];
            }
            atv[i] = sum;
        }
    }

    static void multiplyAtAv(int n, double[] v, double[] tmp, double[] out) {
        multiplyAv(n, v, tmp);
        multiplyAtv(n, tmp, out);
    }

    public long run(int n) {
        double[] u = new double[n];
        double[] v = new double[n];
        double[] tmp = new double[n];
        java.util.Arrays.fill(u, 1.0);
        for (int i = 0; i < 10; i++) {
            multiplyAtAv(n, u, tmp, v);
            multiplyAtAv(n, v, tmp, u);
        }
        double vBv = 0, vv = 0;
        for (int i = 0; i < n; i++) {
            vBv += u[i] * v[i];
            vv += v[i] * v[i];
        }
        return Math.round(Math.sqrt(vBv / vv) * 1e9);
    }

    public static void main(String[] args) {
        Bench.main(args, new SpectralNorm());
    }
}
