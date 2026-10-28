// Downloads the rclone binary that gets bundled with the app as a Tauri sidecar.
// Tauri expects it at src-tauri/binaries/rclone-<target-triple>[.exe].
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, copyFileSync, chmodSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const RCLONE_VERSION = "v1.73.1";

const PLATFORMS = {
  "aarch64-apple-darwin": "osx-arm64",
  "x86_64-apple-darwin": "osx-amd64",
  "x86_64-pc-windows-msvc": "windows-amd64",
  "aarch64-pc-windows-msvc": "windows-arm64",
  "x86_64-unknown-linux-gnu": "linux-amd64",
  "aarch64-unknown-linux-gnu": "linux-arm64",
};

function hostTriple() {
  const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  return out.match(/^host: (\S+)$/m)[1];
}

const triple = process.env.TAURI_ENV_TARGET_TRIPLE || hostTriple();
const platform = PLATFORMS[triple];
if (!platform) {
  console.error(`fetch-rclone: no rclone build known for ${triple}`);
  process.exit(1);
}

const isWindows = triple.includes("windows");
const exe = isWindows ? "rclone.exe" : "rclone";
const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const outDir = join(root, "src-tauri", "binaries");
const outFile = join(outDir, `rclone-${triple}${isWindows ? ".exe" : ""}`);
const versionFile = `${outFile}.version`;

if (existsSync(outFile) && existsSync(versionFile) && readFileSync(versionFile, "utf8") === RCLONE_VERSION) {
  console.log(`fetch-rclone: ${RCLONE_VERSION} for ${triple} already present`);
  process.exit(0);
}

const name = `rclone-${RCLONE_VERSION}-${platform}`;
const base = `https://downloads.rclone.org/${RCLONE_VERSION}`;

async function download(url) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  return Buffer.from(await res.arrayBuffer());
}

console.log(`fetch-rclone: downloading ${name}.zip`);
const [zip, sums] = await Promise.all([download(`${base}/${name}.zip`), download(`${base}/SHA256SUMS`)]);

const expected = sums.toString("utf8").split("\n").find((l) => l.endsWith(` ${name}.zip`))?.split(" ")[0];
const actual = createHash("sha256").update(zip).digest("hex");
if (!expected || expected !== actual) {
  console.error(`fetch-rclone: checksum mismatch for ${name}.zip (expected ${expected}, got ${actual})`);
  process.exit(1);
}

const work = mkdtempSync(join(tmpdir(), "clonemount-rclone-"));
try {
  const zipPath = join(work, `${name}.zip`);
  writeFileSync(zipPath, zip);
  // bsdtar (macOS, Windows 10+) reads zip files; GNU tar on Linux does not.
  if (process.platform === "linux") execFileSync("unzip", ["-q", zipPath, "-d", work]);
  else execFileSync("tar", ["-xf", zipPath, "-C", work]);
  mkdirSync(outDir, { recursive: true });
  copyFileSync(join(work, name, exe), outFile);
  if (!isWindows) chmodSync(outFile, 0o755);
  writeFileSync(versionFile, RCLONE_VERSION);
  console.log(`fetch-rclone: wrote ${outFile}`);
} finally {
  rmSync(work, { recursive: true, force: true });
}
