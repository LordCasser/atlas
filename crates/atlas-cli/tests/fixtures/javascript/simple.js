// Golden test fixture: JavaScript simple
// Covers: class/method/variable definitions, call/field access/reference, import/export, scope nesting

import { Router } from "express";

export class Server {
  #port;

  constructor(port) {
    this.#port = port;
  }

  start() {
    const router = new Router();
    router.listen(this.#port);
    console.log("started");
  }
}

function createServer(port) {
  return new Server(port);
}

export { createServer };
