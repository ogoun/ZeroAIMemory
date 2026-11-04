//! Эталон экстрактора: Rust.

use std::collections::HashMap;
use std::fmt::{self, Display};

pub const MAX_ITEMS: usize = 64;
static GREETING: &str = "hi";

/// Хранилище.
#[derive(Debug, Clone)]
pub struct Store {
    items: HashMap<String, Item>,
}

pub struct Item(u32);

pub(crate) enum Mode {
    Read,
    Write,
}

pub union Bits {
    a: u32,
    b: f32,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error;

pub trait Backend {
    fn open(path: &str) -> Self;
    fn close(&mut self) {
        let _ = 1;
    }
    const KIND: u8;
    type Handle;
}

impl Store {
    pub fn open(path: &str) -> Store {
        let s = Store::new();
        s.load(path);
        s
    }

    fn new() -> Store {
        Store { items: HashMap::new() }
    }

    pub fn load(&self, _path: &str) {}

    pub const LIMIT: usize = 10;
}

impl Backend for Store {
    fn open(path: &str) -> Self {
        Store::open(path)
    }
    const KIND: u8 = 1;
    type Handle = u32;
}

impl Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<T: Clone> From<T> for Error {
    fn from(_v: T) -> Self {
        Error
    }
}

impl Error {
    pub fn code(&self) -> u32 {
        0
    }
}

pub fn run(db: Store, mode: Mode) -> Result<()> {
    fn helper(x: u32) -> u32 {
        x + 1
    }
    let _ = helper(1);
    match mode {
        Mode::Read => {}
        Mode::Write => {}
    }
    drop(db);
    Ok(())
}

fn private_fn() {}

pub async fn fetch() -> u32 {
    7
}

pub unsafe fn raw() {}

macro_rules! square {
    ($x:expr) => {
        $x * $x
    };
}

pub mod net {
    pub struct Client;

    impl Client {
        pub fn connect(&self) -> bool {
            true
        }
    }

    pub fn ping() {}

    pub mod tcp {
        pub fn dial() {}

        pub enum State {
            Open,
            Closed,
        }
    }
}

mod tests {
    use super::*;

    fn check() {
        let _ = square!(2);
    }
}

pub fn generic<T: Display>(value: T) -> String {
    value.to_string()
}

pub struct Pair<A, B> {
    a: A,
    b: B,
}

impl<A, B> Pair<A, B> {
    pub fn first(&self) -> &A {
        &self.a
    }
    pub fn second(&self) -> &B {
        &self.b
    }
}

pub trait Visitor {
    fn visit(&self);
}

impl Visitor for Pair<u8, u8> {
    fn visit(&self) {}
}

impl net::Client {
    pub fn disconnect(&self) {}
}
