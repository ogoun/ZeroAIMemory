# E0106: missing lifetime specifier

```rust
fn longest(a: &str, b: &str) -> &str { if a.len() > b.len() { a } else { b } }
// error[E0106]: missing lifetime specifier
```

The output is a reference, and the compiler cannot guess which parameter it comes from.
Elision covers only two cases: exactly one reference parameter, or a method with `&self` /
`&mut self` (then the result is tied to `self`). With two reference parameters and no
`self`, you must say it:

```rust
fn longest<'a>(a: &'a str, b: &'a str) -> &'a str { ... }
```

Same error for structs holding references: `struct Token { text: &str }` needs
`struct Token<'a> { text: &'a str }`, and every `impl` then carries `<'a>`.

Rule of thumb: the compiler suggests `&'static str` in the hint. Accept that only for
literals. If the struct is long-lived or crosses threads, store `String` instead of
fighting with the annotation.
