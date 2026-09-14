/* Pure path/format helpers shared by the UI. Dependency-free on purpose so
   the same file is unit-testable under `node --test` (util.test.js). */

function parentPath(p) {
  const t = p.replace(/\/+$/, "");
  const i = t.lastIndexOf("/");
  return i <= 0 ? "/" : t.slice(0, i);
}

function joinPath(dir, name) {
  return dir.endsWith("/") ? dir + name : `${dir}/${name}`;
}

function baseName(p) {
  return p.replace(/\/+$/, "").split("/").pop() || p;
}

function fmtBytes(n) {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log2(n) / 10));
  return `${(n / 2 ** (10 * i)).toFixed(i ? 1 : 0)} ${u[i]}`;
}

function fmtOctal(perm) {
  return (perm & 0o7777).toString(8).padStart(4, "0");
}

function fmtPerm(perm) {
  const bits = [0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001];
  return bits.map((b, i) => (perm & b ? "rwx"[i % 3] : "-")).join("");
}

function fmtDate(ts) {
  const d = new Date(ts * 1000);
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

if (typeof module !== "undefined") {
  module.exports = { parentPath, joinPath, baseName, fmtBytes, fmtOctal, fmtPerm, fmtDate };
}
