# dyn Trait and object safety (dyn compatibility)

`error[E0038]: the trait `Plugin` is not dyn compatible` (older compilers: "cannot be made
into an object"). A trait can be used as `dyn Trait` only if the compiler can build a
vtable for it: every method must be callable through a pointer without knowing the
concrete type.

## What breaks it

- A generic method: `fn handle<T: Serialize>(&self, x: T)` — one vtable entry per `T` is
  impossible.
- Returning `Self` or taking `Self` by value: `fn clone_me(&self) -> Self`.
- A method without a receiver (`fn new() -> Self`), i.e. an associated function.
- `trait Plugin: Sized` or a supertrait that is not dyn compatible (`Clone`).
- Associated constants.
- `async fn` in a trait (stable since 1.75) — the returned future has a hidden concrete type.

## Fixes

Exclude a method from the vtable with `where Self: Sized`; it stays callable on concrete
types:

```rust
trait Plugin {
    fn name(&self) -> &str;
    fn run(&self, input: &[u8]) -> Vec<u8>;
    fn boxed(self) -> Box<dyn Plugin> where Self: Sized + 'static { Box::new(self) }
}
```

Generic method → take a trait object instead: `fn handle(&self, x: &dyn erased_serde::Serialize)`.

Cloning boxed objects → a helper method `fn box_clone(&self) -> Box<dyn Plugin>` (or the
`dyn-clone` crate).

Async methods → return `Pin<Box<dyn Future<Output = T> + Send + '_>>` by hand, or use the
`async-trait` macro which does exactly that.

## Practical notes

- `Box<dyn Plugin>` is `!Send` unless written `Box<dyn Plugin + Send + Sync>`; put the
  bounds in a type alias.
- `&dyn Trait` is a fat pointer: data pointer plus vtable pointer.
- Prefer generics (`impl Trait`) when the set of types is known at compile time; use `dyn`
  for heterogeneous collections and plugin registries.
