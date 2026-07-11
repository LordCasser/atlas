// Golden test fixture: ArkTS simple
// Covers: function/class/variable definitions, call/field access, import/export, scope nesting
// Uses TypeScript-compatible syntax retained under ArkTS constraints.

import { Router } from "@ohos/express";

export class Server {
  port: number;

  constructor(port: number) {
    this.port = port;
  }

  start(): void {
    const router = new Router();
    router.listen(this.port);
    console.log("started");
  }
}

function createServer(port: number): Server {
  return new Server(port);
}

export { createServer };
