#!/usr/bin/env node
// Copies the test fixtures next to the compiled tests so `node --test
// out/test/` can spawn them without touching the source tree at runtime.
const { cpSync, mkdirSync } = require("node:fs");
const { join } = require("node:path");

const root = join(__dirname, "..");
const source = join(root, "src", "test", "fixtures");
const destination = join(root, "out", "test", "fixtures");
mkdirSync(destination, { recursive: true });
cpSync(source, destination, { recursive: true });
