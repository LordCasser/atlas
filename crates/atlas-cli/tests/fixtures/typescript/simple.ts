// Golden test fixture: TypeScript simple
// Covers: function/class/variable definitions, call/field access/type reference, import/export, scope nesting

import { Router } from "express";

export interface Config {
  port: number;
  host: string;
}

export class Server {
  private config: Config;

  constructor(config: Config) {
    this.config = config;
  }

  start(): void {
    const router = new Router();
    router.listen(this.config.port);
    console.log("started");
  }
}

function createServer(port: number): Server {
  const config: Config = { port, host: "localhost" };
  return new Server(config);
}

export { createServer };
