// Эталон экстрактора: TSX.
import React, { useState } from "react";

export interface ButtonProps {
  label: string;
  onClick(): void;
}

export type Size = "s" | "m" | "l";

export enum Variant {
  Primary,
  Ghost,
}

export function Button(props: ButtonProps) {
  return <button onClick={props.onClick}>{props.label}</button>;
}

export const Card = ({ title }: { title: string }) => (
  <div className="card">
    <h2>{title}</h2>
  </div>
);

const Hidden = function () {
  return <span hidden />;
};

function useCounter(start: number) {
  const [n, setN] = useState(start);
  const inc = () => setN(n + 1);
  return { n, inc };
}

export class Panel extends React.Component<ButtonProps> {
  state = { open: false };
  toggle = () => {
    this.setState({ open: !this.state.open });
  };

  render() {
    return <div onClick={this.toggle}>{this.props.label}</div>;
  }

  componentDidMount(): void {}

  static defaults(): Partial<ButtonProps> {
    return {};
  }
}

export abstract class Widget {
  abstract draw(): JSX.Element;
  mount(): void {}
}

class Label extends Widget {
  draw(): JSX.Element {
    return <label />;
  }
}

export namespace Theme {
  export interface Palette {
    primary: string;
    shade(level: number): string;
  }

  export function dark(): Palette {
    return { primary: "#000", shade: () => "#111" };
  }

  export class Provider {
    wrap(child: JSX.Element): JSX.Element {
      return <div>{child}</div>;
    }
  }
}

export function overload(a: string): string;
export function overload(a: number): number;
export function overload(a: any): any {
  return a;
}

export default function App() {
  const onClick = () => {};
  return (
    <Panel label="x" onClick={onClick}>
      <Button label="ok" onClick={onClick} />
    </Panel>
  );
}

function* frames() {
  yield 1;
}

export const handlers = {
  submit(event: Event): void {},
  reset: () => undefined,
};

interface Store<T> {
  get(key: string): T;
  set(key: string, value: T): void;
}

class MapStore<T> implements Store<T> {
  get(key: string): T {
    throw new Error(key);
  }
  set(key: string, value: T): void {}
}

type Reducer = (state: number) => number;

const reducer: Reducer = (state) => state + 1;
