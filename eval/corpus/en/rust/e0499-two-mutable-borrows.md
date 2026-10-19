# E0499: cannot borrow as mutable more than once at a time

E0499 fires when two exclusive references to the same value overlap. Unlike E0502 there is
no shared reference involved — both sides want `&mut`.

```rust
let mut s = String::from("x");
let a = &mut s;
let b = &mut s;   // E0499
a.push('y');
b.push('z');
```

## Where it bites in real code

### Methods that take `&mut self`

```rust
struct Game { players: Vec<Player>, log: Vec<String> }

impl Game {
    fn log(&mut self, msg: String) { self.log.push(msg); }

    fn tick(&mut self) {
        for p in self.players.iter_mut() {
            p.hp -= 1;
            self.log(format!("{} hit", p.name)); // E0499: self already mutably borrowed via players
        }
    }
}
```

The compiler can split borrows of *fields* (`&mut self.players` and `&mut self.log` are
fine together), but it cannot see through a method call: `self.log(..)` asks for the whole
`self`. Options:

- write `self.log.push(..)` directly so the borrows stay field-level;
- turn the helper into a free function or an associated function that takes only the field:
  `fn log_to(log: &mut Vec<String>, msg: String)`;
- destructure at the top: `let Game { players, log } = self;` and work with the two locals.

### Closures that capture `&mut`

A closure that mutates a variable holds a unique borrow for as long as the closure exists.
Calling another mutating closure on the same variable while the first is alive gives E0499.
Inline one of them or limit its scope with a block.

### Storing `&mut` in a struct and using the original

If a builder holds `&mut buf`, touching `buf` directly before the builder is dropped is a
second mutable borrow. Drop the builder (or end its scope) first.

## Not E0499

- Two `&mut` to different elements of the same slice: the compiler treats indexing as
  borrowing the whole slice, see the note about splitting slices into halves.
- `RefCell::borrow_mut` twice: compiles, panics at runtime with `already borrowed`.
