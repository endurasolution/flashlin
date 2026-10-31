# RealWeb — FlashLin's built-in realtime hub

RealWeb lets browsers and apps exchange live messages through FlashLin itself.
Your backend never holds a socket: it signs a token for each user and, when it
has something to say, makes one HTTP request. FlashLin keeps the connections,
checks permissions, fans messages out, and replays what a reconnecting client
missed.

FlashLin also still **proxies ordinary WebSockets** to your application
(`proxy_pass` or `app =` routes, see [QUERY_AND_WEBSOCKETS.md](QUERY_AND_WEBSOCKETS.md)).
Use that when your app already speaks WebSocket; use RealWeb when you want the
features below without writing them.

Try it: `flashlin examples/realweb.toml`, then open http://127.0.0.1:8080/ in
two windows (a chat room plus a clock pushed by a Node app).

## What RealWeb adds on top of a plain WebSocket

RealWeb uses standard WebSocket framing as its transport (subprotocol
`realweb.v1`), so it works in every browser and through any proxy or CDN that
carries WebSockets. Raw byte speed is the same as a WebSocket; the gains are in
what the hub does for you:

| Plain WebSocket | RealWeb |
|---|---|
| No built-in authentication; browsers cannot set an `Authorization` header | Signed, expiring HS256 tokens (standard JWTs) sent in the first frame, never in the URL |
| Any site can open a socket with a visitor's cookies (cross-site WebSocket hijacking) | Origin checked at the handshake: same host by default, or an allowlist |
| One stream; apps invent their own routing | Named channels multiplexed on one connection, with per-channel subscribe/publish permissions |
| Messages sent during a disconnection are lost | Per-channel sequence numbers and retained history; clients resume where they stopped, or are told they missed data |
| No delivery confirmation | Every publish is acknowledged with its sequence number |
| Dead connections linger (half-open TCP, sleeping laptops) | Heartbeats in both directions; silent peers are closed |
| A slow reader makes the server buffer without limit | Bounded per-connection queue; a stalled consumer is disconnected |
| No size or flood limits unless the app adds them | Message size limit, per-connection rate limit, channel and connection caps |
| Tokens, once accepted, last for the connection's lifetime | Connection closes when its token expires unless refreshed; refresh can narrow rights |
| Backend must hold every socket | Backend publishes with `POST /flashlin/realweb/publish` |
| No standard reconnect | The JS client reconnects with backoff and jitter, resubscribes and resumes |

Fan-out is efficient: a published message is serialized once and shared by
reference among subscriber queues, and each connection writes queued messages
in batches with one flush.

## Configure

```toml
[realweb]
enabled         = true
path            = "/realweb"             # WebSocket endpoint; client at /realweb/client.js
secret          = "…at least 32 random characters…"   # openssl rand -base64 48
allowed_origins = ["https://example.com"] # default []: the page's own host only; "*": any
public_channels = ["news/*"]              # readable without a token

# Limits (defaults shown)
max_connections     = 10000
max_message_size    = "64KB"   # largest frame a client may send
max_channels        = 100      # subscriptions per connection
history             = 100      # messages kept per channel for resume (0 = none)
history_ttl         = "5m"
messages_per_second = 50       # per connection, burst 2x
heartbeat           = "25s"    # must be below server.keep_alive_timeout
send_queue          = 1024     # queued messages before a client counts as too slow
```

`flashlin --check` validates the section. Either `secret` or `public_channels`
is required. `[realweb]` belongs in `server.toml` and applies to every virtual
host; channels are shared across hosts.

## Channels and permissions

Channel names are 1–128 characters of `A-Z a-z 0-9 - _ . : / @`, for example
`chat/lobby`, `user/42`, `order:9917`. Patterns are an exact name, a prefix
ending in `*` (`chat/*`), or `*` alone.

A connection can **subscribe** to a channel matching `public_channels` or its
token's `subscribe` list, and **publish** only to its token's `publish` list.
Anonymous clients (no token) can only read public channels.

## Tokens

Tokens are HS256 JSON Web Tokens signed with `realweb.secret`. Mint them in
your app after your own login check, with any JWT library:

```jsonc
{
  "sub": "user-42",                       // user id: shown as "from" and in presence
  "exp": 1767225600,                      // required, Unix seconds
  "subscribe": ["chat/*", "user/user-42"],
  "publish": ["chat/*"]
}
```

```js
// Node: npm i jsonwebtoken
jwt.sign({ sub: user.id, subscribe: ["chat/*"], publish: ["chat/*"] }, SECRET, { expiresIn: "15m" });
```

```python
# Python: pip install PyJWT
jwt.encode({"sub": user_id, "exp": int(time.time()) + 900,
            "subscribe": ["chat/*"], "publish": ["chat/*"]}, SECRET, algorithm="HS256")
```

FlashLin rejects tokens without `exp`, any algorithm other than HS256
(including `none`), and channel patterns that are not valid. Expiry and `nbf`
allow 30 seconds of clock skew. Keep tokens short-lived (minutes): the JS
client fetches a new one before expiry and on every reconnect.

For testing, the CLI signs one with the configured secret:

```bash
flashlin realweb token --user alice --subscribe "chat/*" --publish "chat/*" --ttl 1h
```

## Browser and Node client

FlashLin serves the client at `<path>/client.js`. It has no dependencies and
works as a `<script>`, as a side-effect `import`, and with `require()` in
Node 22+.

```html
<script src="/realweb/client.js"></script>
<script>
  const rw = new RealWeb("/realweb", {
    token: () => fetch("/api/realweb-token").then(r => r.text()), // called on every (re)connect
  });

  const sub = rw.subscribe("chat/lobby", (data, meta) => {
    console.log(meta.from, data, meta.seq, meta.ts);
  }, {
    onReset: () => reloadHistoryFromYourApi(),   // missed more than the server kept
  });
  await sub.ready;

  const { seq } = await rw.publish("chat/lobby", { text: "hi" }); // resolves on server ack
  const { count, users } = await rw.presence("chat/lobby");

  rw.on("state", s => console.log(s));   // connecting | open | closed
  sub.unsubscribe();
  rw.close();
</script>
```

Behaviour worth knowing:

- Reconnects automatically with exponential backoff (0.5 s → 30 s) and jitter.
- Resubscribes with the last sequence number seen per channel; replayed
  duplicates are dropped, so each message is delivered to your handler once.
- Publishes made while disconnected are queued and sent after reconnecting
  (each still times out 15 s after it was made, with `code: "timeout"`).
  A publish already sent when the connection drops is rejected with
  `code: "disconnected"` instead of resent, so it is never delivered twice;
  retry it yourself if that is safe for your app.
- Errors from the server reject with `error.code`: `forbidden`, `bad_channel`,
  `rate_limited`, `limit`, `timeout`, …

## Publishing from your backend

From the same machine, any app can publish with one HTTP request:

```bash
curl -X POST http://127.0.0.1:8080/flashlin/realweb/publish \
  -H 'Content-Type: application/json' \
  -d '{"ch": "news/today", "data": {"headline": "Shipped"}}'
# {"seq":12,"delivered":340}
```

Optional `"from"` sets the sender shown to clients. This is a loopback control
endpoint like `/flashlin/apps`: it refuses non-local clients and requests with
proxy headers, and requires `Authorization: Bearer <server.control_token>` when
a control token is set. Apps on other machines should publish through an app
on the FlashLin host.

The CLI does the same, plus counters:

```bash
flashlin realweb publish news/today '{"headline": "Shipped"}'
flashlin realweb status    # {"connections":..,"channels":..,"published":..,"delivered":..,"slow_disconnects":..}
```

`/metrics` includes `flashlin_realweb_connections`, `flashlin_realweb_channels`,
`flashlin_realweb_published_total`, `flashlin_realweb_delivered_total` and
`flashlin_realweb_slow_disconnects_total`.

## Wire protocol (for other client libraries)

JSON text frames over a WebSocket opened with subprotocol `realweb.v1`. Any
frame may carry a numeric `id`; the reply to it carries the same `id`.

Client → server:

| Frame | Meaning |
|---|---|
| `{"t":"hello","v":1,"token":"…"}` | Must be first, within 10 s; `token` optional |
| `{"t":"sub","ch":"…","since":7,"epoch":"…"}` | Subscribe; `since`/`epoch` (from a previous `ok`) resume |
| `{"t":"unsub","ch":"…"}` | Unsubscribe |
| `{"t":"pub","ch":"…","data":<any JSON>}` | Publish |
| `{"t":"presence","ch":"…"}` | Subscriber count and user ids (up to 1,000) |
| `{"t":"auth","token":"…"}` | Replace the token before it expires (same `sub` required) |
| `{"t":"ping"}` | Answered with `pong` |

Server → client:

| Frame | Meaning |
|---|---|
| `{"t":"welcome","sid":…,"uid":…,"exp":…,"heartbeat":25,"max_message":65536}` | Hello accepted |
| `{"t":"ok","id":…,…}` | Success. For `sub`: `seq`, `epoch`, `gap`, `replayed`; for `pub`: `seq` |
| `{"t":"err","id":…,"code":"…","msg":"…"}` | Request refused; connection stays open |
| `{"t":"msg","ch":"…","seq":…,"ts":…,"from":"…","data":…}` | Channel message (`ts` in ms; `from` absent for backend publishes) |
| `{"t":"kicked","ch":"…","reason":"forbidden"}` | Subscription ended by a narrower refreshed token |
| `{"t":"pong","id":…}` | Reply to `ping` |

Resuming: sequence numbers increase by one per channel. After an `ok` with
`replayed: n`, the server sends those `n` retained messages, then live ones.
`gap: true` means messages between `since` and the oldest retained one are
gone, or the channel restarted (different `epoch`, for example after a
FlashLin restart): reload state from your application.

Close codes:

| Code | Reason |
|---|---|
| 4000 | `hello_required`: first frame missing or not `hello` |
| 4001 | `token_expired` |
| 4002 | `slow_consumer`: the client did not read fast enough |
| 4003 | `token_invalid`, `token_expired`, `token_malformed`, `tokens_not_enabled` at hello |
| 4004 | `heartbeat_timeout` |
| 4008 | `rate_limited`: kept flooding after refusals |
| 1003 | binary frames are not supported |
| 1009 | message larger than `max_message_size` |

## Limits and scope

- One FlashLin process is one hub. Running several FlashLin servers behind a
  load balancer gives separate hubs; there is no clustering yet.
- History lives in memory, capped per channel by `history`, `history_ttl` and
  1 MiB. It does not survive a restart (clients then see `gap: true`). At most
  100,000 channels exist at once; idle ones are removed.
- Delivery is at-most-once per connection plus resume from history. It is not
  a durable queue: keep anything that must never be lost in your database.
- WebSocket over HTTP/2 or HTTP/3 (extended CONNECT) is not implemented;
  browsers use HTTP/1.1 for these connections, which works alongside HTTP/2.
- The connection's idle limits still apply: `server.keep_alive_timeout` for
  reads (heartbeats keep it busy) and `server.write_timeout` for a client that
  stops reading entirely.

Test coverage: unit tests for token verification and patterns, and live TCP
tests for the handshake checks, permissions, fan-out, presence, resume and gap
detection, backend publishing, token refresh and expiry, rate/size/binary
limits and slow-consumer disconnection. The JS client was exercised from Node
against a running server (reconnect and resume included). It has not been
load-tested at scale or reviewed independently.
