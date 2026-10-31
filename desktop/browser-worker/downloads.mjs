import { createReadStream } from "node:fs";
import { createHash, randomUUID } from "node:crypto";
import { open, mkdir, unlink, link, realpath } from "node:fs/promises";
import { request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";
import { basename, extname, join, resolve } from "node:path";

const MAX_BYTES = 25 * 1024 * 1024;
const DOCUMENTS = new Set([".pdf", ".docx", ".xlsx", ".pptx", ".txt", ".csv", ".md"]);
const fail = (code) => Object.assign(new Error(), { code });

/** Only explicit downloads reach here. No opening, execution or arbitrary destination. */
export async function saveDocumentDownload(download, root, taskId) {
  if (!root || !/^[0-9a-f-]{36}$/i.test(taskId)) throw fail("BROWSER_DOWNLOAD_FAILED");
  const name = basename(download.suggestedFilename().replaceAll("\\", "/"));
  const extension = extname(name).toLowerCase();
  if (!DOCUMENTS.has(extension)) {
    await download.cancel().catch(() => {});
    throw fail("BROWSER_DOWNLOAD_TYPE_BLOCKED");
  }
  const directory = join(resolve(root), "downloads", taskId);
  await mkdir(directory, { recursive: true, mode: 0o700 });
  // Reject redirected task directories instead of writing outside the managed runtime.
  if (await realpath(directory) !== directory) throw fail("BROWSER_DOWNLOAD_FAILED");
  const safeName = (name.slice(0, -extension.length).replace(/[^\p{L}\p{N}._ -]/gu, "_").slice(0, 90) || "document") + extension;
  const destination = join(directory, randomUUID() + "-" + safeName);
  const temporary = destination + ".part";
  const handle = await open(temporary, "wx", 0o600);
  const timer = setTimeout(() => { void download.cancel().catch(() => {}); }, 25_000);
  let committed = false;
  try {
    const stream = await download.createReadStream();
    if (!stream) throw fail("BROWSER_DOWNLOAD_FAILED");
    let bytes = 0;
    const hash = createHash("sha256");
    let prefix = Buffer.alloc(0);
    for await (const chunk of stream) {
      bytes += chunk.length;
      if (bytes > MAX_BYTES) {
        stream.destroy();
        await download.cancel().catch(() => {});
        throw fail("BROWSER_DOWNLOAD_TOO_LARGE");
      }
      if (prefix.length < 1024) prefix = Buffer.concat([prefix, chunk]).subarray(0, 1024);
      hash.update(chunk);
      let offset = 0;
      while (offset < chunk.length) {
        const result = await handle.write(chunk, offset, chunk.length - offset);
        if (!result.bytesWritten) throw fail("BROWSER_DOWNLOAD_FAILED");
        offset += result.bytesWritten;
      }
    }
    if (!bytes || await download.failure()) throw fail("BROWSER_DOWNLOAD_FAILED");
    // Detect error/login HTML and obvious binaries masquerading as documents.
    if (/^\s*(?:<!doctype\s+html|<html|<script)/i.test(prefix.toString("utf8"))
      || prefix.subarray(0, 2).toString() === "MZ"
      || extension === ".pdf" && !prefix.toString("utf8").trimStart().startsWith("%PDF-")
      || [".docx", ".xlsx", ".pptx"].includes(extension) && (prefix.length < 4 || prefix.readUInt32LE(0) !== 0x04034b50)) {
      throw fail("BROWSER_DOWNLOAD_TYPE_BLOCKED");
    }
    await handle.sync();
    if ((await handle.stat()).size !== bytes) throw fail("BROWSER_DOWNLOAD_FAILED");
    const digest = hash.digest("hex");
    await handle.close();
    const savedHash = createHash("sha256");
    for await (const chunk of createReadStream(temporary)) savedHash.update(chunk);
    if (savedHash.digest("hex") !== digest) throw fail("BROWSER_DOWNLOAD_FAILED");
    // Exclusive publication: an existing file can never be overwritten.
    await link(temporary, destination);
    committed = true;
    return { path: destination, name: safeName, byteCount: bytes, sha256: digest };
  } finally {
    clearTimeout(timer);
    await handle.close().catch(() => {});
    await unlink(temporary).catch(() => {});
    // Playwright owns its temporary copy; don't keep a second unbounded copy.
    await download.delete().catch(() => {});
    if (!committed) await download.cancel().catch(() => {});
  }
}

/** Stream public GET responses locally with DNS pinned to the validated address. Chromium
 * downloads stay disabled, so there is no unchecked browser-side temporary copy. */
export async function fetchDocumentDownload(rawURL, name, resolvePublicAddress, allowedURL, signal) {
  let current = new URL(rawURL);
  for (let hop = 0; hop < 6; hop++) {
    if (!allowedURL(current.toString())) throw fail("BROWSER_NETWORK_BLOCKED");
    const address = await resolvePublicAddress(current.hostname);
    const response = await new Promise((resolveResponse, reject) => {
      const request = (current.protocol === "https:" ? httpsRequest : httpRequest)(current, {
        method: "GET", agent: false, signal, family: address.family, autoSelectFamily: false,
        headers: { "Accept-Encoding": "identity" },
        lookup: (_hostname, _options, callback) => callback(null, address.address, address.family)
      }, resolveResponse);
      request.once("error", reject);
      request.setTimeout(25_000, () => request.destroy(fail("BROWSER_TIMEOUT")));
      request.end();
    });
    if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
      response.destroy();
      if (!response.headers.location) throw fail("BROWSER_DOWNLOAD_FAILED");
      current = new URL(response.headers.location, current);
      continue;
    }
    if (response.statusCode !== 200 || Number(response.headers["content-length"] ?? 0) > MAX_BYTES) {
      response.destroy();
      throw fail("BROWSER_DOWNLOAD_FAILED");
    }
    return {
      suggestedFilename: () => name,
      createReadStream: async () => response,
      failure: async () => response.complete ? null : "incomplete",
      cancel: async () => { response.destroy(); },
      delete: async () => { response.destroy(); }
    };
  }
  throw fail("BROWSER_DOWNLOAD_FAILED");
}
