// Эталон экстрактора: JavaScript.
import { readFile } from "node:fs/promises";
import path from "node:path";
const legacy = require("legacy");

export function load(file) {
  return readFile(path.join(".", file));
}

function helper(x) {
  return x + 1;
}

export const arrow = (a, b) => a + b;

const expr = function named(x) {
  return x;
};

var old = function () {};

let later;
later = () => helper(1);

export class Store {
  #secret = 1;
  handler = () => this.open();

  constructor(dir) {
    this.dir = dir;
  }

  open() {
    return new Store(this.dir);
  }

  static create(dir) {
    return new Store(dir);
  }

  get size() {
    return 0;
  }

  set size(v) {}

  async fetch() {}

  *items() {}

  #hidden() {}
}

class Cache extends Store {
  open() {
    return super.open();
  }

  clear() {}
}

const Anon = class Named {
  run() {}
};

export default class {
  method() {}
}

function* generate() {
  yield 1;
}

async function main() {
  const local = () => 1;
  function inner() {
    return local();
  }
  return inner();
}

export const api = {
  list() {
    return [];
  },
  remove: (id) => id,
  update: function (id) {
    return id;
  },
};

module.exports.exported = function () {};

exports.other = () => {};

Store.prototype.extra = function () {};

class Outer {
  build() {
    class Local {
      go() {}
    }
    return new Local();
  }
}

function withDefaults(a = 1, { b } = {}) {
  return a + b;
}

const asyncArrow = async (x) => {
  await main();
  return x;
};

export function last() {}

class Queue {
  push(item) {}
  pop() {}
  peek() {}
}
