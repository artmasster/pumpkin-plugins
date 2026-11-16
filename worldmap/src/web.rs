//! The live map page: which path asks for what. The HTTP server itself is the shared
//! `mcsv-web` crate (re-exported here), driven from the server tick.
pub use mcsv_web::*;

use crate::store::valid_dir;

/// What a path asks for.
#[derive(Debug, PartialEq)]
pub enum Route {
    Page,
    Index,
    Players,
    /// A tile of the dimension folder `dir`.
    Tile { dir: String, x: i32, z: i32 },
    Unknown,
}

/// Paths are matched whole: nothing from the request ever becomes part of a file path except a
/// dimension folder name that passed [`valid_dir`] and two numbers.
pub fn route(path: &str) -> Route {
    match path {
        "/" | "/index.html" => return Route::Page,
        "/index.json" => return Route::Index,
        "/players.json" => return Route::Players,
        _ => {}
    }
    let Some(rest) = path.strip_prefix("/tiles/") else { return Route::Unknown };
    let Some((dir, name)) = rest.split_once('/') else { return Route::Unknown };
    let Some((x, z)) = name.strip_suffix(".json").and_then(|n| n.split_once('_')) else { return Route::Unknown };
    let (Ok(x), Ok(z)) = (x.parse::<i32>(), z.parse::<i32>()) else { return Route::Unknown };
    // Only the one spelling the store writes (`+1`, `01` and the like are not it)
    if !valid_dir(dir) || format!("{x}_{z}.json") != name {
        return Route::Unknown;
    }
    Route::Tile { dir: dir.to_owned(), x, z }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_never_leave_the_map() {
        assert_eq!(route("/"), Route::Page);
        assert_eq!(route("/index.html"), Route::Page);
        assert_eq!(route("/index.json"), Route::Index);
        assert_eq!(route("/players.json"), Route::Players);
        assert_eq!(route("/tiles/overworld/0_-1.json"), Route::Tile { dir: "overworld".into(), x: 0, z: -1 });
        assert_eq!(route("/tiles/the_nether/-12_340.json"), Route::Tile { dir: "the_nether".into(), x: -12, z: 340 });
        for bad in [
            "/tiles/../config.toml",
            "/tiles/overworld/../../config.toml",
            "/tiles/overworld/0_0.json/..",
            "/tiles/over world/0_0.json",
            "/tiles/Overworld/0_0.json",
            "/tiles//0_0.json",
            "/tiles/overworld/0_0",
            "/tiles/overworld/+1_0.json",
            "/tiles/overworld/01_0.json",
            "/tiles/overworld/0_0_0.json",
            "/tiles/overworld/99999999999_0.json",
            "/tiles/overworld/a/0_0.json",
            "/config.toml",
            "/status.json",
            "/map.html",
            "//",
            "/index.json/",
            "/%2e%2e/config.toml",
            "",
        ] {
            assert_eq!(route(bad), Route::Unknown, "{bad}");
        }
    }
}
