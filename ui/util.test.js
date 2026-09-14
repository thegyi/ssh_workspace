const { test } = require("node:test");
const assert = require("node:assert/strict");
const {
  parentPath,
  joinPath,
  baseName,
  fmtBytes,
  fmtOctal,
  fmtPerm,
} = require("./util.js");

test("parentPath", () => {
  assert.equal(parentPath("/a/b/c"), "/a/b");
  assert.equal(parentPath("/a"), "/");
  assert.equal(parentPath("/"), "/");
  assert.equal(parentPath("/a/b/"), "/a"); // trailing slashes stripped first
});

test("joinPath", () => {
  assert.equal(joinPath("/a/b", "c"), "/a/b/c");
  assert.equal(joinPath("/", "c"), "/c");
  assert.equal(joinPath("/a/", "c"), "/a/c");
});

test("baseName", () => {
  assert.equal(baseName("/a/b/c.txt"), "c.txt");
  assert.equal(baseName("/a/b/"), "b");
  assert.equal(baseName("file"), "file");
  assert.equal(baseName("/"), "/");
});

test("fmtBytes", () => {
  assert.equal(fmtBytes(0), "0 B");
  assert.equal(fmtBytes(512), "512 B");
  assert.equal(fmtBytes(1024), "1.0 KB");
  assert.equal(fmtBytes(1536), "1.5 KB");
  assert.equal(fmtBytes(5 * 1024 * 1024), "5.0 MB");
  assert.equal(fmtBytes(3 * 1024 ** 3), "3.0 GB");
});

test("fmtOctal", () => {
  assert.equal(fmtOctal(0o755), "0755");
  assert.equal(fmtOctal(0o644), "0644");
  assert.equal(fmtOctal(0o4755), "4755"); // setuid bit preserved
  assert.equal(fmtOctal(0o77777), "7777"); // masked to 4 digits
});

test("fmtPerm", () => {
  assert.equal(fmtPerm(0o755), "rwxr-xr-x");
  assert.equal(fmtPerm(0o644), "rw-r--r--");
  assert.equal(fmtPerm(0o000), "---------");
  assert.equal(fmtPerm(0o600), "rw-------");
});
