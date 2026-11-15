/** Allocation of many short-lived small objects: allocator and collector throughput. */
public final class BinaryTrees implements Bench.Workload {
    static final class Node {
        final Node left, right;

        Node(Node left, Node right) {
            this.left = left;
            this.right = right;
        }

        int check() {
            return left == null ? 1 : 1 + left.check() + right.check();
        }
    }

    static Node create(int depth) {
        return depth == 0 ? new Node(null, null) : new Node(create(depth - 1), create(depth - 1));
    }

    public long run(int maxDepth) {
        int minDepth = 4;
        long total = create(maxDepth + 1).check();
        Node longLived = create(maxDepth);
        for (int depth = minDepth; depth <= maxDepth; depth += 2) {
            int iterations = 1 << (maxDepth - depth + minDepth);
            long check = 0;
            for (int i = 0; i < iterations; i++) {
                check += create(depth).check();
            }
            total += check;
        }
        return total + longLived.check();
    }

    public static void main(String[] args) {
        Bench.main(args, new BinaryTrees());
    }
}
