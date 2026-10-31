// RealWeb demo backend for FlashLin's [[apps]] manager — no dependencies.
//
// 1. Mints RealWeb tokens (HS256 JWTs) at /api/realweb-token. A real app
//    would check its own session/login here before deciding what to grant;
//    any JWT library works too, e.g. jsonwebtoken.sign(claims, secret).
// 2. Publishes to a channel with one HTTP call to FlashLin's loopback
//    endpoint — no WebSocket handling in the app at all.
const http = require("http");
const crypto = require("crypto");

const SECRET = process.env.REALWEB_SECRET;
const FLASHLIN = process.env.FLASHLIN_URL || "http://127.0.0.1:8080";

function b64url(value) {
  return Buffer.from(value).toString("base64url");
}

function mintToken(claims) {
  const header = b64url(JSON.stringify({ alg: "HS256", typ: "JWT" }));
  const payload = b64url(JSON.stringify(claims));
  const signature = crypto
    .createHmac("sha256", SECRET)
    .update(`${header}.${payload}`)
    .digest("base64url");
  return `${header}.${payload}.${signature}`;
}

async function publish(ch, data) {
  const response = await fetch(`${FLASHLIN}/flashlin/realweb/publish`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ch, data }),
  });
  if (!response.ok) throw new Error(`publish failed: ${response.status}`);
  return response.json(); // { seq, delivered }
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (url.pathname === "/health") {
    res.end("ok");
    return;
  }
  if (url.pathname === "/api/realweb-token") {
    const name = (url.searchParams.get("name") || "").replace(/[^A-Za-z0-9_-]/g, "").slice(0, 24);
    const user = name || `guest-${crypto.randomInt(1000, 9999)}`;
    const token = mintToken({
      sub: user,
      exp: Math.floor(Date.now() / 1000) + 15 * 60, // the client refreshes before this
      subscribe: ["chat/*", "news/*"],
      publish: ["chat/*"],
    });
    res.setHeader("content-type", "text/plain");
    res.setHeader("cache-control", "no-store");
    res.end(token);
    return;
  }
  res.statusCode = 404;
  res.end("not found");
});

server.listen(Number(process.env.PORT), process.env.HOST, () => {
  console.log(`listening on ${process.env.HOST}:${process.env.PORT}`);
});

// Server-pushed events: anyone (even without a token) can read news/*.
const clock = setInterval(() => {
  publish("news/clock", { now: new Date().toISOString() }).catch((e) => console.error(e.message));
}, 5000);

process.on("SIGTERM", () => {
  clearInterval(clock);
  server.close(() => process.exit(0));
  server.closeIdleConnections();
});
