// Эталон экстрактора: Java.
package org.example.store;

import java.util.HashMap;
import java.util.List;
import java.util.function.Function;

public class Store implements AutoCloseable, Iterable<Item> {
    public static final int MAX_ITEMS = 64;
    private final HashMap<String, Item> items = new HashMap<>();

    public Store() {
        this("/");
    }

    public Store(String path) {
        open(path);
    }

    @Override
    public void close() {
        items.clear();
    }

    public Item get(String key) {
        return items.get(key);
    }

    public Item get(int index) {
        return null;
    }

    protected void open(String path) {}

    private static Store create() {
        return new Store();
    }

    @Override
    public java.util.Iterator<Item> iterator() {
        return items.values().iterator();
    }

    public <T> T map(Function<Store, T> fn) {
        return fn.apply(this);
    }

    public static class Builder {
        private String path;

        public Builder path(String path) {
            this.path = path;
            return this;
        }

        public Store build() {
            return new Store(path);
        }
    }

    private class Cursor {
        int pos;

        boolean next() {
            return false;
        }

        class Deep {
            void dive() {}
        }
    }

    Runnable task() {
        return new Runnable() {
            @Override
            public void run() {}
        };
    }
}

interface Backend {
    void open(String path);

    default void close() {}

    static Backend none() {
        return null;
    }
}

public interface Repository<T> extends Backend {
    T find(long id);

    List<T> all();
}

enum Mode {
    READ,
    WRITE;

    public boolean writable() {
        return this == WRITE;
    }
}

record Item(String name, int count) {
    Item {
        if (count < 0) throw new IllegalArgumentException();
    }

    public String label() {
        return name + count;
    }
}

@interface Marker {
    String value() default "";
}

abstract class Base {
    abstract void run();

    void stop() {}
}

final class Worker extends Base implements Runnable {
    @Override
    public void run() {
        stop();
    }
}

class Util {
    static int sum(int a, int b) {
        return a + b;
    }

    static double sum(double a, double b) {
        return a + b;
    }
}
