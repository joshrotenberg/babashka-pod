// A fixture for the outline tests.

export interface Point {
  x: number;
  y: number;
}

export type Pair = [number, number];

export enum Color {
  Red,
  Green,
}

export function add(a: number, b: number): number {
  return a + b;
}

export const square = (x: number): number => x * x;

export abstract class Shape {
  abstract area(): number;

  describe(): string {
    return `area ${this.area()}`;
  }
}

export class Circle extends Shape {
  constructor(private r: number) {
    super();
  }

  area(): number {
    return Math.PI * this.r * this.r;
  }
}

export namespace Geometry {
  export function origin(): Point {
    return { x: 0, y: 0 };
  }
}
