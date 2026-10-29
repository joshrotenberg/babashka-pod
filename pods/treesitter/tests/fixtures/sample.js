// A fixture for the outline tests.

import { readFile } from "node:fs/promises";

export const LIMIT = 10;

export function add(a, b) {
  return a + b;
}

function* count(n) {
  for (let i = 0; i < n; i++) yield i;
}

const square = (x) => x * x;

const load = async function (path) {
  return readFile(path, "utf8");
};

export class Shape {
  constructor(name) {
    this.name = name;
  }

  area() {
    return 0;
  }

  static unit() {
    return new Shape("unit");
  }
}
