import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/** HashMap and ArrayList with boxed integers: interface calls, boxing, hashing, sorting. */
public final class Collections implements Bench.Workload {
    public long run(int n) {
        Map<Integer, Integer> map = new HashMap<>();
        for (int i = 0; i < n; i++) {
            map.merge(i % (n / 4 + 1), i, Integer::sum);
        }
        List<Integer> list = new ArrayList<>(map.values());
        for (int i = 0; i < n; i++) {
            list.add((i * 2654435761L % 1000003) > 0 ? (int) (i * 2654435761L % 1000003) : i);
        }
        list.sort(null);
        long sum = 0;
        for (int i = 0; i < list.size(); i += 7) {
            sum = sum * 31 + list.get(i);
        }
        for (int i = 0; i < n; i++) {
            Integer v = map.get(i);
            if (v != null) {
                sum += v;
            }
        }
        return sum;
    }

    public static void main(String[] args) {
        Bench.main(args, new Collections());
    }
}
