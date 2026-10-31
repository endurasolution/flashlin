/*! RealWeb client v1 — served by FlashLin at <realweb.path>/client.js
 *
 *   <script src="/realweb/client.js"></script>
 *   const rw = new RealWeb("/realweb", { token: () => fetch("/api/realweb-token").then(r => r.text()) });
 *   rw.subscribe("chat/lobby", (data, meta) => console.log(meta.from, data));
 *   await rw.publish("chat/lobby", { text: "hello" });
 *
 * Works as a classic <script> (defines globalThis.RealWeb), as a side-effect
 * `import`, and from Node 22+ via require() (pass { WebSocket } on older Node).
 */
(function (root) {
  "use strict";

  var PROTOCOL = "realweb.v1";
  var REQUEST_TIMEOUT = 15000;

  function RealWeb(url, options) {
    options = options || {};
    this.url = resolveUrl(url || "/realweb");
    this.options = options;
    this.state = "idle"; // idle | connecting | open | closed (emitted as "state")
    this.uid = null;
    this._WebSocket = options.WebSocket || root.WebSocket;
    this._channels = new Map(); // ch -> { handlers:Set, resets:Set, seq, epoch, ready:{promise,resolve,reject}|null }
    this._pending = new Map(); // id -> { resolve, reject, timer, frame, queued }
    this._outbox = []; // ids waiting for the connection
    this._listeners = {};
    this._nextId = 1;
    this._attempt = 0;
    this._closedByUser = false;
    if (options.autoConnect !== false) this.connect();
  }

  function resolveUrl(url) {
    if (/^wss?:\/\//i.test(url)) return url;
    var loc = root.location;
    if (!loc) throw new Error("RealWeb: pass an absolute ws:// or wss:// URL outside a browser");
    var absolute = new URL(url, loc.href);
    absolute.protocol = absolute.protocol === "https:" ? "wss:" : "ws:";
    return absolute.href;
  }

  function jwtExpiry(token) {
    try {
      var part = token.split(".")[1].replace(/-/g, "+").replace(/_/g, "/");
      return JSON.parse(atob(part + "===".slice((part.length + 3) % 4))).exp || null;
    } catch (e) {
      return null;
    }
  }

  RealWeb.prototype.on = function (event, fn) {
    (this._listeners[event] = this._listeners[event] || []).push(fn);
    return this;
  };

  RealWeb.prototype.off = function (event, fn) {
    var list = this._listeners[event] || [];
    this._listeners[event] = list.filter(function (f) { return f !== fn; });
    return this;
  };

  RealWeb.prototype._emit = function (event, value) {
    (this._listeners[event] || []).forEach(function (fn) {
      try { fn(value); } catch (e) { setTimeout(function () { throw e; }); }
    });
  };

  RealWeb.prototype._setState = function (state) {
    if (this.state === state) return;
    this.state = state;
    this._emit("state", state);
  };

  RealWeb.prototype._token = async function () {
    var token = this.options.token;
    if (typeof token === "function") token = await token();
    return token || null;
  };

  RealWeb.prototype.connect = async function () {
    if (this._ws || this._connecting) return;
    this._connecting = true;
    this._closedByUser = false;
    clearTimeout(this._retryTimer);
    this._setState("connecting");
    var token, ws;
    try {
      token = await this._token();
      if (this._closedByUser) return;
      ws = new this._WebSocket(this.url, PROTOCOL);
    } catch (e) {
      this._emit("error", e);
      return this._reconnect();
    } finally {
      this._connecting = false;
    }
    this._ws = ws;
    var self = this;
    ws.onopen = function () {
      ws.send(JSON.stringify({ t: "hello", v: 1, token: token }));
    };
    ws.onmessage = function (event) {
      if (ws !== self._ws) return;
      self._lastRx = Date.now();
      var frame;
      try { frame = JSON.parse(event.data); } catch (e) { return; }
      self._receive(frame, token);
    };
    ws.onclose = function (event) {
      if (ws !== self._ws) return;
      self._onClose(event);
    };
    ws.onerror = function () {};
  };

  RealWeb.prototype._receive = function (frame, token) {
    switch (frame.t) {
      case "welcome":
        this.uid = frame.uid;
        this._attempt = 0;
        this._heartbeat = (frame.heartbeat || 25) * 1000;
        this._startWatchdog();
        this._scheduleRefresh(token);
        this._setState("open");
        this._emit("open", frame);
        this._resubscribe();
        this._flush();
        break;
      case "msg":
        var channel = this._channels.get(frame.ch);
        if (!channel || frame.seq <= channel.seq) return; // duplicate from a replay
        channel.seq = frame.seq;
        var meta = { ch: frame.ch, seq: frame.seq, from: frame.from, ts: frame.ts };
        channel.handlers.forEach(function (fn) {
          try { fn(frame.data, meta); } catch (e) { setTimeout(function () { throw e; }); }
        });
        break;
      case "ok":
      case "err":
      case "pong":
        var pending = frame.id != null && this._pending.get(frame.id);
        if (!pending) return;
        this._pending.delete(frame.id);
        clearTimeout(pending.timer);
        if (frame.t === "err") {
          var error = new Error(frame.msg || frame.code);
          error.code = frame.code;
          pending.reject(error);
        } else {
          pending.resolve(frame);
        }
        break;
      case "kicked":
        var kicked = this._channels.get(frame.ch);
        this._channels.delete(frame.ch);
        if (kicked && kicked.ready) kicked.ready.reject(new Error(frame.reason));
        this._emit("kicked", frame);
        break;
    }
  };

  RealWeb.prototype._onClose = function (event) {
    clearInterval(this._watchdog);
    clearTimeout(this._refreshTimer);
    this._ws = null;
    var self = this;
    // Requests already sent may or may not have been applied; fail them rather
    // than resend (a resent publish could be delivered twice). Unsent ones wait.
    this._pending.forEach(function (pending, id) {
      if (pending.queued) return;
      clearTimeout(pending.timer);
      self._pending.delete(id);
      var error = new Error("RealWeb disconnected");
      error.code = "disconnected";
      pending.reject(error);
    });
    this._emit("close", { code: event.code, reason: event.reason });
    if (this._closedByUser) return this._setState("closed");
    this._reconnect();
  };

  RealWeb.prototype._reconnect = function () {
    if (this._closedByUser) return;
    this._setState("connecting");
    var base = Math.min(30000, 500 * Math.pow(2, this._attempt++));
    var delay = base / 2 + Math.random() * base / 2;
    var self = this;
    clearTimeout(this._retryTimer);
    this._retryTimer = setTimeout(function () { self.connect(); }, delay);
  };

  // Browsers answer server pings invisibly, so the client also watches for
  // silence itself: that catches connections that died while a laptop slept.
  RealWeb.prototype._startWatchdog = function () {
    var self = this;
    clearInterval(this._watchdog);
    this._lastRx = Date.now();
    this._watchdog = setInterval(function () {
      if (!self._ws) return;
      if (Date.now() - self._lastRx > self._heartbeat * 2 + 5000) {
        self._ws.close(4004, "heartbeat_timeout");
        self._onClose({ code: 4004, reason: "heartbeat_timeout" });
        return;
      }
      self._send({ t: "ping" }).catch(function () {});
    }, this._heartbeat);
  };

  RealWeb.prototype._scheduleRefresh = function (token) {
    clearTimeout(this._refreshTimer);
    var exp = token && jwtExpiry(token);
    if (!exp || typeof this.options.token !== "function") return;
    var self = this;
    var wait = Math.max(5000, exp * 1000 - Date.now() - 60000);
    this._refreshTimer = setTimeout(async function () {
      try {
        var fresh = await self._token();
        await self._send({ t: "auth", token: fresh });
        self._scheduleRefresh(fresh);
      } catch (e) {
        self._emit("error", e);
      }
    }, wait);
  };

  RealWeb.prototype._send = function (frame) {
    var self = this;
    return new Promise(function (resolve, reject) {
      var id = self._nextId++;
      frame.id = id;
      var pending = { resolve: resolve, reject: reject, frame: frame, queued: true };
      pending.timer = setTimeout(function () {
        self._pending.delete(id);
        var error = new Error("RealWeb request timed out");
        error.code = "timeout";
        reject(error);
      }, REQUEST_TIMEOUT);
      self._pending.set(id, pending);
      if (self.state === "open") self._transmit(id);
      else self._outbox.push(id);
    });
  };

  RealWeb.prototype._transmit = function (id) {
    var pending = this._pending.get(id);
    if (!pending || !this._ws) return;
    pending.queued = false;
    this._ws.send(JSON.stringify(pending.frame));
  };

  RealWeb.prototype._flush = function () {
    var ids = this._outbox;
    this._outbox = [];
    ids.forEach(this._transmit, this);
  };

  RealWeb.prototype._subscribeFrame = function (ch, channel) {
    var frame = { t: "sub", ch: ch };
    if (channel.epoch) {
      frame.since = channel.seq;
      frame.epoch = channel.epoch;
    }
    var self = this;
    return this._send(frame).then(function (ok) {
      if (channel.epoch !== ok.epoch) channel.seq = 0; // a new epoch restarts numbering
      channel.epoch = ok.epoch;
      if (ok.gap) {
        channel.resets.forEach(function (fn) {
          try { fn({ ch: ch }); } catch (e) { setTimeout(function () { throw e; }); }
        });
      }
      if (channel.ready) channel.ready.resolve(ok);
      return ok;
    }, function (error) {
      if (channel.ready) channel.ready.reject(error);
      if (error.code !== "disconnected" && error.code !== "timeout") self._channels.delete(ch);
      throw error;
    });
  };

  RealWeb.prototype._resubscribe = function () {
    var self = this;
    this._channels.forEach(function (channel, ch) {
      self._subscribeFrame(ch, channel).catch(function (e) { self._emit("error", e); });
    });
  };

  /**
   * Receive messages on `ch`. `onMessage(data, {ch, seq, from, ts})`.
   * `options.onReset({ch})` runs when messages were missed during a
   * disconnection longer than the server's history: reload state then.
   * Returns { ready: Promise, unsubscribe() }.
   */
  RealWeb.prototype.subscribe = function (ch, onMessage, options) {
    var channel = this._channels.get(ch);
    var fresh = !channel;
    if (fresh) {
      channel = { handlers: new Set(), resets: new Set(), seq: 0, epoch: null, ready: null };
      this._channels.set(ch, channel);
    }
    channel.handlers.add(onMessage);
    var onReset = options && options.onReset;
    if (onReset) channel.resets.add(onReset);
    var ready;
    if (fresh) {
      var deferred = {};
      ready = new Promise(function (resolve, reject) { deferred.resolve = resolve; deferred.reject = reject; });
      ready.catch(function () {});
      channel.ready = deferred;
      this._subscribeFrame(ch, channel).catch(function () {});
    } else {
      ready = Promise.resolve();
    }
    var self = this;
    return {
      ready: ready,
      unsubscribe: function () {
        channel.handlers.delete(onMessage);
        if (onReset) channel.resets.delete(onReset);
        if (channel.handlers.size === 0 && self._channels.get(ch) === channel) {
          self._channels.delete(ch);
          if (self.state === "open") self._send({ t: "unsub", ch: ch }).catch(function () {});
        }
      },
    };
  };

  /** Publish `data` (any JSON) to `ch`; resolves with {seq} once the server has accepted it. */
  RealWeb.prototype.publish = function (ch, data) {
    return this._send({ t: "pub", ch: ch, data: data }).then(function (ok) {
      return { seq: ok.seq };
    });
  };

  /** Resolves with { count, users } for a channel this client may subscribe to. */
  RealWeb.prototype.presence = function (ch) {
    return this._send({ t: "presence", ch: ch }).then(function (ok) {
      return { count: ok.count, users: ok.users };
    });
  };

  RealWeb.prototype.close = function () {
    this._closedByUser = true;
    clearTimeout(this._retryTimer);
    clearTimeout(this._refreshTimer);
    clearInterval(this._watchdog);
    if (this._ws) this._ws.close(1000, "client_closed");
    else this._setState("closed");
  };

  root.RealWeb = RealWeb;
  if (typeof module === "object" && module.exports) module.exports = RealWeb;
})(typeof globalThis !== "undefined" ? globalThis : this);
