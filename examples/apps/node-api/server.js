// Minimal Node.js app for FlashLin's [[apps]] manager — no dependencies.
// FlashLin passes PORT and HOST; it proxies /api/* here (see examples/apps.toml).
const http = require("http");

const server = http.createServer((req, res) => {
  if (req.url === "/health") {
    res.end("ok");
    return;
  }
  res.setHeader("content-type", "application/json");
  res.end(
    JSON.stringify({
      app: process.env.FLASHLIN_APP,
      instance: Number(process.env.FLASHLIN_INSTANCE),
      path: req.url,
      client: req.headers["x-forwarded-for"],
    }),
  );
});

server.listen(Number(process.env.PORT), process.env.HOST, () => {
  console.log(`listening on ${process.env.HOST}:${process.env.PORT}`);
});

// FlashLin sends SIGTERM on stop/restart; finish in-flight requests first.
// FlashLin's proxy keeps idle keep-alive connections open to the app, and
// server.close() alone waits for those to time out (5 s by default), so close
// them right away.
process.on("SIGTERM", () => {
  server.close(() => process.exit(0));
  server.closeIdleConnections();
});
