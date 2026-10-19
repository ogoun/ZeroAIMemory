---
tags: [rust, borrowck]
---
# E0502: mutable borrow while a shared borrow is alive

Full message: `cannot borrow `v` as mutable because it is also borrowed as immutable`.
It means there is a `&T` that is still used later, and in between something wants `&mut T`
to the same place. Not two mutable borrows (that is E0499) — one shared, one exclusive.

## Minimal repro

```rust
let mut v = vec![1, 2, 3];
let first = &v[0];      // shared borrow starts
v.push(4);              // E0502: push needs &mut v
println!("{first}");    // shared borrow still used here
```

The reason is real, not pedantic: `push` may reallocate the buffer, and `first` would point
into freed memory.

Since NLL a borrow ends at its *last use*, not at the end of the block. So if the `println!`
goes away, the code compiles. When reading the error, look at the third label in the
diagnostic ("immutable borrow later used here") — that line is what keeps the borrow alive.

## Fixes I actually use

1. **Copy or clone the value out**: `let first = v[0];` for `Copy` types, `.clone()` otherwise.
2. **Reorder**: finish using the reference before mutating.
3. **Store an index instead of a reference**: `let i = 0; v.push(4); v[i]`.
4. **Collect first, mutate after**: when iterating and deciding what to change, gather the
   keys or indices into a `Vec`, drop the iterator, then apply the changes.

## The HashMap trap

```rust
fn get_or_insert(map: &mut HashMap<String, String>, k: &str) -> &String {
    if let Some(v) = map.get(k) {
        return v;                 // returned borrow is tied to the caller's lifetime
    }
    map.insert(k.to_owned(), String::new()); // E0502 with current borrowck
    map.get(k).unwrap()
}
```

This is a known limitation of the current borrow checker (Polonius accepts it). The idiomatic
answer is the entry API: `map.entry(k.to_owned()).or_default()`, which does one lookup and
returns `&mut V`.

## Note on method calls

`v.push(v.len())` compiles thanks to two-phase borrows: the outer `&mut v` is reserved but
not activated until the arguments are evaluated. Do not generalise this — it only works for
autoref method receivers, not for explicit `&mut` taken earlier.
