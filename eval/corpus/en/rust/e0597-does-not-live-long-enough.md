# E0597: borrowed value does not live long enough

E0597 means a reference outlives the thing it points to. The compiler shows where the value
is dropped (`- `x` dropped here while still borrowed`) and where the reference is still used.

```rust
let r;
{
    let x = 5;
    r = &x;      // E0597
}
println!("{r}");
```

## Patterns from real code

**Collecting `&str` from strings created in a loop.**

```rust
let mut names: Vec<&str> = Vec::new();
for line in reader.lines() {
    let line = line?;
    names.push(line.trim());   // E0597: `line` dropped at end of iteration
}
```

The fix is to own the data: `Vec<String>` and `names.push(line.trim().to_owned())`. If the
whole input is read into one `String` up front, borrowing slices of that buffer is fine,
because the buffer outlives the vector.

**Returning a struct that borrows a local.** A parser struct `Parser<'a> { src: &'a str }`
built from a `String` created inside the same function cannot be returned. Either return the
`String` together with the parsed result or make the struct own the text.

**Declaration order.** Locals are dropped in reverse order of declaration. If `a` holds a
reference to `b`, `b` must be declared first. Moving `let b = ...` above `let a = ...` is
sometimes the whole fix.

## Close relatives

- **E0716** `temporary value dropped while borrowed`: `let s = String::from("a").as_str();`
  — the `String` is a temporary that dies at the end of the statement. Bind it to a variable.
- **E0373** for `std::thread::spawn` with a closure borrowing locals: the thread may outlive
  the function. Use `move`, `Arc`, or `std::thread::scope`, which guarantees the threads are
  joined before the borrowed locals go away.

## What not to do

Adding `'static` everywhere or leaking with `Box::leak` silences the error but changes the
design. Usually the right answer is "this data should be owned here", or `Rc`/`Arc` when the
ownership is genuinely shared.
