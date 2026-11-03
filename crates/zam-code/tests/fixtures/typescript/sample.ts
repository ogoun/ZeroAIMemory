// Эталон экстрактора: TypeScript.
import { Store, type Db } from "./store";
import Default from "./default";

export interface Options {
  path: string;
  open(mode: number): void;
  close(): Promise<void>;
}

export type Id = string | number;

type Handler = (e: Event) => void;

export enum Color {
  Red,
  Green,
}

const enum Flags {
  A = 1,
}

export function connect(opts: Options): Store {
  return new Store(opts.path);
}

function helper(x: number): number {
  return x * 2;
}

export function parse(input: string): number;
export function parse(input: number): number;
export function parse(input: any): number {
  return Number(input);
}

export const makeId = (n: number): Id => `${n}`;

const twice = function (x: number) {
  return helper(helper(x));
};

function component(target: any) {
  return target;
}

@component
export class App implements Options {
  path = "/";
  private handler = (e: Event): void => {
    console.log(e);
  };

  constructor(private readonly db: Db) {}

  @log
  open(mode: number): void {
    this.start();
  }

  async close(): Promise<void> {}

  start(): void {}

  get size(): number {
    return 0;
  }

  static create(): App {
    return new App(null as unknown as Db);
  }
}

export abstract class Base {
  abstract run(): void;
  protected stop(): void {}
}

class Derived extends Base {
  run(): void {
    this.stop();
  }
}

function log(target: any, key: string) {}

export namespace Geometry {
  export interface Point {
    x: number;
    distance(other: Point): number;
  }

  export function origin(): Point {
    return { x: 0, distance: () => 0 };
  }

  export class Circle {
    area(): number {
      return 0;
    }
  }
}

declare module "external" {
  export function ext(): void;
}

export default class Main {
  main(): void {}
}

function* gen() {
  yield 1;
}

export const util = {
  format(value: string): string {
    return value;
  },
  trim: (value: string) => value.trim(),
};

let late: () => void;
late = () => {};

interface Repo<T> {
  find(id: Id): T;
}

class MemRepo<T> implements Repo<T> {
  find(id: Id): T {
    throw new Error(String(id));
  }
}
