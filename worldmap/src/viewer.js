"use strict";
// WorldMap map viewer. Everything it needs is in this file: no libraries.
// Exported page (map.html): the tiles are in the page and the network is never asked.
// Live page (meta.live, served by the plugin's own web server): index.json, the tiles in view and
// players.json are asked from the server the page came from, through `window.WorldMapGet`, which
// only the live page defines. Nothing in this file can reach the network by itself.
// The first part (base64, inflate, tile decoding) has no DOM in it, so it can be tested alone.
var WorldMap = (function () {
  var B64 = new Int16Array(128).fill(-1);
  (function () {
    var abc = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for (var i = 0; i < 64; i++) B64[abc.charCodeAt(i)] = i;
  })();

  function base64(text) {
    var len = text.length, pad = 0;
    if (len % 4) throw new Error("worldmap: bad base64");
    if (len && text.charCodeAt(len - 1) === 61) pad++;
    if (len > 1 && text.charCodeAt(len - 2) === 61) pad++;
    var out = new Uint8Array(len / 4 * 3 - pad), o = 0;
    for (var i = 0; i < len; i += 4) {
      var a = B64[text.charCodeAt(i)], b = B64[text.charCodeAt(i + 1)];
      var c = B64[text.charCodeAt(i + 2)], d = B64[text.charCodeAt(i + 3)];
      var last = i + 4 === len;
      if (a < 0 || b < 0 || (c < 0 && !(last && pad === 2)) || (d < 0 && !(last && pad >= 1))) throw new Error("worldmap: bad base64");
      var n = a << 18 | b << 12 | (c & 63) << 6 | (d & 63);
      out[o++] = n >> 16;
      if (o < out.length) out[o++] = n >> 8 & 255;
      if (o < out.length) out[o++] = n & 255;
    }
    return out;
  }

  var LBASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
  var LEXT = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
  var DBASE = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
  var DEXT = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
  var ORDER = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

  // Canonical Huffman table from code lengths lens[off .. off+n)
  function huffman(lens, off, n) {
    var count = new Uint16Array(16), offs = new Uint16Array(16), sym = new Uint16Array(n), i;
    for (i = 0; i < n; i++) count[lens[off + i]]++;
    count[0] = 0;
    for (i = 1; i < 15; i++) offs[i + 1] = offs[i] + count[i];
    for (i = 0; i < n; i++) if (lens[off + i]) sym[offs[lens[off + i]]++] = i;
    return { count: count, sym: sym };
  }

  var fixedLit = null, fixedDist = null;

  // Raw deflate (RFC 1951) to exactly `size` bytes.
  function inflate(src, size) {
    var out = new Uint8Array(size), op = 0, ip = 0, buf = 0, cnt = 0;
    function bits(n) {
      while (cnt < n) {
        if (ip >= src.length) throw new Error("worldmap: data ends early");
        buf |= src[ip++] << cnt;
        cnt += 8;
      }
      var v = buf & ((1 << n) - 1);
      buf >>>= n;
      cnt -= n;
      return v;
    }
    function symbol(h) {
      var code = 0, first = 0, index = 0;
      for (var len = 1; len <= 15; len++) {
        code |= bits(1);
        var c = h.count[len];
        if (code - c < first) return h.sym[index + (code - first)];
        index += c;
        first += c;
        first <<= 1;
        code <<= 1;
      }
      throw new Error("worldmap: bad code");
    }
    var last;
    do {
      last = bits(1);
      var type = bits(2);
      if (type === 0) {
        buf = 0;
        cnt = 0;
        if (ip + 4 > src.length) throw new Error("worldmap: data ends early");
        var n = src[ip] | src[ip + 1] << 8;
        ip += 4;
        if (ip + n > src.length || op + n > size) throw new Error("worldmap: bad stored block");
        out.set(src.subarray(ip, ip + n), op);
        ip += n;
        op += n;
      } else if (type === 1 || type === 2) {
        var lit, dist, i;
        if (type === 1) {
          if (!fixedLit) {
            var fl = new Uint8Array(318);
            for (i = 0; i < 144; i++) fl[i] = 8;
            for (; i < 256; i++) fl[i] = 9;
            for (; i < 280; i++) fl[i] = 7;
            for (; i < 288; i++) fl[i] = 8;
            for (; i < 318; i++) fl[i] = 5;
            fixedLit = huffman(fl, 0, 288);
            fixedDist = huffman(fl, 288, 30);
          }
          lit = fixedLit;
          dist = fixedDist;
        } else {
          var nlen = bits(5) + 257, ndist = bits(5) + 1, ncode = bits(4) + 4;
          if (nlen > 286 || ndist > 30) throw new Error("worldmap: bad table");
          var cl = new Uint8Array(19);
          for (i = 0; i < ncode; i++) cl[ORDER[i]] = bits(3);
          var codes = huffman(cl, 0, 19), lens = new Uint8Array(nlen + ndist);
          i = 0;
          while (i < nlen + ndist) {
            var s = symbol(codes);
            if (s < 16) {
              lens[i++] = s;
            } else {
              var prev = 0, rep;
              if (s === 16) {
                if (!i) throw new Error("worldmap: bad table");
                prev = lens[i - 1];
                rep = 3 + bits(2);
              } else if (s === 17) {
                rep = 3 + bits(3);
              } else {
                rep = 11 + bits(7);
              }
              if (i + rep > nlen + ndist) throw new Error("worldmap: bad table");
              while (rep--) lens[i++] = prev;
            }
          }
          lit = huffman(lens, 0, nlen);
          dist = huffman(lens, nlen, ndist);
        }
        for (;;) {
          var v = symbol(lit);
          if (v < 256) {
            if (op >= size) throw new Error("worldmap: too much data");
            out[op++] = v;
          } else if (v === 256) {
            break;
          } else {
            v -= 257;
            if (v >= 29) throw new Error("worldmap: bad length");
            var len = LBASE[v] + bits(LEXT[v]);
            var dv = symbol(dist);
            if (dv >= 30) throw new Error("worldmap: bad distance");
            var d = DBASE[dv] + bits(DEXT[dv]);
            if (d > op || op + len > size) throw new Error("worldmap: bad copy");
            for (var k = 0; k < len; k++, op++) out[op] = out[op - d];
          }
        }
      } else {
        throw new Error("worldmap: bad block");
      }
    } while (!last);
    if (op !== size) throw new Error("worldmap: wrong size");
    return out;
  }

  // A tile object ({size, band, bands}) to size*size pixel bytes, row by row.
  function decodeTile(tile) {
    var size = tile.size, rows = tile.band, out = new Uint8Array(size * size);
    if (!(size > 0) || !(rows > 0) || tile.bands.length * rows !== size) throw new Error("worldmap: bad tile");
    for (var b = 0; b < tile.bands.length; b++) {
      if (tile.bands[b]) out.set(inflate(base64(tile.bands[b]), rows * size), b * rows * size);
    }
    return out;
  }

  // "rrggbb" x 256 to RGBA x 256; pixel byte 0 (nothing mapped) is transparent.
  function palette(hex) {
    var out = new Uint8Array(1024);
    for (var i = 0; i < 256; i++) {
      out[i * 4] = parseInt(hex.substr(i * 6, 2), 16);
      out[i * 4 + 1] = parseInt(hex.substr(i * 6 + 2, 2), 16);
      out[i * 4 + 2] = parseInt(hex.substr(i * 6 + 4, 2), 16);
      out[i * 4 + 3] = i ? 255 : 0;
    }
    return out;
  }

  // Pixel bytes to RGBA, taking every `step`-th pixel (1 = all).
  function toRGBA(px, size, pal, step) {
    var n = Math.floor(size / step), out = new Uint8ClampedArray(n * n * 4), o = 0;
    for (var z = 0; z < n; z++) {
      var row = z * step * size;
      for (var x = 0; x < n; x++, o += 4) {
        var p = px[row + x * step] * 4;
        out[o] = pal[p];
        out[o + 1] = pal[p + 1];
        out[o + 2] = pal[p + 2];
        out[o + 3] = pal[p + 3];
      }
    }
    return out;
  }

  function boot() {
    var meta = JSON.parse(document.getElementById("meta").textContent), T = meta.t;
    var live = !!meta.live, scale = meta.scale || 1;
    var palHex = meta.palette || "", pal = palHex ? palette(palHex) : null, TB = meta.tileBlocks;
    var canvas = document.getElementById("map"), ctx = canvas.getContext("2d");
    var tiles = {}, count = 0, bounds = null, clock = 0, cached = 0;
    // A tile: `el` (exported page) or `text` (live page, null until fetched) holds its JSON
    function add(tx, tz, t) {
      tiles[tx + "," + tz] = t;
      count++;
      if (!bounds) bounds = { x0: tx, z0: tz, x1: tx, z1: tz };
      bounds.x0 = Math.min(bounds.x0, tx);
      bounds.z0 = Math.min(bounds.z0, tz);
      bounds.x1 = Math.max(bounds.x1, tx);
      bounds.z1 = Math.max(bounds.z1, tz);
    }
    var list = document.querySelectorAll("script.tile");
    for (var i = 0; i < list.length; i++) {
      var ex = parseInt(list[i].getAttribute("data-x"), 10), ez = parseInt(list[i].getAttribute("data-z"), 10);
      add(ex, ez, { el: list[i], text: null, canvas: null, step: 0, used: 0, bad: false, fresh: false });
    }
    var spawn = meta.spawn; // null where the world spawn means nothing (nether, End)
    var players = meta.players || []; // the ones drawn: of this page (export) or of the dimension shown (live)
    var view = { x: spawn ? spawn[0] : 0, z: spawn ? spawn[2] : 0, zoom: 2 };
    var grid = false, mark = null, raf = 0, hover = null;
    // The live page has no smaller copies of the map to show from far away: it stops at 4 blocks a pixel
    var MIN = live ? 1 / 4 : 1 / 64, MAX = 32;
    var MAXLOAD = 240, PARALLEL = 4, get = window.WorldMapGet;
    var index = null, cur = null, active = 0, everyone = [], follow = null, offline = false, started = false, hashTimer = 0;

    function $(id) { return document.getElementById(id); }
    function redraw() { if (!raf) raf = requestAnimationFrame(draw); }
    function clampZoom(z) { return Math.max(MIN, Math.min(MAX, z)); }

    function fit() {
      if (!bounds) return;
      var w = canvas.clientWidth, h = canvas.clientHeight;
      var bw = (bounds.x1 - bounds.x0 + 1) * TB, bh = (bounds.z1 - bounds.z0 + 1) * TB;
      view.x = bounds.x0 * TB + bw / 2;
      view.z = bounds.z0 * TB + bh / 2;
      view.zoom = clampZoom(Math.min(w / bw, h / bh) * 0.95);
      redraw();
    }

    // A move the viewer made themselves ends following a player
    function letGo() {
      if (!follow) return;
      follow = null;
      showPlayers();
    }

    function go(x, z, zoom) {
      view.x = x;
      view.z = z;
      if (zoom) view.zoom = clampZoom(zoom);
      redraw();
    }

    // Draw one tile into its own small canvas, at the detail the current zoom needs.
    function decode(t, step) {
      try {
        var tile = JSON.parse(t.el ? t.el.textContent : t.text), px = decodeTile(tile);
        while (step > 1 && tile.size / step < 1) step /= 2;
        var n = Math.floor(tile.size / step), c = document.createElement("canvas");
        c.width = n;
        c.height = n;
        var img = new ImageData(toRGBA(px, tile.size, pal, step), n, n);
        c.getContext("2d").putImageData(img, 0, 0);
        if (t.canvas) cached -= t.canvas.width * t.canvas.height;
        t.canvas = c;
        t.step = step;
        t.fresh = false;
        cached += n * n;
      } catch (e) {
        t.bad = true;
        if (window.console) console.error(e);
      }
    }

    // Keep at most ~24 million decoded pixels (about 100 MB); drop what was not drawn lately.
    function trim() {
      if (cached <= 24e6) return;
      var all = [];
      for (var k in tiles) if (tiles[k].canvas && tiles[k].used < clock) all.push(tiles[k]);
      all.sort(function (a, b) { return a.used - b.used; });
      for (var i = 0; i < all.length && cached > 16e6; i++) {
        cached -= all[i].canvas.width * all[i].canvas.height;
        all[i].canvas = null;
        // Live: the text goes too; the browser's cache has it if the tile is looked at again
        if (live) all[i].text = null;
      }
    }

    // Live: fetch the tiles in view that are not here yet, nearest to the middle first.
    function load(want) {
      if (!want.length || active >= PARALLEL) return;
      var mx = view.x / TB - 0.5, mz = view.z / TB - 0.5, now = Date.now();
      want.sort(function (a, b) { return Math.hypot(a.x - mx, a.z - mz) - Math.hypot(b.x - mx, b.z - mz); });
      for (var i = 0; i < want.length && i < MAXLOAD && active < PARALLEL; i++) {
        var t = want[i];
        if (!t.loading && now - t.failed > 10000) fetchTile(t, cur.dir);
      }
    }

    function fetchTile(t, dir) {
      var v = t.v;
      t.loading = true;
      active++;
      get("tiles/" + dir + "/" + t.x + "_" + t.z + ".json?v=" + v).then(function (r) {
        if (!r.ok) throw new Error("worldmap: tile " + r.status);
        return r.text();
      }).then(function (text) {
        // Drawn again since, or another dimension is shown now: not this text
        if (t.v === v && tiles[t.x + "," + t.z] === t) {
          t.text = text;
          t.fresh = true;
          t.bad = false;
        }
      }).catch(function () {
        t.failed = Date.now();
      }).then(function () {
        t.loading = false;
        active--;
        redraw();
      });
    }

    function line(x0, y0, x1, y1) {
      ctx.beginPath();
      ctx.moveTo(x0, y0);
      ctx.lineTo(x1, y1);
      ctx.stroke();
    }

    function label(text, x, y, color) {
      ctx.font = "600 12px system-ui, -apple-system, 'Segoe UI', sans-serif";
      ctx.textAlign = "center";
      ctx.lineWidth = 3;
      ctx.strokeStyle = "rgba(0,0,0,.75)";
      ctx.strokeText(text, x, y);
      ctx.fillStyle = color;
      ctx.fillText(text, x, y);
    }

    function draw() {
      raf = 0;
      clock++;
      var dpr = window.devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
      if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
        canvas.width = Math.round(w * dpr);
        canvas.height = Math.round(h * dpr);
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.fillStyle = "#10141b";
      ctx.fillRect(0, 0, w, h);
      var zoom = view.zoom, x0 = view.x - w / 2 / zoom, z0 = view.z - h / 2 / zoom;
      var sx = function (bx) { return Math.round((bx - x0) * zoom); };
      var sz = function (bz) { return Math.round((bz - z0) * zoom); };
      ctx.imageSmoothingEnabled = zoom < 1;
      var began = Date.now(), more = false, want = [];
      var tx0 = Math.floor(x0 / TB), tx1 = Math.floor((x0 + w / zoom) / TB);
      var tz0 = Math.floor(z0 / TB), tz1 = Math.floor((z0 + h / zoom) / TB);
      if (bounds) {
        tx0 = Math.max(tx0, bounds.x0);
        tx1 = Math.min(tx1, bounds.x1);
        tz0 = Math.max(tz0, bounds.z0);
        tz1 = Math.min(tz1, bounds.z1);
        for (var tz = tz0; tz <= tz1; tz++) {
          for (var tx = tx0; tx <= tx1; tx++) {
            var t = tiles[tx + "," + tz];
            if (!t) continue;
            var here = t.el || t.text !== null;
            // Live: not fetched yet (or drawn again on the server since); what is here is shown meanwhile
            if (!here) want.push(t);
            if (t.bad && here) continue;
            // One canvas pixel should not be smaller than one screen pixel
            var step = 1, per = scale * zoom;
            while (per * step * 2 <= 1 && step < 256) step *= 2;
            if (here && (!t.canvas || t.step > step || t.fresh)) {
              // Decoding takes a few ms a tile: do some now, the rest in the next frames
              if (Date.now() - began > 14) more = true; else decode(t, step);
            }
            if (!t.canvas) continue;
            t.used = clock;
            var l = sx(tx * TB), r = sx((tx + 1) * TB), u = sz(tz * TB), d = sz((tz + 1) * TB);
            ctx.drawImage(t.canvas, l, u, r - l, d - u);
          }
        }
      }
      if (grid) {
        ctx.lineWidth = 1;
        var steps = zoom >= 2 ? [16, 512] : zoom >= 0.06 ? [512] : [];
        for (var s = 0; s < steps.length; s++) {
          var g = steps[s];
          ctx.strokeStyle = g === 16 ? "rgba(255,255,255,.13)" : "rgba(255,255,255,.4)";
          for (var gx = Math.floor(x0 / g) * g; gx <= x0 + w / zoom; gx += g) line(sx(gx) + 0.5, 0, sx(gx) + 0.5, h);
          for (var gz = Math.floor(z0 / g) * g; gz <= z0 + h / zoom; gz += g) line(0, sz(gz) + 0.5, w, sz(gz) + 0.5);
        }
        ctx.strokeStyle = "rgba(255,210,90,.7)";
        line(sx(0) + 0.5, 0, sx(0) + 0.5, h);
        line(0, sz(0) + 0.5, w, sz(0) + 0.5);
      }
      if (spawn) {
        // World spawn: a diamond
        var px = sx(spawn[0] + 0.5), pz = sz(spawn[2] + 0.5);
        ctx.beginPath();
        ctx.moveTo(px, pz - 8);
        ctx.lineTo(px + 7, pz);
        ctx.lineTo(px, pz + 8);
        ctx.lineTo(px - 7, pz);
        ctx.closePath();
        ctx.fillStyle = "#ffd25a";
        ctx.strokeStyle = "#1b1300";
        ctx.lineWidth = 2;
        ctx.fill();
        ctx.stroke();
        label(T.spawn, px, pz - 13, "#ffd25a");
      }
      for (var p = 0; p < players.length; p++) {
        var pl = players[p], ax = sx(pl.x), az = sz(pl.z);
        ctx.beginPath();
        ctx.arc(ax, az, 5.5, 0, Math.PI * 2);
        ctx.fillStyle = pl.name === follow ? "#ffd25a" : "#5ad1ff";
        ctx.strokeStyle = "#00202e";
        ctx.lineWidth = 2;
        ctx.fill();
        ctx.stroke();
        label(pl.name, ax, az - 11, "#d9f4ff");
      }
      if (mark) {
        var mx = sx(mark.x + 0.5), mz = sz(mark.z + 0.5);
        ctx.strokeStyle = "#ff5a7a";
        ctx.lineWidth = 2;
        line(mx - 10, mz, mx + 10, mz);
        line(mx, mz - 10, mx, mz + 10);
        label(mark.x + ", " + mark.z, mx, mz - 14, "#ffc2cf");
      }
      var at = hover || { x: w / 2, y: h / 2 };
      var bx = Math.floor(x0 + at.x / zoom), bz = Math.floor(z0 + at.y / zoom);
      $("coords").textContent = "x " + bx + "   z " + bz + "   ·   chunk " + Math.floor(bx / 16) + ", " + Math.floor(bz / 16);
      trim();
      if (live) {
        load(want);
        if (started && !hashTimer) hashTimer = setTimeout(saveHash, 500);
      }
      if (more) redraw();
    }

    // Live: the address bar always names what is shown, so a link to it can be shared
    function saveHash() {
      hashTimer = 0;
      if (!cur) return;
      var z = Math.round(view.zoom * 1000) / 1000;
      var hash = "#" + cur.dir + "/" + Math.round(view.x) + "/" + Math.round(view.z) + "/" + z;
      if (location.hash !== hash) try { history.replaceState(null, "", hash); } catch (e) { /* a sandboxed frame */ }
    }

    // "#overworld/120/-48/2" -> what to show, or null
    function readHash() {
      var m = /^#([a-z0-9_-]{1,40})\/(-?\d{1,8})\/(-?\d{1,8})(?:\/(\d{1,3}(?:\.\d{1,4})?))?$/.exec(location.hash);
      return m ? { dir: m[1], x: parseInt(m[2], 10), z: parseInt(m[3], 10), zoom: m[4] ? parseFloat(m[4]) : 0 } : null;
    }

    function dimName(dir) { return (T.dims && T.dims[dir]) || dir; }

    function findDim(dir) {
      var all = index ? index.dimensions : [];
      for (var i = 0; i < all.length; i++) if (all[i].dir === dir) return all[i];
      return null;
    }

    // Live: show one dimension of the index. Tiles that are already here stay; one the server
    // has drawn again keeps its old picture until the new one has arrived.
    function useDim(d) {
      var old = cur && cur.dir === d.dir ? tiles : {}, k;
      tiles = {};
      count = 0;
      bounds = null;
      for (var i = 0; i < d.tiles.length; i++) {
        var m = d.tiles[i], t = old[m.x + "," + m.z];
        if (!t) t = { el: null, x: m.x, z: m.z, v: m.updated, text: null, canvas: null, step: 0, used: 0, bad: false, fresh: false, loading: false, failed: 0 };
        else if (t.v !== m.updated) {
          t.v = m.updated;
          t.text = null;
          t.failed = 0;
        }
        add(m.x, m.z, t);
      }
      for (k in old) if (!tiles[k] && old[k].canvas) cached -= old[k].canvas.width * old[k].canvas.height;
      cur = d;
      // The server gives every dimension the overworld's spawn point: only show it there
      spawn = d.dir === "overworld" ? d.spawn : null;
      $("home").hidden = !spawn;
      $("empty").textContent = count ? "" : T.empty;
      document.title = meta.title + " · " + dimName(d.dir);
      $("title").textContent = document.title;
      var btns = $("dims").children;
      for (var b = 0; b < btns.length; b++) btns[b].setAttribute("aria-pressed", btns[b].getAttribute("data-dir") === d.dir ? "true" : "false");
      showPlayers();
      showInfo();
      redraw();
    }

    function showInfo() {
      if (!cur) return;
      var when = new Date(index.updated * 1000).toLocaleTimeString(meta.lang === "th" ? "th-TH" : "en-GB");
      var info = [T.chunks.replace("{n}", cur.chunks.toLocaleString()), T.live.replace("{t}", when)];
      if (offline) info.push(T.offline);
      $("info").textContent = info.join(" · ");
    }

    function setIndex(ix) {
      if (!ix || !ix.dimensions || !(ix.tile_px > 0)) throw new Error("worldmap: bad index");
      index = ix;
      TB = ix.tile_px;
      if (ix.palette !== palHex) {
        // Another plugin version: every picture is drawn again with its colours
        palHex = ix.palette;
        pal = palette(palHex);
        for (var k in tiles) tiles[k].fresh = true;
      }
      var box = $("dims"), dirs = ix.dimensions.map(function (d) { return d.dir; }).join(",");
      if (box.getAttribute("data-dirs") !== dirs) {
        box.setAttribute("data-dirs", dirs);
        box.textContent = "";
        ix.dimensions.forEach(function (d) {
          var b = document.createElement("button");
          b.type = "button";
          b.textContent = dimName(d.dir);
          b.setAttribute("data-dir", d.dir);
          b.addEventListener("click", function () {
            var to = findDim(d.dir);
            if (!to || (cur && cur.dir === to.dir)) return;
            follow = null;
            useDim(to);
            if (!start(null)) fit();
          });
          box.appendChild(b);
        });
        box.hidden = ix.dimensions.length < 2;
      }
      var hash = started ? null : readHash();
      var d = findDim(cur ? cur.dir : hash ? hash.dir : "overworld") || ix.dimensions[0];
      if (!d) {
        $("empty").textContent = T.empty;
        return;
      }
      useDim(d);
      if (!started) {
        started = true;
        if (!start(hash && hash.dir === d.dir ? hash : null) && bounds) fit();
      }
    }

    // Where a dimension is first looked at: the place in the link, else the spawn if it is on the map.
    function start(hash) {
      if (hash) {
        go(hash.x, hash.z, hash.zoom || 2);
        return true;
      }
      if (spawn && tiles[Math.floor(spawn[0] / TB) + "," + Math.floor(spawn[2] / TB)]) {
        go(spawn[0], spawn[2], 2);
        return true;
      }
      return false;
    }

    function ask(url, then) {
      return get(url, { cache: "no-cache" }).then(function (r) {
        if (!r.ok) throw new Error("worldmap: " + url + " " + r.status);
        return r.json();
      }).then(function (data) {
        then(data);
        if (offline) {
          offline = false;
          showInfo();
        }
      }).catch(function (e) {
        if (!offline) {
          offline = true;
          if (cur) showInfo(); else $("empty").textContent = T.offline;
        }
        if (window.console) console.error(e);
      });
    }

    // Live: the players of the dimension shown are drawn; the list has everyone.
    function showPlayers() {
      var dir = cur ? cur.dir : "";
      players = everyone.filter(function (p) { return p.dim === dir; });
      var box = $("players"), sig = everyone.map(function (p) { return p.name + "@" + p.dim; }).join(",") + "|" + follow;
      if (box.getAttribute("data-sig") !== sig) {
        box.setAttribute("data-sig", sig);
        box.textContent = "";
        if (everyone.length) {
          var head = document.createElement("div");
          head.className = "head";
          head.textContent = T.players.replace("{n}", everyone.length);
          box.appendChild(head);
        }
        everyone.forEach(function (p) {
          var b = document.createElement("button");
          b.type = "button";
          b.textContent = p.dim === dir ? p.name : p.name + " · " + dimName(p.dim);
          b.setAttribute("aria-pressed", p.name === follow ? "true" : "false");
          // Press a name to go there and stay with that player; press again to let go
          b.addEventListener("click", function () {
            follow = follow === p.name ? null : p.name;
            showPlayers();
            track();
            redraw();
          });
          box.appendChild(b);
        });
      }
    }

    function track() {
      if (!follow) return;
      for (var i = 0; i < everyone.length; i++) {
        var p = everyone[i];
        if (p.name !== follow) continue;
        if (!cur || p.dim !== cur.dir) {
          var to = findDim(p.dim);
          if (!to) return;
          useDim(to);
        }
        go(p.x, p.z, Math.max(view.zoom, 2));
        return;
      }
      // Left the server
      follow = null;
      showPlayers();
    }

    function poll() {
      var hidden = document.hidden, now = Date.now();
      if (!hidden && now - poll.index >= 30000) {
        poll.index = now;
        ask("index.json", setIndex);
      }
      if (!hidden && meta.showPlayers && now - poll.players >= 2000) {
        poll.players = now;
        ask("players.json", function (data) {
          everyone = (data.players || []).filter(function (p) { return p && typeof p.name === "string" && isFinite(p.x) && isFinite(p.z); });
          everyone.sort(function (a, b) { return a.name < b.name ? -1 : a.name > b.name ? 1 : 0; });
          showPlayers();
          track();
          redraw();
        });
      }
    }
    poll.index = -1e9;
    poll.players = -1e9;

    function zoomAt(factor, cx, cy) {
      var w = canvas.clientWidth, h = canvas.clientHeight, old = view.zoom, next = clampZoom(old * factor);
      if (cx === undefined) { cx = w / 2; cy = h / 2; }
      // The block under (cx, cy) stays under it
      view.x += (cx - w / 2) / old - (cx - w / 2) / next;
      view.z += (cy - h / 2) / old - (cy - h / 2) / next;
      view.zoom = next;
      redraw();
    }

    var pointers = {}, pinch = 0;
    function pts() { var a = []; for (var k in pointers) a.push(pointers[k]); return a; }
    canvas.addEventListener("pointerdown", function (e) {
      canvas.setPointerCapture(e.pointerId);
      pointers[e.pointerId] = { x: e.clientX, y: e.clientY };
      var a = pts();
      pinch = a.length === 2 ? Math.hypot(a[0].x - a[1].x, a[0].y - a[1].y) : 0;
      canvas.classList.add("drag");
    });
    canvas.addEventListener("pointermove", function (e) {
      var rect = canvas.getBoundingClientRect();
      hover = e.pointerType === "mouse" ? { x: e.clientX - rect.left, y: e.clientY - rect.top } : null;
      var p = pointers[e.pointerId];
      if (p) {
        var a = pts();
        if (a.length === 1) {
          view.x -= (e.clientX - p.x) / view.zoom;
          view.z -= (e.clientY - p.y) / view.zoom;
          if (e.clientX !== p.x || e.clientY !== p.y) letGo();
        }
        p.x = e.clientX;
        p.y = e.clientY;
        if (a.length === 2) {
          var dist = Math.hypot(a[0].x - a[1].x, a[0].y - a[1].y);
          if (pinch > 0 && dist > 0) zoomAt(dist / pinch, (a[0].x + a[1].x) / 2 - rect.left, (a[0].y + a[1].y) / 2 - rect.top);
          pinch = dist;
        }
      }
      redraw();
    });
    function up(e) {
      delete pointers[e.pointerId];
      pinch = 0;
      if (!pts().length) canvas.classList.remove("drag");
    }
    canvas.addEventListener("pointerup", up);
    canvas.addEventListener("pointercancel", up);
    canvas.addEventListener("pointerleave", function () { hover = null; redraw(); });
    canvas.addEventListener("wheel", function (e) {
      e.preventDefault();
      var rect = canvas.getBoundingClientRect(), delta = e.deltaMode === 1 ? e.deltaY * 33 : e.deltaY;
      zoomAt(Math.exp(-delta * 0.0016), e.clientX - rect.left, e.clientY - rect.top);
    }, { passive: false });
    window.addEventListener("resize", redraw);
    window.addEventListener("keydown", function (e) {
      if (e.target && /INPUT|TEXTAREA/.test(e.target.tagName)) return;
      var move = 80 / view.zoom;
      if (e.key.indexOf("Arrow") === 0) letGo();
      if (e.key === "+" || e.key === "=") zoomAt(1.5);
      else if (e.key === "-" || e.key === "_") zoomAt(1 / 1.5);
      else if (e.key === "ArrowLeft") go(view.x - move, view.z);
      else if (e.key === "ArrowRight") go(view.x + move, view.z);
      else if (e.key === "ArrowUp") go(view.x, view.z - move);
      else if (e.key === "ArrowDown") go(view.x, view.z + move);
      else if (e.key === "g" || e.key === "G") $("grid").click();
    });

    document.title = meta.title;
    $("title").textContent = meta.title;
    if (!live) {
      var when = new Date(meta.generated * 1000);
      var info = [T.chunks.replace("{n}", meta.chunks.toLocaleString()), T.made.replace("{t}", when.toLocaleString(meta.lang === "th" ? "th-TH" : "en-GB"))];
      if (scale > 1) info.push(T.reduced.replace("{n}", scale));
      info.push(T.snapshot);
      $("info").textContent = info.join(" · ");
    }
    $("grid").textContent = T.grid;
    $("home").textContent = T.spawn;
    $("fit").textContent = T.fit;
    $("gobtn").textContent = T.go;
    $("gotolabel").textContent = T.goto;
    $("grid").addEventListener("click", function () {
      grid = !grid;
      this.setAttribute("aria-pressed", grid ? "true" : "false");
      redraw();
    });
    $("home").addEventListener("click", function () {
      letGo();
      if (spawn) go(spawn[0], spawn[2], Math.max(view.zoom, 2));
    });
    $("home").hidden = !spawn;
    $("fit").addEventListener("click", function () {
      letGo();
      fit();
    });
    $("zin").addEventListener("click", function () { zoomAt(1.6); });
    $("zout").addEventListener("click", function () { zoomAt(1 / 1.6); });
    $("goto").addEventListener("submit", function (e) {
      e.preventDefault();
      var x = Math.round(parseFloat($("gx").value)), z = Math.round(parseFloat($("gz").value));
      if (!isFinite(x) || !isFinite(z)) return;
      letGo();
      mark = { x: x, z: z };
      go(x, z, Math.max(view.zoom, 2));
    });
    if (live) {
      $("empty").textContent = T.loading;
      window.addEventListener("hashchange", function () {
        var hash = readHash(), to = hash && findDim(hash.dir);
        if (!to) return;
        follow = null;
        if (!cur || cur.dir !== to.dir) useDim(to);
        go(hash.x, hash.z, hash.zoom || view.zoom);
      });
      document.addEventListener("visibilitychange", poll);
      setInterval(poll, 1000);
      poll();
      return;
    }
    var box = $("players");
    if (players.length) {
      var head = document.createElement("div");
      head.className = "head";
      head.textContent = T.players.replace("{n}", players.length);
      box.appendChild(head);
      players.forEach(function (pl) {
        var b = document.createElement("button");
        b.type = "button";
        b.textContent = pl.name;
        b.title = Math.floor(pl.x) + ", " + Math.floor(pl.z);
        b.addEventListener("click", function () { go(pl.x, pl.z, Math.max(view.zoom, 2)); });
        box.appendChild(b);
      });
    }
    if (!count) $("empty").textContent = T.empty;
    // Start at the spawn if it is on the map, else show everything
    var here = spawn && tiles[Math.floor(spawn[0] / TB) + "," + Math.floor(spawn[2] / TB)];
    if (!here && bounds) fit();
    redraw();
  }

  return { base64: base64, inflate: inflate, decodeTile: decodeTile, palette: palette, toRGBA: toRGBA, boot: boot };
})();
if (typeof document !== "undefined") WorldMap.boot();
