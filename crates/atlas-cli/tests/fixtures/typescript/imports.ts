// Golden test fixture: TypeScript imports
// Covers: import/export chains, barrel re-exports, path aliases

import { Router } from "express";
import { greet, farewell } from "./lib";
import { format } from "./utils/index";
import * as fs from "fs";

export function main(): void {
  const r = new Router();
  greet("World");
  farewell("World");
  format("text");
  fs.readFileSync("file.txt");
}
