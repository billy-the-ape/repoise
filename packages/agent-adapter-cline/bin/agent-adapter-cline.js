#!/usr/bin/env node
import { main } from "../src/main.js";

main().catch((error) => {
  process.stderr.write(`agent-adapter-cline: ${error.message}\n`);
  process.exit(1);
});
