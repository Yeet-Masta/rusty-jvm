package samples.reflection.methodhandledispatch;

import java.lang.invoke.MethodHandle;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.MethodType;
import java.lang.reflect.Field;
import java.lang.reflect.Method;

public class MethodHandleDispatchExample {
    interface Shape {
        double area();

        default String describe() {
            return getClass().getSimpleName() + " with area " + area();
        }
    }

    static class Square implements Shape {
        final double side;

        Square(double side) {
            this.side = side;
        }

        public double area() {
            return side * side;
        }
    }

    static class Circle implements Shape {
        final double radius;

        Circle(double radius) {
            this.radius = radius;
        }

        public double area() {
            return 3.0 * radius * radius;
        }

        @Override
        public String describe() {
            return "round thing, r=" + radius;
        }
    }

    static class Animal {
        public String sound() {
            return "...";
        }

        public final String kind() {
            return "animal";
        }
    }

    static class Dog extends Animal {
        @Override
        public String sound() {
            return "woof";
        }
    }

    static class Counter {
        public int count;
        public static String label = "initial";
    }

    public static void main(String[] args) throws Throwable {
        MethodHandles.Lookup lookup = MethodHandles.lookup();

        // findVirtual on an interface: REF_invokeInterface, dispatched on the receiver.
        MethodHandle area = lookup.findVirtual(Shape.class, "area", MethodType.methodType(double.class));
        System.out.println("square area: " + (double) area.invoke((Shape) new Square(3)));
        System.out.println("circle area: " + (double) area.invoke((Shape) new Circle(2)));

        // Interface default method and its override.
        MethodHandle describe = lookup.findVirtual(Shape.class, "describe", MethodType.methodType(String.class));
        System.out.println((String) describe.invoke((Shape) new Square(2)));
        System.out.println((String) describe.invoke((Shape) new Circle(1)));

        // unreflect of instance methods: virtual, interface and statically bound (final).
        Method sound = Animal.class.getMethod("sound");
        MethodHandle soundHandle = lookup.unreflect(sound);
        System.out.println("animal sound: " + (String) soundHandle.invoke(new Animal()));
        System.out.println("dog sound: " + (String) soundHandle.invoke(new Dog()));

        Method kind = Animal.class.getMethod("kind");
        System.out.println("dog kind: " + (String) lookup.unreflect(kind).invoke(new Dog()));

        Method shapeArea = Shape.class.getMethod("area");
        System.out.println("unreflected interface: " + (double) lookup.unreflect(shapeArea).invoke((Shape) new Square(5)));

        // unreflectGetter / unreflectSetter on instance and static fields.
        Counter counter = new Counter();
        Field count = Counter.class.getField("count");
        MethodHandle setCount = lookup.unreflectSetter(count);
        MethodHandle getCount = lookup.unreflectGetter(count);
        setCount.invoke(counter, 42);
        System.out.println("count via getter: " + (int) getCount.invoke(counter));
        System.out.println("count via field: " + counter.count);

        Field label = Counter.class.getField("label");
        lookup.unreflectSetter(label).invoke("updated");
        System.out.println("label via getter: " + (String) lookup.unreflectGetter(label).invoke());

        // A null receiver raises NullPointerException instead of crashing the VM.
        try {
            String ignored = (String) soundHandle.invoke((Animal) null);
            System.out.println("no exception: " + ignored);
        } catch (NullPointerException e) {
            System.out.println("caught NullPointerException");
        }
    }
}
