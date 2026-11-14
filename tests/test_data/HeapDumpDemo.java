package samples.management.heapdump;

import com.sun.management.HotSpotDiagnosticMXBean;
import com.sun.management.VMOption;
import java.io.IOException;
import java.lang.management.ManagementFactory;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * Dumps the heap through {@code HotSpotDiagnosticMXBean} and reads the HPROF file back, checking
 * that objects this program created are in it with their classes, fields and array contents.
 */
public class HeapDumpDemo {

    static class Base {
        short tag = 7;
        char letter = 'q';
    }

    static class Marker extends Base {
        static int COUNT;
        static final Object SHARED = new int[] {42};

        int id;
        long weight;
        double ratio;
        float scale;
        boolean flag;
        byte small;
        String label;
        int[] data;
        Marker[] peers;

        Marker(int id) {
            this.id = id;
            this.weight = 1_000_000_000_000L * id;
            this.ratio = id / 4.0;
            this.scale = id * 1.5f;
            this.flag = id % 2 == 0;
            this.small = (byte) -id;
            this.label = "marker-" + id;
            this.data = new int[] {1, 2, 3, 5, 8 * id};
            COUNT++;
        }
    }

    static class Garbage {
        long filler = 99;
    }

    static Marker[] keep;

    // ---- a small HPROF reader ----

    record Field(String name, int type) {}

    /** Big-endian reads from a byte array. */
    static final class Reader {
        final byte[] bytes;
        int at;

        Reader(byte[] bytes, int at) {
            this.bytes = bytes;
            this.at = at;
        }

        int u1() {
            return bytes[at++] & 0xFF;
        }

        int u2() {
            int value = (bytes[at] & 0xFF) << 8 | (bytes[at + 1] & 0xFF);
            at += 2;
            return value;
        }

        int i4() {
            int value = (bytes[at] & 0xFF) << 24 | (bytes[at + 1] & 0xFF) << 16
                    | (bytes[at + 2] & 0xFF) << 8 | (bytes[at + 3] & 0xFF);
            at += 4;
            return value;
        }

        long i8() {
            long high = i4();
            return high << 32 | (i4() & 0xFFFFFFFFL);
        }

        void skip(int count) {
            at += count;
        }
    }

    static final class ClassDump {
        long superId;
        long loaderId;
        final Map<String, Object> statics = new HashMap<>();
        final List<Field> fields = new ArrayList<>();
    }

    static final int CLASS = 0x20;
    static final int INSTANCE = 0x21;
    static final int OBJECT_ARRAY = 0x22;
    static final int PRIMITIVE_ARRAY = 0x23;

    /**
     * An index of a dump: where each record is, kept in plain arrays so that reading a dump of
     * tens of thousands of objects allocates next to nothing. Records are decoded when asked for.
     */
    static final class Dump {
        byte[] bytes;
        String magic;
        int idSize;
        boolean ended;
        int segments;
        final int[] records = new int[256];

        // UTF8 records: the text of name `nameIds[i]` is `nameLength[i]` bytes at `nameAt[i]`.
        long[] nameIds = new long[4096];
        int[] nameAt = new int[4096];
        int[] nameLength = new int[4096];
        int names;

        // LOAD CLASS records.
        long[] loadedIds = new long[1024];
        long[] loadedNames = new long[1024];
        int loaded;

        // Heap records. `owner` is an instance's or object array's class; `size` is an
        // instance's byte length or an array's element count; `at` is where the payload starts.
        long[] ids = new long[8192];
        long[] owner = new long[8192];
        int[] at = new int[8192];
        int[] size = new int[8192];
        byte[] kind = new byte[8192];
        int objects;

        final Map<Long, ClassDump> classes = new HashMap<>();

        void addName(long id, int position, int length) {
            if (names == nameIds.length) {
                nameIds = Arrays.copyOf(nameIds, names * 2);
                nameAt = Arrays.copyOf(nameAt, names * 2);
                nameLength = Arrays.copyOf(nameLength, names * 2);
            }
            nameIds[names] = id;
            nameAt[names] = position;
            nameLength[names++] = length;
        }

        void addLoaded(long classId, long nameId) {
            if (loaded == loadedIds.length) {
                loadedIds = Arrays.copyOf(loadedIds, loaded * 2);
                loadedNames = Arrays.copyOf(loadedNames, loaded * 2);
            }
            loadedIds[loaded] = classId;
            loadedNames[loaded++] = nameId;
        }

        void addObject(int tag, long id, long ownerId, int position, int length) {
            if (objects == ids.length) {
                ids = Arrays.copyOf(ids, objects * 2);
                owner = Arrays.copyOf(owner, objects * 2);
                at = Arrays.copyOf(at, objects * 2);
                size = Arrays.copyOf(size, objects * 2);
                kind = Arrays.copyOf(kind, objects * 2);
            }
            ids[objects] = id;
            owner[objects] = ownerId;
            at[objects] = position;
            size[objects] = length;
            kind[objects++] = (byte) tag;
        }

        // Open-addressing hash table from an object's id to its index plus one.
        int[] slots;

        static int hash(long id) {
            long mixed = id * 0x9E3779B97F4A7C15L;
            return (int) (mixed >>> 40);
        }

        int find(long id, int tag) {
            if (slots == null) {
                slots = new int[Integer.highestOneBit(objects + 1) * 4];
                for (int i = 0; i < objects; i++) {
                    int slot = hash(ids[i]) & (slots.length - 1);
                    while (slots[slot] != 0) {
                        slot = (slot + 1) & (slots.length - 1);
                    }
                    slots[slot] = i + 1;
                }
            }
            int slot = hash(id) & (slots.length - 1);
            while (slots[slot] != 0) {
                int index = slots[slot] - 1;
                if (ids[index] == id && kind[index] == (byte) tag) {
                    return index;
                }
                slot = (slot + 1) & (slots.length - 1);
            }
            throw new IllegalStateException("no record " + tag + " for an object");
        }

        String name(long nameId) {
            for (int i = 0; i < names; i++) {
                if (nameIds[i] == nameId) {
                    return new String(bytes, nameAt[i], nameLength[i], StandardCharsets.UTF_8);
                }
            }
            return null;
        }

        long classIdOf(String name) {
            byte[] wanted = name.getBytes(StandardCharsets.UTF_8);
            for (int i = 0; i < names; i++) {
                if (nameLength[i] == wanted.length && Arrays.equals(bytes, nameAt[i],
                        nameAt[i] + wanted.length, wanted, 0, wanted.length)) {
                    for (int c = 0; c < loaded; c++) {
                        if (loadedNames[c] == nameIds[i]) {
                            return loadedIds[c];
                        }
                    }
                }
            }
            return 0;
        }

        String className(long classId) {
            for (int c = 0; c < loaded; c++) {
                if (loadedIds[c] == classId) {
                    return name(loadedNames[c]);
                }
            }
            return null;
        }

        ClassDump classDump(long classId) {
            if (classId == 0) {
                return null;
            }
            ClassDump c = classes.get(classId);
            if (c == null) {
                c = new ClassDump();
                Reader in = new Reader(bytes, at[find(classId, CLASS)]);
                in.i4();
                c.superId = in.i8();
                c.loaderId = in.i8();
                in.at += 4 * 8 + 4;
                int constants = in.u2();
                for (int i = 0; i < constants; i++) {
                    in.u2();
                    in.skip(sizeOf(in.u1()));
                }
                int statics = in.u2();
                for (int i = 0; i < statics; i++) {
                    String name = name(in.i8());
                    c.statics.put(name, readValue(in, in.u1()));
                }
                int fields = in.u2();
                for (int i = 0; i < fields; i++) {
                    String name = name(in.i8());
                    c.fields.add(new Field(name, in.u1()));
                }
                classes.put(classId, c);
            }
            return c;
        }

        int countInstances(long classId) {
            int count = 0;
            for (int i = 0; i < objects; i++) {
                if (kind[i] == INSTANCE && owner[i] == classId) {
                    count++;
                }
            }
            return count;
        }

        long classOf(long instanceId) {
            return owner[find(instanceId, INSTANCE)];
        }

        /** Field values of an instance by name, walking the class and then its superclasses. */
        Map<String, Object> fieldsOf(long objectId) {
            int index = find(objectId, INSTANCE);
            Map<String, Object> values = new HashMap<>();
            Reader in = new Reader(bytes, at[index]);
            for (ClassDump c = classDump(owner[index]); c != null; c = classDump(c.superId)) {
                for (Field field : c.fields) {
                    Object value = readValue(in, field.type);
                    values.putIfAbsent(field.name, value);
                }
            }
            if (in.at != at[index] + size[index]) {
                throw new IllegalStateException("instance length does not match its class's fields");
            }
            return values;
        }

        int[] ints(long arrayId) {
            int index = find(arrayId, PRIMITIVE_ARRAY);
            Reader in = new Reader(bytes, at[index]);
            int[] result = new int[size[index]];
            for (int i = 0; i < result.length; i++) {
                result[i] = in.i4();
            }
            return result;
        }

        long[] elements(long arrayId) {
            int index = find(arrayId, OBJECT_ARRAY);
            Reader in = new Reader(bytes, at[index]);
            long[] result = new long[size[index]];
            for (int i = 0; i < result.length; i++) {
                result[i] = in.i8();
            }
            return result;
        }

        String string(long stringId) {
            Map<String, Object> fields = fieldsOf(stringId);
            int value = find((Long) fields.get("value"), PRIMITIVE_ARRAY);
            byte coder = (Byte) fields.get("coder");
            return new String(bytes, at[value], size[value],
                    coder == 0 ? StandardCharsets.ISO_8859_1 : StandardCharsets.UTF_16LE);
        }
    }

    static int sizeOf(int type) {
        return switch (type) {
            case 2 -> 8;
            case 4, 8 -> 1;
            case 5, 9 -> 2;
            case 6, 10 -> 4;
            case 7, 11 -> 8;
            default -> throw new IllegalStateException("unknown basic type " + type);
        };
    }

    static Object readValue(Reader in, int type) {
        return switch (type) {
            case 2, 11 -> in.i8();
            case 4 -> in.u1() != 0;
            case 5 -> (char) in.u2();
            case 6 -> Float.intBitsToFloat(in.i4());
            case 7 -> Double.longBitsToDouble(in.i8());
            case 8 -> (byte) in.u1();
            case 9 -> (short) in.u2();
            case 10 -> in.i4();
            default -> throw new IllegalStateException("unknown basic type " + type);
        };
    }

    /** Indexes a dump; without {@code heap}, only checks how its records are framed. */
    static Dump parse(Path file, boolean heap) throws IOException {
        Dump dump = new Dump();
        Reader in = new Reader(Files.readAllBytes(file), 0);
        dump.bytes = in.bytes;
        StringBuilder magic = new StringBuilder();
        for (int b = in.u1(); b != 0; b = in.u1()) {
            magic.append((char) b);
        }
        dump.magic = magic.toString();
        dump.idSize = in.i4();
        in.i8(); // timestamp
        if (dump.idSize != 8) {
            return dump;
        }
        while (in.at < in.bytes.length) {
            int tag = in.u1();
            in.i4(); // time offset
            int length = in.i4();
            int end = in.at + length;
            switch (tag) {
                case 0x01 -> dump.addName(in.i8(), in.at, length - 8);
                case 0x02 -> {
                    in.i4();
                    long classId = in.i8();
                    in.i4();
                    dump.addLoaded(classId, in.i8());
                }
                case 0x0C, 0x1C -> {
                    dump.segments++;
                    if (heap) {
                        parseSegment(dump, in, end);
                    }
                }
                case 0x2C -> dump.ended = !dump.ended && length == 0 && end == in.bytes.length;
                default -> {}
            }
            in.at = end;
        }
        return dump;
    }

    static void parseSegment(Dump dump, Reader in, int end) {
        while (in.at < end) {
            int tag = in.u1();
            dump.records[tag]++;
            switch (tag) {
                case 0xFF, 0x05, 0x07 -> in.at += 8;
                case 0x01, 0x02, 0x03, 0x08 -> in.at += 16;
                case 0x04, 0x06 -> in.at += 12;
                case CLASS -> {
                    dump.addObject(tag, in.i8(), 0, in.at, 0);
                    in.at += 4 + 6 * 8 + 4;
                    int constants = in.u2();
                    for (int i = 0; i < constants; i++) {
                        in.at += 2;
                        in.skip(sizeOf(in.u1()));
                    }
                    int statics = in.u2();
                    for (int i = 0; i < statics; i++) {
                        in.at += 8;
                        in.skip(sizeOf(in.u1()));
                    }
                    in.skip(in.u2() * 9);
                }
                case INSTANCE -> {
                    long id = in.i8();
                    in.at += 4;
                    long classId = in.i8();
                    int length = in.i4();
                    dump.addObject(tag, id, classId, in.at, length);
                    in.at += length;
                }
                case OBJECT_ARRAY -> {
                    long id = in.i8();
                    in.at += 4;
                    int count = in.i4();
                    dump.addObject(tag, id, in.i8(), in.at, count);
                    in.at += count * 8;
                }
                case PRIMITIVE_ARRAY -> {
                    long id = in.i8();
                    in.at += 4;
                    int count = in.i4();
                    int type = in.u1();
                    dump.addObject(tag, id, type, in.at, count);
                    in.at += count * sizeOf(type);
                }
                default -> throw new IllegalStateException("unknown heap record " + tag);
            }
        }
    }

    // ---- the checks ----

    static void makeGarbage() {
        Garbage garbage = new Garbage();
        if (garbage.filler != 99) {
            throw new AssertionError();
        }
    }

    static Dump frame(Path file, boolean heap) throws IOException {
        Dump dump = parse(file, heap);
        System.out.println("magic: " + dump.magic + " idSize=" + dump.idSize);
        System.out.println("ended: " + dump.ended + " segments>0=" + (dump.segments > 0));
        return dump;
    }

    static void describe(Path file) throws IOException {
        Dump dump = frame(file, true);
        String prefix = "samples/management/heapdump/HeapDumpDemo$";
        long markerClass = dump.classIdOf(prefix + "Marker");
        long baseClass = dump.classIdOf(prefix + "Base");
        ClassDump marker = dump.classDump(markerClass);
        System.out.println("classes: marker=" + (markerClass != 0) + " base=" + (baseClass != 0)
                + " super=" + (marker.superId == baseClass)
                + " superOfBase=" + dump.className(dump.classDump(baseClass).superId)
                + " loader=" + dump.className(dump.classOf(marker.loaderId)));
        StringBuilder declared = new StringBuilder();
        for (Field field : marker.fields) {
            declared.append(field.name).append(':').append(field.type).append(' ');
        }
        System.out.println("declared: " + declared.toString().trim());
        System.out.println("statics: COUNT=" + marker.statics.get("COUNT") + " SHARED="
                + Arrays.toString(dump.ints((Long) marker.statics.get("SHARED"))));
        System.out.println("instances: marker=" + dump.countInstances(markerClass)
                + " base=" + dump.countInstances(baseClass));
        System.out.println("garbage: " + dump.countInstances(dump.classIdOf(prefix + "Garbage")));

        long keepClass = dump.classIdOf("samples/management/heapdump/HeapDumpDemo");
        long keepId = (Long) dump.classDump(keepClass).statics.get("keep");
        long[] kept = dump.elements(keepId);
        System.out.println("keep: " + dump.className(dump.owner[dump.find(keepId, OBJECT_ARRAY)])
                + " length=" + kept.length);
        for (long element : kept) {
            Map<String, Object> fields = dump.fieldsOf(element);
            long peers = (Long) fields.get("peers");
            System.out.println("  id=" + fields.get("id") + " weight=" + fields.get("weight")
                    + " ratio=" + fields.get("ratio") + " scale=" + fields.get("scale")
                    + " flag=" + fields.get("flag") + " small=" + fields.get("small")
                    + " tag=" + fields.get("tag") + " letter=" + fields.get("letter")
                    + " label=" + dump.string((Long) fields.get("label"))
                    + " data=" + Arrays.toString(dump.ints((Long) fields.get("data")))
                    + " peers=" + (peers == 0 ? "null" : peers == keepId ? "keep" : "other"));
        }
        long threadClass = dump.classIdOf("java/lang/Thread");
        boolean mainFound = false;
        for (int i = 0; i < dump.objects && !mainFound; i++) {
            if (dump.kind[i] == INSTANCE && dump.owner[i] == threadClass) {
                Object name = dump.fieldsOf(dump.ids[i]).get("name");
                mainFound = "main".equals(dump.string((Long) name));
            }
        }
        System.out.println("threads: main=" + mainFound
                + " threadRoots=" + (dump.records[0x08] > 0)
                + " stickyClasses=" + (dump.records[0x05] > 100));
        System.out.println("arrays: int[]=" + (dump.classIdOf("[I") != 0)
                + " String[]=" + (dump.classIdOf("[Ljava/lang/String;") != 0));
    }

    public static void main(String[] args) throws Exception {
        keep = new Marker[] {new Marker(1), new Marker(2), new Marker(3)};
        keep[1].peers = keep;
        makeGarbage();

        HotSpotDiagnosticMXBean bean = ManagementFactory.getPlatformMXBean(HotSpotDiagnosticMXBean.class);
        // Not Files.createTempDirectory: it seeds a SecureRandom, which costs more than the dumps.
        Path directory = Files.createDirectory(Path.of(System.getProperty("java.io.tmpdir"),
                "heapdump-" + ProcessHandle.current().pid() + "-" + System.nanoTime()));
        Path all = directory.resolve("all.hprof");
        Path live = directory.resolve("live.hprof");
        try {
            System.out.println("== all objects ==");
            bean.dumpHeap(all.toString(), false);
            frame(all, false);

            System.out.println("== live objects ==");
            bean.dumpHeap(live.toString(), true);
            describe(live);

            System.out.println("== failures ==");
            try {
                bean.dumpHeap(all.toString(), true);
                System.out.println("overwrote a file");
            } catch (IOException e) {
                System.out.println("existing file: " + e.getMessage());
            }
            try {
                bean.dumpHeap(directory.resolve("missing").resolve("x.hprof").toString(), true);
                System.out.println("wrote into a missing directory");
            } catch (IOException e) {
                System.out.println("missing directory: " + e.getMessage());
            }
            try {
                bean.dumpHeap(directory.resolve("dump.bin").toString(), true);
                System.out.println("accepted a wrong suffix");
            } catch (IllegalArgumentException e) {
                System.out.println("wrong suffix: " + e.getMessage());
            }
            try {
                bean.dumpHeap(null, true);
            } catch (NullPointerException e) {
                System.out.println("null file: NullPointerException");
            }
        } finally {
            Files.deleteIfExists(all);
            Files.deleteIfExists(live);
            Files.deleteIfExists(directory);
        }

        System.out.println("== options ==");
        VMOption maxHeap = bean.getVMOption("MaxHeapSize");
        System.out.println(maxHeap.getName() + ": positive=" + (Long.parseLong(maxHeap.getValue()) > 0)
                + " writeable=" + maxHeap.isWriteable() + " origin=" + maxHeap.getOrigin());
        try {
            bean.getVMOption("NoSuchOption");
        } catch (IllegalArgumentException e) {
            System.out.println("unknown: " + e.getMessage());
        }
        try {
            bean.setVMOption("MaxHeapSize", "1");
        } catch (IllegalArgumentException e) {
            System.out.println("read-only: " + e.getMessage());
        }
    }
}
