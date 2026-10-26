#!/usr/bin/env node
// Incompatible protocol version in the hello handshake.
import { hello, readRequest } from "./helpers.mjs";

await readRequest();
hello({ protocolVersion: 2 });
process.exit(0);
