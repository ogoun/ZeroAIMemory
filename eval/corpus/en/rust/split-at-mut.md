# split_at_mut for disjoint mutable access

Indexing `&mut v[i]` and `&mut v[j]` at the same time does not compile: the borrow checker
sees each index as borrowing all of `v`. `split_at_mut(mid)` returns `(&mut [T], &mut [T])`
— left `[0, mid)`, right `[mid, len)` — and they can be used together.

```rust
fn swap_add(v: &mut [i64], i: usize, j: usize) {
    assert!(i < j);
    let (left, right) = v.split_at_mut(j);
    left[i] += right[0];
}
```

Related helpers:

- `v.swap(i, j)` if you only need a swap;
- `chunks_mut`, `chunks_exact_mut` for fixed-size blocks;
- `split_first_mut` / `split_last_mut` for head plus tail;
- `get_disjoint_mut([i, j])` (stable since 1.86) returns `Result` with an error on
  overlap or out of range.

Inside, it is a few lines of `unsafe` with `from_raw_parts_mut`; the safe signature is what
proves to the compiler that the parts do not overlap. Panics if `mid > len`.
