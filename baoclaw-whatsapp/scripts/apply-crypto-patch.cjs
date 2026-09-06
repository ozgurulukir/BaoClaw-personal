const { createCipheriv, createDecipheriv } = require("node:crypto");
const { spawnSync } = require("node:child_process");
const path = require("node:path");
const fs = require("node:fs");

function nativeAesWorks() {
  try {
    const key = Buffer.alloc(32, 1);
    const iv = Buffer.alloc(16, 2);
    const cipher = createCipheriv("aes-256-cbc", key, iv);
    const encrypted = Buffer.concat([cipher.update("baoclaw"), cipher.final()]);
    const decipher = createDecipheriv("aes-256-cbc", key, iv);
    const decrypted = Buffer.concat([
      decipher.update(encrypted),
      decipher.final(),
    ]);
    return decrypted.toString() === "baoclaw";
  } catch {
    return false;
  }
}

if (nativeAesWorks() && process.env.BAOCLAW_FORCE_PATCH !== "1") {
  console.log(
    "[crypto-patch] Native AES is working; skipping Baileys/libsignal patches.",
  );
  process.exit(0);
}

// patch-package resolves the app root by walking up from its cwd and then
// looks for node_modules there. Under npm workspaces the patched deps
// (baileys, libsignal) are hoisted to the workspace root, so we must run
// with cwd = workspace root and a patch dir relative to that root.
// Set BAOCLAW_FORCE_PATCH=1 to exercise this path on healthy hardware.
const workspaceRoot = path.resolve(__dirname, "..", "..");
const pkgDir = path.resolve(__dirname, "..");
const patchDir = path.join(path.relative(workspaceRoot, pkgDir), "patches");

// patch-package is not idempotent: re-applying to an already-patched tree
// fails and would abort npm install. Both patches are applied by the same
// invocation, so the Baileys shim is a reliable "already patched" marker.
const baileysShim = path.join(
  workspaceRoot,
  "node_modules",
  "@whiskeysockets",
  "baileys",
  "lib",
  "Utils",
  "_aes_cbc_shim.js",
);
if (fs.existsSync(baileysShim)) {
  console.log(
    "[crypto-patch] Patches already applied; skipping patch-package.",
  );
  process.exit(0);
}

const patchPackage = require.resolve("patch-package", { paths: [__dirname] });

console.warn(
  "[crypto-patch] Applying Baileys/libsignal patches (root: %s)...",
  workspaceRoot,
);
const result = spawnSync(
  process.execPath,
  [patchPackage, "--patch-dir", patchDir],
  {
    cwd: workspaceRoot,
    stdio: "inherit",
  },
);
process.exit(result.status ?? 1);
