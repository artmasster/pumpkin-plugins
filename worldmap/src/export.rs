//! The exported page: one HTML file with the tiles inside and a small viewer.
//!
//! ```text
//! <head> … style …
//! <script id="meta" type="application/json">{…}</script>                    what, where, labels, palette
//! <script class="tile" type="application/json" data-x="0" data-z="-1">{…}</script>   one per tile
//! <script>viewer</script>
//! ```
//!
//! A tile block is the tile file as it is on disk (full resolution) or the same shape at a smaller
//! `size` (reduced resolution). The page asks the network for nothing: no script, font or image
//! comes from outside, and its Content-Security-Policy forbids it.
use serde::Serialize;

use crate::tile;

const VIEWER: &str = include_str!("viewer.js");

const STYLE: &str = "html,body{margin:0;height:100%;overflow:hidden;background:#10141b;color:#e8edf4;font:14px/1.45 system-ui,-apple-system,'Segoe UI','Leelawadee UI',Tahoma,sans-serif}\
#map{position:fixed;inset:0;width:100%;height:100%;touch-action:none;cursor:grab}#map.drag{cursor:grabbing}\
#panel{position:fixed;top:12px;left:12px;max-width:min(340px,calc(100vw - 24px));max-height:calc(100vh - 70px);overflow:auto;box-sizing:border-box;padding:12px 14px;background:rgba(18,23,32,.88);border:1px solid rgba(255,255,255,.12);border-radius:12px;backdrop-filter:blur(6px);box-shadow:0 8px 28px rgba(0,0,0,.45)}\
#title{font-weight:700;font-size:16px;letter-spacing:.01em}#info{margin:2px 0 10px;color:#9fb0c3;font-size:12px}\
.row{display:flex;flex-wrap:wrap;gap:6px;margin-top:6px;align-items:center}\
button,input{font:inherit;color:inherit;background:#222b38;border:1px solid rgba(255,255,255,.16);border-radius:8px;padding:6px 10px;min-height:34px;box-sizing:border-box}\
button{cursor:pointer}button:hover{background:#2c394a}button[aria-pressed=true]{background:#2f6fde;border-color:#6aa0ff}\
button:focus-visible,input:focus-visible{outline:2px solid #6aa0ff;outline-offset:1px}\
input{width:84px}#gotolabel{color:#9fb0c3;font-size:12px;width:100%;margin-top:4px}\
#players .head{color:#9fb0c3;font-size:12px;margin:10px 0 4px}#players button{margin:0 6px 6px 0;border-color:rgba(90,209,255,.45)}\
#players button[aria-pressed=true]{background:#7a5a10;border-color:#ffd25a}#dims{margin:0 0 4px}[hidden]{display:none!important}\
#coords{position:fixed;left:12px;bottom:12px;padding:6px 12px;background:rgba(18,23,32,.88);border:1px solid rgba(255,255,255,.12);border-radius:8px;font:13px/1.4 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;white-space:pre}\
#empty{position:fixed;inset:0;display:flex;align-items:center;justify-content:center;pointer-events:none;color:#9fb0c3;padding:24px;text-align:center}";

/// File name of the exported page of a dimension folder.
pub fn file_name(dir: &str) -> String {
    match dir {
        "overworld" => "map.html".into(),
        "the_nether" => "map-nether.html".into(),
        "the_end" => "map-end.html".into(),
        other => format!("map-{other}.html"),
    }
}

/// How far the map has to be shrunk (1, 2, 4, 8 or 16 blocks per pixel) so that `tile_bytes`
/// of tile files fit in a page of `cap` bytes that already has `fixed` bytes of viewer in it.
///
/// Halving the resolution leaves a quarter of the pixels, but neighbouring pixels then differ
/// more and compress worse, so a shrunk tile is counted as twice its share. The export checks
/// the real size while writing and goes one step further if this guess was too kind.
pub fn pick_scale(tile_bytes: u64, fixed: u64, cap: u64) -> u32 {
    let room = cap.saturating_sub(fixed);
    let mut scale = 1u32;
    while scale < MAX_SCALE && estimate(tile_bytes, scale) > room {
        scale *= 2;
    }
    scale
}

pub const MAX_SCALE: u32 = 16;

/// Guess of the tile bytes in a page at `scale`.
pub fn estimate(tile_bytes: u64, scale: u32) -> u64 {
    if scale <= 1 { tile_bytes } else { tile_bytes.saturating_mul(2) / u64::from(scale * scale) }
}

#[derive(Clone, Debug, Serialize)]
pub struct Marker {
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Everything the viewer is told.
#[derive(Clone, Debug)]
pub struct Meta {
    pub th: bool,
    /// `minecraft:overworld`
    pub dim: String,
    pub version: String,
    pub generated: u64,
    pub scale: u32,
    /// World spawn; `None` where the server's spawn point means nothing (nether, End).
    pub spawn: Option<[i32; 3]>,
    pub players: Vec<Marker>,
    pub chunks: u64,
    pub palette: String,
}

/// Text in a `<script>` block must not be able to close it.
fn script_safe(json: &str) -> String {
    json.replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026")
}

fn html_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn title(meta: &Meta) -> String {
    let d = meta.dim.rsplit(':').next().unwrap_or("");
    let name = match (d, meta.th) {
        ("overworld", true) => "โลกหลัก".to_owned(),
        ("the_nether", true) => "เนเธอร์".to_owned(),
        ("the_end", true) => "ดิเอนด์".to_owned(),
        ("overworld", false) => "Overworld".to_owned(),
        ("the_nether", false) => "Nether".to_owned(),
        ("the_end", false) => "The End".to_owned(),
        (other, _) => other.to_owned(),
    };
    format!("WorldMap · {name}")
}

/// Everything of a page before its tiles: `policy` is what it may ask the network for, `meta` what the viewer is told.
fn shell(th: bool, version: &str, title: &str, policy: &str, meta: &serde_json::Value) -> String {
    format!(
        "<!doctype html>\n<html lang=\"{lang}\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no\">\n\
<meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\">\n\
<meta name=\"referrer\" content=\"no-referrer\">\n<meta name=\"generator\" content=\"MCSV WorldMap {version}\">\n<title>{title}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n\
<canvas id=\"map\"></canvas>\n<div id=\"empty\"></div>\n\
<div id=\"panel\"><div id=\"title\"></div><div id=\"info\"></div><div id=\"dims\" class=\"row\" hidden></div>\
<div class=\"row\"><button type=\"button\" id=\"grid\" aria-pressed=\"false\"></button><button type=\"button\" id=\"home\"></button><button type=\"button\" id=\"fit\"></button>\
<button type=\"button\" id=\"zin\" aria-label=\"zoom in\">+</button><button type=\"button\" id=\"zout\" aria-label=\"zoom out\">&minus;</button></div>\
<form id=\"goto\" class=\"row\"><div id=\"gotolabel\"></div><input id=\"gx\" type=\"number\" step=\"1\" placeholder=\"x\" aria-label=\"x\"><input id=\"gz\" type=\"number\" step=\"1\" placeholder=\"z\" aria-label=\"z\"><button id=\"gobtn\"></button></form>\
<div id=\"players\"></div></div>\n<div id=\"coords\"></div>\n\
<script id=\"meta\" type=\"application/json\">{meta}</script>\n",
        lang = if th { "th" } else { "en" },
        version = html_text(version),
        title = html_text(title),
        meta = script_safe(&meta.to_string()),
    )
}

/// What the exported page may load: nothing from anywhere.
pub const EXPORT_POLICY: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:";
/// The live page also asks the server it came from (and nobody else) for the map.
pub const LIVE_POLICY: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'self'";

/// The page of the plugin's own web server: the same viewer as the exported page, without tiles
/// in it. It fetches `index.json`, the tiles in view and (`players`) `players.json` from the
/// server it came from.
pub fn live_page(th: bool, version: &str, players: bool) -> String {
    let t = if th {
        serde_json::json!({
            "grid": "เส้นกริด", "spawn": "จุดเกิด", "fit": "ทั้งแผนที่", "go": "ไป", "goto": "ไปที่พิกัด x z",
            "players": "ผู้เล่นออนไลน์ ({n})", "chunks": "{n} chunks", "live": "แผนที่สด ข้อมูลล่าสุด {t}",
            "offline": "ติดต่อเซิร์ฟเวอร์ไม่ได้ กำลังลองใหม่", "loading": "กำลังโหลดแผนที่…",
            "empty": "ยังไม่มีพื้นที่ที่ทำแผนที่ในมิตินี้",
            "dims": { "overworld": "โลกหลัก", "the_nether": "เนเธอร์", "the_end": "ดิเอนด์" },
        })
    } else {
        serde_json::json!({
            "grid": "Grid", "spawn": "Spawn", "fit": "Whole map", "go": "Go", "goto": "Go to x z",
            "players": "Players online ({n})", "chunks": "{n} chunks", "live": "live map, last change {t}",
            "offline": "The server cannot be reached, trying again", "loading": "Loading the map…",
            "empty": "Nothing of this dimension has been mapped yet",
            "dims": { "overworld": "Overworld", "the_nether": "Nether", "the_end": "The End" },
        })
    };
    let json = serde_json::json!({
        "live": true, "title": "WorldMap", "lang": if th { "th" } else { "en" }, "version": version,
        "scale": 1, "tileBlocks": tile::SIZE, "spawn": null, "players": [], "showPlayers": players, "t": t,
    });
    // The viewer reaches the network only through this: the exported page does not have it
    shell(th, version, "WorldMap", LIVE_POLICY, &json) + "<script>window.WorldMapGet=function(u,o){return fetch(u,o)};</script>\n" + &tail()
}

/// The page up to the first tile.
pub fn head(meta: &Meta) -> String {
    let t = if meta.th {
        serde_json::json!({
            "grid": "เส้นกริด", "spawn": "จุดเกิด", "fit": "ทั้งแผนที่", "go": "ไป", "goto": "ไปที่พิกัด x z",
            "players": "ผู้เล่นตอน export ({n})", "chunks": "{n} chunks", "made": "สร้างเมื่อ {t}",
            "reduced": "ย่อความละเอียด 1 จุด = {n} บล็อก", "snapshot": "ภาพนิ่ง ณ เวลาที่ export",
            "empty": "ยังไม่มีพื้นที่ที่ทำแผนที่ในมิตินี้",
        })
    } else {
        serde_json::json!({
            "grid": "Grid", "spawn": "Spawn", "fit": "Whole map", "go": "Go", "goto": "Go to x z",
            "players": "Players at export time ({n})", "chunks": "{n} chunks", "made": "made {t}",
            "reduced": "reduced: 1 pixel = {n} blocks", "snapshot": "a snapshot from export time",
            "empty": "Nothing of this dimension has been mapped yet",
        })
    };
    let title = title(meta);
    let json = serde_json::json!({
        "title": title, "lang": if meta.th { "th" } else { "en" }, "dim": meta.dim, "version": meta.version,
        "generated": meta.generated, "scale": meta.scale, "tileBlocks": tile::SIZE, "spawn": meta.spawn,
        "players": meta.players, "chunks": meta.chunks, "palette": meta.palette, "t": t,
    });
    shell(meta.th, &meta.version, &title, EXPORT_POLICY, &json)
}

/// One tile of the page. `None` when the text could break out of its block (a tile file that
/// somebody edited): such a tile is left out.
pub fn tile_block(x: i32, z: i32, json: &str) -> Option<String> {
    let json = json.trim();
    if !json.starts_with('{') || !json.ends_with('}') || json.contains('<') {
        return None;
    }
    Some(format!("<script class=\"tile\" type=\"application/json\" data-x=\"{x}\" data-z=\"{z}\">{json}</script>\n"))
}

/// The rest of the page: the viewer.
pub fn tail() -> String {
    format!("<script>\n{VIEWER}</script>\n</body>\n</html>\n")
}

/// Bytes of a page without tiles.
pub fn fixed_bytes(meta: &Meta) -> u64 {
    (head(meta).len() + tail().len()) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color;

    fn meta(th: bool) -> Meta {
        Meta {
            th,
            dim: "minecraft:overworld".into(),
            version: "1.2.3".into(),
            generated: 1_790_000_000,
            scale: 1,
            spawn: Some([8, 64, -40]),
            players: vec![Marker { name: "Alice".into(), x: 1.5, y: 70.0, z: -3.25 }],
            chunks: 1234,
            palette: color::palette_hex(),
        }
    }

    fn sample(th: bool) -> String {
        let mut t = tile::Tile::new();
        t.set_chunk(0, 0, &[35; 256], 10);
        let mut page = head(&meta(th));
        page.push_str(&tile_block(0, -1, &t.to_json("minecraft:overworld", 0, -1, 10)).unwrap());
        page.push_str(&tail());
        page
    }

    #[test]
    fn the_page_asks_the_network_for_nothing() {
        for th in [true, false] {
            let page = sample(th);
            let lower = page.to_lowercase();
            for bad in ["http://", "https://", "//cdn", "src=", "href=", "@import", "url(", "fetch(", "xmlhttprequest", "websocket", "import(", "sendbeacon", "<link", "<img", "<iframe"] {
                assert!(!lower.contains(bad), "page contains {bad}");
            }
            assert!(page.contains("default-src 'none'"));
        }
    }

    #[test]
    fn the_page_is_whole() {
        let page = sample(true);
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.trim_end().ends_with("</html>"));
        assert_eq!(page.matches("<script").count(), page.matches("</script>").count());
        assert_eq!(page.matches("<script").count(), 3);
        assert!(page.contains("data-x=\"0\" data-z=\"-1\""));
        assert!(page.contains("จุดเกิด") && page.contains("WorldMap · โลกหลัก"));
        assert!(super::page_ids().iter().all(|id| page.contains(&format!("id=\"{id}\""))));
        let en = sample(false);
        assert!(en.contains("\"spawn\":\"Spawn\"") && en.contains("lang=\"en\"") && !en.contains("จุดเกิด"));
        // The meta block is JSON the viewer can read
        let start = page.find("type=\"application/json\">").unwrap() + 24;
        let end = start + page[start..].find("</script>").unwrap();
        let v: serde_json::Value = serde_json::from_str(&page[start..end]).unwrap();
        assert_eq!(v["spawn"][2], -40);
        let mut nether = meta(true);
        nether.spawn = None;
        assert!(head(&nether).contains("\"spawn\":null"));
        assert_eq!(v["players"][0]["name"], "Alice");
        assert_eq!(v["tileBlocks"], 512);
        assert_eq!(v["palette"].as_str().unwrap().len(), 1536);
    }

    #[test]
    fn the_live_page_asks_only_its_own_server() {
        for th in [true, false] {
            let page = live_page(th, "1.2.3", true);
            assert!(page.starts_with("<!doctype html>") && page.trim_end().ends_with("</html>"));
            assert_eq!(page.matches("<script").count(), 3);
            assert_eq!(page.matches("</script>").count(), 3);
            assert!(page.contains("connect-src 'self'") && page.contains("default-src 'none'"));
            assert!(super::page_ids().iter().all(|id| page.contains(&format!("id=\"{id}\""))));
            let lower = page.to_lowercase();
            for bad in ["http://", "https://", "//cdn", "src=", "href=", "@import", "url(", "xmlhttprequest", "websocket", "import(", "sendbeacon", "<link", "<img", "<iframe"] {
                assert!(!lower.contains(bad), "page contains {bad}");
            }
            // One way to the network, and only relative addresses go through it
            assert_eq!(page.matches("fetch(").count(), 1);
            let start = page.find("type=\"application/json\">").unwrap() + 24;
            let end = start + page[start..].find("</script>").unwrap();
            let v: serde_json::Value = serde_json::from_str(&page[start..end]).unwrap();
            assert_eq!((v["live"].as_bool(), v["showPlayers"].as_bool(), v["tileBlocks"].as_u64()), (Some(true), Some(true), Some(512)));
            assert_eq!(v["t"]["dims"]["the_nether"], if th { "เนเธอร์" } else { "Nether" });
            // Every label the viewer reads on the live page
            for key in ["grid", "spawn", "fit", "go", "goto", "players", "chunks", "live", "offline", "loading", "empty"] {
                assert!(v["t"][key].is_string(), "{key}");
            }
        }
        assert!(live_page(true, "1.2.3", false).contains("\"showPlayers\":false"));
        // The exported page has no way to the network at all
        assert!(!tail().contains("fetch(") && !head(&meta(true)).contains("WorldMapGet="));
    }

    #[test]
    fn hostile_text_cannot_leave_its_block() {
        let mut m = meta(false);
        m.players[0].name = "</script><script>alert(1)</script>".into();
        m.dim = "x:<b>&".into();
        let h = head(&m);
        assert_eq!(h.matches("</script>").count(), 1);
        assert!(!h.contains("<b>"));
        assert!(tile_block(0, 0, "{\"a\":\"</script>\"}").is_none());
        assert!(tile_block(0, 0, "nonsense").is_none());
        assert!(tile_block(0, 0, " {\"a\":1}\n").is_some());
    }

    #[test]
    fn file_names_per_dimension() {
        assert_eq!(file_name("overworld"), "map.html");
        assert_eq!(file_name("the_nether"), "map-nether.html");
        assert_eq!(file_name("the_end"), "map-end.html");
        assert_eq!(file_name("skylands"), "map-skylands.html");
    }

    #[test]
    fn big_maps_are_shrunk_to_fit() {
        let mb = 1024 * 1024;
        // Fits as it is
        assert_eq!(pick_scale(10 * mb, 40_000, 30 * mb), 1);
        assert_eq!(pick_scale(0, 40_000, 30 * mb), 1);
        // Just over: one step is enough (a half-resolution tile counts as half the bytes)
        assert_eq!(pick_scale(31 * mb, 40_000, 30 * mb), 2);
        assert_eq!(pick_scale(59 * mb, 40_000, 30 * mb), 2);
        assert_eq!(pick_scale(61 * mb, 40_000, 30 * mb), 4);
        assert_eq!(pick_scale(239 * mb, 40_000, 30 * mb), 4);
        assert_eq!(pick_scale(241 * mb, 40_000, 30 * mb), 8);
        // Never past the last step, even when nothing can fit
        assert_eq!(pick_scale(u64::MAX, 40_000, 30 * mb), MAX_SCALE);
        assert_eq!(pick_scale(5 * mb, 2 * mb, mb), MAX_SCALE);
        assert_eq!(estimate(100, 1), 100);
        assert_eq!(estimate(128, 2), 64);
        assert_eq!(estimate(128, 4), 16);
        assert_eq!(estimate(128, 16), 1);
        // The chosen scale always fits by the estimate, and the one before it does not
        for total in [1u64, 20, 33, 70, 150, 900, 5000] {
            let s = pick_scale(total * mb, 50_000, 30 * mb);
            if s < MAX_SCALE {
                assert!(estimate(total * mb, s) <= 30 * mb - 50_000);
            }
            if s > 1 {
                assert!(estimate(total * mb, s / 2) > 30 * mb - 50_000);
            }
        }
    }

    #[test]
    fn a_shrunk_tile_fits_the_same_block() {
        let px = vec![7u8; tile::SIZE * tile::SIZE];
        let small = tile::downsample(&px, tile::SIZE, 8);
        let n = tile::SIZE / 8;
        let json = tile::export_json(3, -4, n, n, &tile::pack_bands(&small, n, n));
        let block = tile_block(3, -4, &json).unwrap();
        assert!(block.contains("\"size\":64") && block.contains("data-x=\"3\""));
        assert!(fixed_bytes(&meta(true)) > 10_000);
    }
}

/// Elements the viewer looks up by id (kept next to the markup so a test can hold them together).
#[cfg(test)]
fn page_ids() -> [&'static str; 17] {
    ["map", "empty", "panel", "title", "info", "dims", "grid", "home", "fit", "zin", "zout", "goto", "gotolabel", "gx", "gz", "gobtn", "players"]
}
