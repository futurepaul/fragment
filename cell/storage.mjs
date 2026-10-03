// A computer's storage endpoint (docs/computers.md, Storage): a minimal
// S3-compatible gateway over the deployment's R2 bucket, scoped to one key
// prefix. It runs behind the computer's HTTP intercept for
// storage.fragment.internal (entry.mjs, ComputerEgress), so the guest
// reaches it with any keys and never holds an R2 credential: the computer
// (and so its prefix) comes from the handler's props, set by the Computer
// DO, not from anything the guest sends. Signatures are not checked (there
// is nothing secret to check them against). From spike S3, where Litestream
// replicated and restored through it.
//
// Path-style only: /<bucket>/<key>. The bucket name is ignored (one per computer).
// Operations: ListObjectsV2 (and V1), GetObject (Range), HeadObject, PutObject
// (incl. aws-chunked bodies), DeleteObject, DeleteObjects, multipart
// (Create/UploadPart/Complete/Abort), HeadBucket, GetBucketLocation.

const XMLNS = "http://s3.amazonaws.com/doc/2006-03-01/";
const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&apos;");
const unesc = (s) => s.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&apos;/g, "'").replace(/&amp;/g, "&");
const xml = (body, status = 200) => new Response(`<?xml version="1.0" encoding="UTF-8"?>\n${body}`, { status, headers: { "content-type": "application/xml" } });
const s3error = (code, message, status) => xml(`<Error><Code>${code}</Code><Message>${esc(message)}</Message></Error>`, status);

// aws-chunked: "<hex>[;chunk-signature=..]\r\n<data>\r\n ... 0[;..]\r\n<trailers>\r\n\r\n"
function awsChunkedDecoder() {
  let buf = new Uint8Array(0), state = "size", remaining = 0;
  const td = new TextDecoder();
  const concat = (a, b) => { const c = new Uint8Array(a.length + b.length); c.set(a); c.set(b, a.length); return c; };
  const crlf = (b) => { for (let i = 0; i + 1 < b.length; i++) if (b[i] === 13 && b[i + 1] === 10) return i; return -1; };
  return new TransformStream({
    transform(chunk, ctrl) {
      buf = buf.length ? concat(buf, chunk) : chunk;
      for (;;) {
        if (state === "size") {
          const i = crlf(buf); if (i < 0) return;
          remaining = parseInt(td.decode(buf.subarray(0, i)).split(";")[0], 16);
          buf = buf.subarray(i + 2);
          if (!(remaining >= 0)) throw new Error("bad aws-chunked size");
          state = remaining === 0 ? "trailer" : "data";
        } else if (state === "data") {
          if (!buf.length) return;
          const n = Math.min(remaining, buf.length);
          ctrl.enqueue(buf.slice(0, n)); buf = buf.subarray(n); remaining -= n;
          if (remaining === 0) state = "crlf";
        } else if (state === "crlf") {
          if (buf.length < 2) return; buf = buf.subarray(2); state = "size";
        } else { buf = new Uint8Array(0); return; } // trailers (checksums): ignored
      }
    },
  });
}

// A body R2 can store: R2 needs a known length for streams.
function bodyFor(request) {
  const h = request.headers;
  const chunked = (h.get("content-encoding") || "").includes("aws-chunked") || (h.get("x-amz-content-sha256") || "").startsWith("STREAMING-");
  if (chunked) {
    const len = Number(h.get("x-amz-decoded-content-length"));
    if (!Number.isFinite(len)) throw new Error("aws-chunked body without x-amz-decoded-content-length");
    const fixed = new FixedLengthStream(len);
    request.body.pipeThrough(awsChunkedDecoder()).pipeTo(fixed.writable).catch(() => {});
    return { stream: fixed.readable, chunked, len };
  }
  if (!h.has("content-length") && request.body) {
    // Transfer-Encoding: chunked (e.g. curl -T -): R2 needs a length, so buffer it.
    return { stream: request.arrayBuffer(), chunked: false, len: "unknown", buffered: true };
  }
  const len = Number(h.get("content-length") || 0);
  if (!request.body || len === 0) return { stream: new Uint8Array(0), chunked, len: 0 };
  const fixed = new FixedLengthStream(len);
  request.body.pipeTo(fixed.writable).catch(() => {});
  return { stream: fixed.readable, chunked, len };
}

const objHeaders = (o) => {
  const h = new Headers();
  o.writeHttpMetadata(h);
  h.set("etag", o.httpEtag);
  h.set("last-modified", o.uploaded.toUTCString());
  h.set("accept-ranges", "bytes");
  return h;
};

/** Handle one S3 request against `bucket` under `prefix` (ends in "/"). Returns [Response, opName]. */
export async function handleS3(request, bucket, prefix) {
  const url = new URL(request.url);
  const q = url.searchParams;
  const m = request.method;
  const path = url.pathname.replace(/^\/+/, "");
  const slash = path.indexOf("/");
  const bucketName = decodeURIComponent(slash < 0 ? path : path.slice(0, slash));
  const key = slash < 0 ? "" : decodeURIComponent(path.slice(slash + 1));
  const full = prefix + key;
  if (key.includes("..")) return [s3error("InvalidArgument", "bad key", 400), "bad"];

  if (!key) {
    if (m === "HEAD") return [new Response(null, { status: 200 }), "HeadBucket"];
    if (m === "GET" && q.has("location")) return [xml(`<LocationConstraint xmlns="${XMLNS}">auto</LocationConstraint>`), "GetBucketLocation"];
    if (m === "GET") {
      const v2 = q.get("list-type") === "2";
      const p = q.get("prefix") || "";
      const delimiter = q.get("delimiter") || undefined;
      const max = Math.min(Number(q.get("max-keys") || 1000), 1000);
      const cursor = (v2 ? q.get("continuation-token") : null) || undefined;
      const startAfter = q.get("start-after") || (!v2 ? q.get("marker") : null);
      const r = await bucket.list({ prefix: prefix + p, delimiter, cursor, limit: max, startAfter: startAfter ? prefix + startAfter : undefined });
      const strip = (k) => k.slice(prefix.length);
      const contents = r.objects.map((o) => `<Contents><Key>${esc(strip(o.key))}</Key><LastModified>${o.uploaded.toISOString()}</LastModified><ETag>${esc(o.httpEtag)}</ETag><Size>${o.size}</Size><StorageClass>STANDARD</StorageClass></Contents>`).join("");
      const prefixes = (r.delimitedPrefixes || []).map((d) => `<CommonPrefixes><Prefix>${esc(strip(d))}</Prefix></CommonPrefixes>`).join("");
      const trunc = r.truncated ? (v2 ? `<NextContinuationToken>${esc(r.cursor)}</NextContinuationToken>` : `<NextMarker>${esc(strip(r.objects.at(-1)?.key || ""))}</NextMarker>`) : "";
      const count = r.objects.length + (r.delimitedPrefixes || []).length;
      return [xml(`<ListBucketResult xmlns="${XMLNS}"><Name>${esc(bucketName)}</Name><Prefix>${esc(p)}</Prefix>${v2 ? `<KeyCount>${count}</KeyCount>` : ""}<MaxKeys>${max}</MaxKeys>${delimiter ? `<Delimiter>${esc(delimiter)}</Delimiter>` : ""}<IsTruncated>${r.truncated}</IsTruncated>${trunc}${contents}${prefixes}</ListBucketResult>`), v2 ? "ListObjectsV2" : "ListObjects"];
    }
    if (m === "POST" && q.has("delete")) {
      const body = await request.text();
      const keys = [...body.matchAll(/<Key>([\s\S]*?)<\/Key>/g)].map((x) => unesc(x[1]));
      const quiet = /<Quiet>\s*true\s*<\/Quiet>/i.test(body);
      if (keys.length) await bucket.delete(keys.map((k) => prefix + k));
      const deleted = quiet ? "" : keys.map((k) => `<Deleted><Key>${esc(k)}</Key></Deleted>`).join("");
      return [xml(`<DeleteResult xmlns="${XMLNS}">${deleted}</DeleteResult>`), `DeleteObjects(${keys.length})`];
    }
    return [s3error("NotImplemented", `${m} on bucket`, 501), "unsupported"];
  }

  if (m === "GET") {
    const o = await bucket.get(full, { range: request.headers, onlyIf: request.headers });
    if (!o) return [s3error("NoSuchKey", key, 404), "GetObject(404)"];
    const h = objHeaders(o);
    if (!("body" in o)) return [new Response(null, { status: 304, headers: h }), "GetObject(304)"];
    if (o.range && request.headers.has("range")) {
      const off = o.range.offset ?? 0, len = o.range.length ?? o.size - off;
      h.set("content-range", `bytes ${off}-${off + len - 1}/${o.size}`);
      h.set("content-length", String(len));
      return [new Response(o.body, { status: 206, headers: h }), "GetObject(range)"];
    }
    h.set("content-length", String(o.size));
    return [new Response(o.body, { status: 200, headers: h }), "GetObject"];
  }
  if (m === "HEAD") {
    const o = await bucket.head(full);
    if (!o) return [new Response(null, { status: 404 }), "HeadObject(404)"];
    const h = objHeaders(o); h.set("content-length", String(o.size));
    return [new Response(null, { status: 200, headers: h }), "HeadObject"];
  }
  if (m === "PUT" && q.has("uploadId")) {
    const up = bucket.resumeMultipartUpload(full, q.get("uploadId"));
    const { stream } = bodyFor(request);
    const part = await up.uploadPart(Number(q.get("partNumber")), await stream);
    return [new Response(null, { status: 200, headers: { etag: part.etag } }), "UploadPart"];
  }
  if (m === "PUT") {
    if (request.headers.has("x-amz-copy-source")) return [s3error("NotImplemented", "CopyObject", 501), "CopyObject"];
    const { stream, chunked, len } = bodyFor(request);
    const o = await bucket.put(full, await stream, { httpMetadata: { contentType: request.headers.get("content-type") || undefined } });
    return [new Response(null, { status: 200, headers: { etag: o.httpEtag } }), `PutObject(${len}${chunked ? ",aws-chunked" : ""})`];
  }
  if (m === "POST" && q.has("uploads")) {
    const up = await bucket.createMultipartUpload(full);
    return [xml(`<InitiateMultipartUploadResult xmlns="${XMLNS}"><Bucket>${esc(bucketName)}</Bucket><Key>${esc(key)}</Key><UploadId>${esc(up.uploadId)}</UploadId></InitiateMultipartUploadResult>`), "CreateMultipartUpload"];
  }
  if (m === "POST" && q.has("uploadId")) {
    const body = await request.text();
    const parts = [...body.matchAll(/<Part>([\s\S]*?)<\/Part>/g)].map((x) => ({
      partNumber: Number(/<PartNumber>(\d+)<\/PartNumber>/.exec(x[1])[1]),
      etag: unesc(/<ETag>([\s\S]*?)<\/ETag>/.exec(x[1])[1]).replace(/"/g, ""),
    }));
    const o = await bucket.resumeMultipartUpload(full, q.get("uploadId")).complete(parts);
    return [xml(`<CompleteMultipartUploadResult xmlns="${XMLNS}"><Bucket>${esc(bucketName)}</Bucket><Key>${esc(key)}</Key><ETag>${esc(o.httpEtag)}</ETag></CompleteMultipartUploadResult>`), `CompleteMultipartUpload(${parts.length})`];
  }
  if (m === "DELETE" && q.has("uploadId")) {
    await bucket.resumeMultipartUpload(full, q.get("uploadId")).abort();
    return [new Response(null, { status: 204 }), "AbortMultipartUpload"];
  }
  if (m === "DELETE") {
    await bucket.delete(full);
    return [new Response(null, { status: 204 }), "DeleteObject"];
  }
  return [s3error("NotImplemented", `${m} ${key}`, 501), "unsupported"];
}
