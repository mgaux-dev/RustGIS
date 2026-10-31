//! Add From Service's Remove: which layers of a service's listing (`service.layers`) are on the
//! map already, and the command that removes each. A helper for the dialog, not a command of its
//! own (`cmd::service` lists and adds the layers). A listed layer is known by the address it came
//! from (`service::mark_service_source`, a tile layer's template), never by its name alone, so a
//! file's layer or another service's that shares the title is left alone.

use rustgis_map::Map;
use serde_json::{Value, json};

/// A command and its parameters (`layer.remove {"layer"}`, `map.removeTiles {"index"}`).
pub type Removal = (&'static str, Value);

/// For each layer of `listed` (a `service.layers` listing's `layers`) whose place is in `rows`,
/// the command that removes it from `map` when it is there; `None` for the others, and for every
/// row not asked about. Rows naming one layer each offer it; a tile layer is claimed by one OGC
/// API row at most (`claim_tilesets`).
pub fn added(map: &Map, listed: &[Value], rows: &[usize]) -> Vec<Option<Removal>> {
    // Each layer's source and each tile layer's address as they are and without their keys, once:
    // a project saved and opened again keeps its addresses without them (the listing's still carry
    // them), and a copy of the same service added under another key is still known.
    let sources: Vec<Source<'_>> =
        map.layers.iter().filter_map(|l| l.dataset.source.as_deref().map(|s| (l.id, s, crate::sources::redact(s)))).collect();
    let tiles: Vec<Tile<'_>> = map.tiles.iter().enumerate().map(|(i, t)| (i, t.url.as_str(), crate::sources::redact(&t.url))).collect();
    let mut added = vec![None; listed.len()];
    for &i in rows {
        if let (Some(l), Some(slot)) = (listed.get(i), added.get_mut(i)) {
            *slot = removal(&sources, &tiles, l);
        }
    }
    claim_tilesets(listed, rows, &tiles, &mut added);
    added
}

/// A map layer's id and source, as it is and without its keys.
type Source<'a> = (u32, &'a str, String);

/// A tile layer's index, address, and address without its keys.
type Tile<'a> = (usize, &'a str, String);

/// The command that removes the listed layer `l` from the map, when it is there: matched by its
/// address as it is first, then without its keys (as a saved project keeps it), so a copy of the
/// same service added under another key is only taken when nothing matches exactly.
fn removal(sources: &[Source<'_>], tiles: &[Tile<'_>], l: &Value) -> Option<Removal> {
    let offer = l.get("offer")?;
    let url = offer.get("url").and_then(Value::as_str).filter(|u| !u.is_empty())?;
    let keyless = crate::sources::redact(url);
    let remove_layer = |matches: &dyn Fn(&str, &str) -> bool| {
        let exact = sources.iter().find(|(_, s, _)| matches(url, s));
        exact
            .or_else(|| sources.iter().find(|(_, _, redacted)| matches(&keyless, redacted)))
            .map(|(id, ..)| ("layer.remove", json!({"layer": id})))
    };
    match offer.get("type").and_then(Value::as_str) {
        // Downloaded features keep the address they came from, with only the area query the app
        // adds to it.
        Some("features") => remove_layer(&from_area),
        // A coverage keeps its GetCoverage request: the service's address, which its other
        // coverages share, with this coverage's id.
        Some("coverage") => {
            let id = offer.get("describe").and_then(Value::as_str).and_then(coverage_id)?;
            remove_layer(&|url, s| from_address(url, s) && coverage_id(s) == Some(id))
        }
        // By position: a service's tile layers have no catalog id to tell them apart. Matched by
        // the listed template here; an OGC API layer's advertised one, by `claim_tilesets`.
        Some("tiles" | "vectorTiles" | "style") => tile_match(tiles, l).map(|i| ("map.removeTiles", json!({"index": i}))),
        // A folder or service (its layers are added from their rows) has nothing to remove.
        _ => None,
    }
}

/// The tile layer a listed layer's own template matches, if any: by its address as it is, then
/// without its keys (a saved project's, or a copy of the same service under another key).
fn tile_match(tiles: &[Tile<'_>], l: &Value) -> Option<usize> {
    let offer = l.get("offer")?;
    if !matches!(offer.get("type").and_then(Value::as_str), Some("tiles" | "vectorTiles" | "style")) {
        return None;
    }
    let url = offer.get("url").and_then(Value::as_str).filter(|u| !u.is_empty())?;
    let keyless = crate::sources::redact(url);
    tiles
        .iter()
        .find(|(_, address, _)| template_matches(url, address))
        .or_else(|| tiles.iter().find(|(_, _, keyless_address)| template_matches(&keyless, keyless_address)))
        .map(|(i, ..)| *i)
}

/// Gives the OGC API rows asked about (`rows`) that `added` doesn't match yet the tile layer their
/// tileset list advertised. Its template isn't in the listing, so it is found under that list: a
/// tile layer under one listed layer's list alone (counting every layer listed, shown or not),
/// starting with the listed layer's own template's start (its tile matrix set), and that no row
/// matched by its own template. Two rows of one collection may share a list: their tile matrix
/// sets tell their tiles apart. A tile layer under no listed layer's list, or another tileset's
/// under the same list (a layer added by hand), is nobody's, so Remove never removes another layer.
fn claim_tilesets(listed: &[Value], rows: &[usize], tiles: &[Tile<'_>], added: &mut [Option<Removal>]) {
    // Every listed row's own match, shown or not: a tile a row's own template matched is that
    // row's, so an asked row's advertised template never takes it.
    let mut claimed: Vec<u64> = listed.iter().filter_map(|l| tile_match(tiles, l)).map(|i| i as u64).collect();
    claimed.sort_unstable();
    claimed.dedup();
    // Each listed layer's tileset list and the start of its own template (its tile matrix set),
    // as listed and without their keys: a saved project keeps its addresses without them.
    let starts: Vec<Option<Tileset<'_>>> = listed
        .iter()
        .map(|l| {
            let list = tileset_list(l)?;
            let start = l.get("offer")?.get("url")?.as_str().map(tile_start)?;
            (!start.is_empty()).then(|| Tileset {
                list,
                start,
                list_keyless: crate::sources::redact(list),
                start_keyless: crate::sources::redact(start),
            })
        })
        .collect();
    // How many listed layers' tile matrix sets each tile layer is under, once: a row counts only
    // for tiles that start with its own template's start, so the tile matrix sets of one
    // collection sharing a tileset list are told apart (and the same start twice counts once).
    let under: Vec<usize> = tiles
        .iter()
        .map(|(_, url, keyless)| {
            let mut starts: Vec<&str> = starts
                .iter()
                .flatten()
                .filter(|t| under_list(t.list, url) || under_list(&t.list_keyless, keyless))
                .filter(|t| url.starts_with(t.start) || keyless.starts_with(t.start_keyless.as_str()))
                .map(|t| t.start_keyless.as_str())
                .collect();
            starts.sort_unstable();
            starts.dedup();
            starts.len()
        })
        .collect();
    for &i in rows {
        if added.get(i).is_none_or(Option::is_some) {
            continue;
        }
        let Some(tileset) = starts.get(i).and_then(Option::as_ref) else { continue };
        let found = tiles
            .iter()
            .zip(&under)
            .zip(0u64..)
            .find(|((tile, under), n)| {
                **under == 1
                    && !claimed.contains(n)
                    && (under_list(tileset.list, tile.1) || under_list(&tileset.list_keyless, &tile.2))
                    && (tile.1.starts_with(tileset.start) || tile.2.starts_with(tileset.start_keyless.as_str()))
            })
            .map(|(_, n)| n);
        if let (Some(n), Some(slot)) = (found, added.get_mut(i)) {
            *slot = Some(("map.removeTiles", json!({"index": n})));
            claimed.push(n);
        }
    }
}

/// A listed OGC API layer's tileset list and template start, as listed and without their keys.
struct Tileset<'a> {
    list: &'a str,
    start: &'a str,
    list_keyless: String,
    start_keyless: String,
}

/// The address a tile template's tiles share: the template up to its first placeholder (`{z}`,
/// `{Time}`), which names the tile matrix set.
fn tile_start(template: &str) -> &str {
    template.split_once('{').map_or(template, |(start, _)| start)
}

/// An OGC API layer's tileset list address, without its query.
fn tileset_list(l: &Value) -> Option<&str> {
    l.get("tilesets").and_then(Value::as_str).and_then(|t| t.split(['?', '#']).next()).filter(|t| !t.is_empty())
}

/// Whether the tile template `url` is under the tileset list address `list` (its tilesets' paths):
/// a trailing slash on the list names the same list.
fn under_list(list: &str, url: &str) -> bool {
    let list = list.trim_end_matches('/');
    !list.is_empty() && url.strip_prefix(list).is_some_and(|rest| rest.starts_with('/'))
}

/// Whether `source` is a coverage's endpoint `url`, or `url` with a query or fragment after it
/// (its GetCoverage request): never another address that only starts with the same letters
/// (`…/wcs2` for `…/wcs`), nor one further down its path.
fn from_address(url: &str, source: &str) -> bool {
    source.strip_prefix(url).is_some_and(|rest| rest.is_empty() || url.ends_with(['?', '&']) || rest.starts_with(['?', '&', '#']))
}

/// Whether `source` is the listed feature address `url` with only the area query the app appends
/// (`bbox`, a WFS's `BBOX`, or ArcGIS's geometry query): never a filter, another query of its own
/// or a fragment, so a filtered layer of the same service is left alone.
fn from_area(url: &str, source: &str) -> bool {
    let Some(rest) = source.strip_prefix(url) else { return false };
    if rest.is_empty() {
        return true;
    }
    let query = match url.ends_with(['?', '&']) {
        true => rest,
        false => match rest.strip_prefix('?').or_else(|| rest.strip_prefix('&')) {
            Some(q) => q,
            None => return false,
        },
    };
    !query.is_empty()
        && !query.contains('#')
        && query.split('&').all(|kv| {
            let key = kv.split_once('=').map_or(kv, |(k, _)| k);
            matches!(key, "bbox" | "BBOX" | "geometry" | "geometryType" | "inSR" | "spatialRel")
        })
}

/// A WCS request's coverage (`COVERAGEID`, or a WCS 1.0's `COVERAGE`), as its address gives it.
fn coverage_id(url: &str) -> Option<&str> {
    let query = url.split('#').next()?.split_once('?')?.1;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k.eq_ignore_ascii_case("coverageid") || k.eq_ignore_ascii_case("coverage")).then_some(v).filter(|v| !v.is_empty())
    })
}

/// Whether `url` is `template` with its `{Time}` placeholders (a time-enabled layer's) filled in.
fn template_matches(template: &str, url: &str) -> bool {
    let mut parts = template.split("{Time}");
    let Some(first) = parts.next() else { return false };
    let Some(mut rest) = url.strip_prefix(first) else { return false };
    // Each placeholder takes a date: one piece of the address, never a path or query.
    let date = |time: &str| !time.is_empty() && !time.contains(['/', '?', '#', '&', '=']);
    let mut parts = parts.peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return rest.strip_suffix(part).is_some_and(date);
        }
        let Some(at) = rest.find(part).filter(|&at| part.is_empty() || rest.get(..at).is_some_and(date)) else { return false };
        rest = rest.get(at + part.len()..).unwrap_or("");
    }
    rest.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_map::{TileKind, TileLayer};

    fn tiles(urls: &[&str]) -> Map {
        let mut map = Map::new("Map");
        map.tiles = urls.iter().map(|u| TileLayer::new("T", TileKind::Raster, *u)).collect();
        map
    }

    #[test]
    fn a_template_matches_only_its_own_address() {
        let wms = "http://h/wms?layers=a&bbox={bbox-epsg-3857}";
        assert!(template_matches(wms, wms));
        assert!(!template_matches(wms, "http://h/wms?layers=b&bbox={bbox-epsg-3857}"));
        assert!(!template_matches("", "http://h/x"));
        let timed = "http://h/{Time}/{z}/{x}/{y}.png";
        assert!(template_matches(timed, "http://h/2024-06-01/{z}/{x}/{y}.png"));
        assert!(!template_matches(timed, "http://other/2024-06-01/{z}/{x}/{y}.png"));
        assert!(!template_matches(timed, "http://h/2024-06-01/{z}/{x}/{y}.jpg"));
        // A trailing placeholder: a date, not every address under the prefix.
        assert!(template_matches("http://h/tiles?time={Time}", "http://h/tiles?time=2024-06-01"));
        // A placeholder takes a date, not the rest of a query (`&LAYERS=other` after it).
        assert!(template_matches("http://h/tiles?TIME={Time}", "http://h/tiles?TIME=2024-06-01"));
        assert!(!template_matches("http://h/tiles?TIME={Time}", "http://h/tiles?TIME=2024&LAYERS=other"));
        assert!(!template_matches("http://h/{Time}", "http://h/other/{z}/{x}/{y}.png"));
        assert!(!template_matches("http://h/{Time}", "http://h/"));
        // A middle one: one piece of the path, not several.
        assert!(!template_matches(timed, "http://h/a/b/{z}/{x}/{y}.png"));
        assert!(!template_matches("http://h/{Time}/x/{Time}.png", "http://h/a/b/x/c.png"));
        assert!(template_matches("http://h/{Time}/x/{Time}.png", "http://h/2024/x/2025.png"));
    }

    /// A coverage endpoint matches its GetCoverage request after it, never another address that
    /// only shares the prefix or is further down the path.
    #[test]
    fn an_endpoint_matches_only_its_request() {
        let endpoint = "https://h/wcs";
        assert!(from_address(endpoint, endpoint));
        assert!(from_address(endpoint, "https://h/wcs?SERVICE=WCS&REQUEST=GetCoverage"));
        assert!(!from_address(endpoint, "https://h/wcs/2"), "a path under the endpoint");
        assert!(!from_address(endpoint, "https://h/wcs2"), "another address sharing the prefix");
        assert!(!from_address(endpoint, "https://h/wc"));
        assert!(!from_address(endpoint, "wfs.geojson"));
        // An address ending in its query's start: what follows is that query.
        assert!(from_address("https://h/wcs?", "https://h/wcs?SERVICE=WCS"));
        // One ending in `/` is a path, not a prefix of every address under it.
        assert!(!from_address("https://h/", "https://h/other"));
        assert!(from_address("https://h/", "https://h/?bbox=1,2,3,4"));
    }

    /// A feature layer's address matches only the area query the app appends, never a filter or
    /// fragment of its own.
    #[test]
    fn a_feature_address_matches_only_its_area_query() {
        let items = "https://h/items";
        assert!(from_area(items, items));
        assert!(from_area(items, "https://h/items?bbox=1,2,3,4"));
        assert!(from_area("https://h/items?f=json", "https://h/items?f=json&bbox=1,2,3,4"));
        assert!(!from_area(items, "https://h/items?f=json&filter=private"), "a filtered layer is another layer");
        assert!(!from_area(items, "https://h/items?bbox=1,2,3,4&where=x"));
        assert!(!from_area(items, "https://h/items#top"), "a fragment isn't an area");
        assert!(!from_area(items, "https://h/items/2"), "one feature further down the path");
        assert!(!from_area(items, "https://h/items2"), "another service's address sharing the prefix");
        // A WFS GetFeature address ending in its query's start, and ArcGIS's geometry query.
        assert!(from_area("https://h/wfs?", "https://h/wfs?BBOX=1,2,3,4"));
        assert!(from_area(
            "https://h/FeatureServer/0/query",
            "https://h/FeatureServer/0/query?geometry=1,2,3,4&geometryType=esriGeometryEnvelope&inSR=4326&spatialRel=esriSpatialRelIntersects"
        ));
        assert!(!from_area("https://h/FeatureServer/0/query", "https://h/FeatureServer/0/query?where=x"));
    }

    #[test]
    fn a_coverage_is_told_by_its_id() {
        let v2 = "https://h/wcs?SERVICE=WCS&VERSION=2.0.1&REQUEST=GetCoverage&COVERAGEID=dem&FORMAT=image%2Ftiff";
        assert_eq!(coverage_id(v2), Some("dem"));
        assert_eq!(coverage_id("https://h/wcs?SERVICE=WCS&VERSION=1.0.0&REQUEST=DescribeCoverage&COVERAGE=soc"), Some("soc"));
        assert_eq!(coverage_id("https://h/wcs?SERVICE=WCS&COVERAGEID="), None);
        assert_eq!(coverage_id("https://h/wcs"), None);
    }

    /// A saved project keeps a tile template without its key: the row still finds it.
    #[test]
    fn a_keyed_tile_layer_is_known_after_a_reopen() {
        let offer = "https://h/wms?layers=a&api_key=K&bbox={bbox-epsg-3857}";
        let saved = crate::sources::redact(offer);
        assert!(!saved.contains("api_key"), "{saved}");
        let map = tiles(&["https://elsewhere/{z}/{x}/{y}.png", &saved]);
        let listed = [json!({"title": "A", "offer": {"type": "tiles", "url": offer}})];
        assert_eq!(added(&map, &listed, &[0]), [Some(("map.removeTiles", json!({"index": 1})))]);
    }

    /// A tile layer added under another key of the same service is found without its key, as a
    /// feature layer is: the row doesn't offer Add and make a duplicate.
    #[test]
    fn a_tile_under_another_key_is_known() {
        let offer = "https://h/wms?layers=a&api_key=A&bbox={bbox-epsg-3857}";
        let in_use = "https://h/wms?layers=a&api_key=B&bbox={bbox-epsg-3857}";
        let map = tiles(&[in_use]);
        let listed = [json!({"title": "A", "offer": {"type": "tiles", "url": offer}})];
        assert_eq!(added(&map, &listed, &[0]), [Some(("map.removeTiles", json!({"index": 0})))]);
    }

    /// Two copies of one service under different keys: each row takes its own copy first.
    #[test]
    fn the_exact_address_wins_over_the_keyless_one() {
        let mut map = Map::new("Map");
        for (id, key) in [(7, "A"), (8, "B")] {
            let mut ds = rustgis_data::Dataset::new("Items", rustgis_data::GeometryType::Point, Vec::new());
            ds.source = Some(format!("https://h/items?f=json&api_key={key}"));
            map.layers.push(rustgis_map::Layer::new(id, ds));
        }
        let row = |key: &str| json!({"offer": {"type": "features", "url": format!("https://h/items?f=json&api_key={key}")}});
        let listed = [row("B"), row("A"), row("C")];
        let found = added(&map, &listed, &[0, 1, 2]);
        assert_eq!(found[0], Some(("layer.remove", json!({"layer": 8}))));
        assert_eq!(found[1], Some(("layer.remove", json!({"layer": 7}))));
        // A key no copy has: the first copy without its key, as after a reopen.
        assert_eq!(found[2], Some(("layer.remove", json!({"layer": 7}))));
    }

    /// A layer with the listed title but another source is not the service's: its row keeps Add.
    #[test]
    fn a_layer_with_the_same_title_but_another_source_is_left_alone() {
        let mut map = Map::new("Map");
        // A name that reads as a dated copy of the title (`Roads 2 Lanes`) is another layer too.
        let mut ds = rustgis_data::Dataset::new("Roads 2 Lanes", rustgis_data::GeometryType::Point, Vec::new());
        ds.source = Some("C:/data/roads.shp".into());
        map.layers.push(rustgis_map::Layer::new(7, ds));
        let listed = [json!({"title": "Roads", "offer": {"type": "features", "url": "https://h/items"}})];
        assert_eq!(added(&map, &listed, &[0]), [None]);
        let map = tiles(&["https://elsewhere/{z}/{x}/{y}.png"]);
        let listed = [json!({"title": "T", "offer": {"type": "tiles", "url": "https://h/{z}/{x}/{y}.png"}})];
        assert_eq!(added(&map, &listed, &[0]), [None]);
    }

    /// A coverage is known by its own id, not just its service's address.
    #[test]
    fn a_coverage_of_another_id_is_left_alone() {
        let mut map = Map::new("Map");
        let mut ds = rustgis_data::Dataset::new("DEM", rustgis_data::GeometryType::None, Vec::new());
        ds.source = Some("https://h/wcs?SERVICE=WCS&REQUEST=GetCoverage&COVERAGEID=soc".into());
        map.layers.push(rustgis_map::Layer::new(7, ds));
        let row = |id: &str| json!({"title": "DEM", "offer": {"type": "coverage", "url": "https://h/wcs", "describe": format!("https://h/wcs?SERVICE=WCS&REQUEST=DescribeCoverage&COVERAGEID={id}")}});
        assert_eq!(added(&map, &[row("dem")], &[0]), [None]);
        assert_eq!(added(&map, &[row("soc")], &[0]), [Some(("layer.remove", json!({"layer": 7})))]);
    }

    /// An OGC API row's advertised tiles: its own row claims them, and one tile layer is claimed
    /// by one row only, counting the rows a filter hides too.
    #[test]
    fn a_tileset_is_claimed_by_one_row_of_a_collection() {
        let map = tiles(&["https://h/tiles/WebMercatorQuad/{z}/{y}/{x}.png", "https://h/c/tiles/WebMercatorQuad/{z}/{y}/{x}.png"]);
        let row = |start: &str, list: &str| json!({"offer": {"type": "tiles", "url": format!("{start}/WebMercatorQuad/{{z}}/{{y}}/{{x}}")}, "tilesets": list});
        let listed = [
            row("https://h/tiles", "https://h/tiles"),
            row("https://h/tiles", "https://h/tiles"),
            row("https://h/c/tiles", "https://h/c/tiles?f=json"),
        ];
        let remove = |i: u64| Some(("map.removeTiles", json!({"index": i})));
        // Rows a and b are one collection: a claims its tiles, and b doesn't take them too. It
        // stays a's when only b is shown: the rows a filter hides count.
        assert_eq!(added(&map, &listed, &[0]), [remove(0), None, None]);
        assert_eq!(added(&map, &listed, &[1]), [None, remove(0), None]);
        assert_eq!(added(&map, &listed, &[0, 1, 2]), [remove(0), None, remove(1)]);
    }

    /// Two tile matrix sets of one collection may share a tileset list: each row claims its own
    /// tiles, and a hand-added tile of another tile set under that list is left alone.
    #[test]
    fn tile_matrix_sets_sharing_a_list_are_told_apart() {
        let map = tiles(&["https://h/c/tiles/WorldCRS84Quad/{z}/{x}/{y}.png", "https://h/c/tiles/WebMercatorQuad/{z}/{y}/{x}.png"]);
        let row = |start: &str| json!({"offer": {"type": "tiles", "url": format!("{start}/{{z}}/{{y}}/{{x}}")}, "tilesets": "https://h/c/tiles?f=json"});
        let listed = [row("https://h/c/tiles/WebMercatorQuad"), row("https://h/c/tiles/WorldCRS84Quad")];
        // Each row takes the tile under its own template's start.
        assert_eq!(
            added(&map, &listed, &[0, 1]),
            [Some(("map.removeTiles", json!({"index": 1}))), Some(("map.removeTiles", json!({"index": 0})))]
        );
        // The WebMercatorQuad row alone doesn't take the WorldCRS84Quad tile.
        assert_eq!(added(&map, &listed, &[0]), [Some(("map.removeTiles", json!({"index": 1}))), None]);
    }

    /// A tileset list address written with a trailing slash names the same list.
    #[test]
    fn a_tileset_list_with_a_trailing_slash_still_claims_its_tile() {
        let map = tiles(&["https://h/c/tiles/WebMercatorQuad/{z}/{y}/{x}.png"]);
        let row = json!({
            "offer": {"type": "tiles", "url": "https://h/c/tiles/WebMercatorQuad/{z}/{y}/{x}"},
            "tilesets": "https://h/c/tiles/?f=json",
        });
        assert_eq!(added(&map, &[row], &[0]), [Some(("map.removeTiles", json!({"index": 0})))]);
    }

    /// A tile a listed row's own template matches is that row's even when the filter hides it: an
    /// OGC API row doesn't take it with its advertised template.
    #[test]
    fn a_tile_a_hidden_row_matches_is_not_claimed() {
        let tile = "https://h/c/tiles/WebMercatorQuad/{z}/{y}/{x}.png";
        let map = tiles(&[tile]);
        let listed = [
            json!({"offer": {"type": "tiles", "url": tile}}),
            json!({
                "offer": {"type": "tiles", "url": "https://h/c/tiles/WebMercatorQuad/{z}/{y}/{x}"},
                "tilesets": "https://h/c/tiles?f=json",
            }),
        ];
        // The OGC API row is shown, the row whose template is the tile is hidden.
        assert_eq!(added(&map, &listed, &[1]), [None, None]);
    }

    /// A tile layer under the listed layer's tileset list but in another tile matrix set (a layer
    /// added by hand from the same collection) is left alone.
    #[test]
    fn a_hand_added_tile_in_another_tileset_is_left_alone() {
        let map = tiles(&["https://h/c/tiles/WorldCRS84Quad/{z}/{x}/{y}.png"]);
        let row = json!({
            "offer": {"type": "tiles", "url": "https://h/c/tiles/WebMercatorQuad/{z}/{y}/{x}"},
            "tilesets": "https://h/c/tiles?f=json",
        });
        assert_eq!(added(&map, &[row], &[0]), [None]);
    }

    /// A project saved and reopened keeps the tileset's addresses without their keys: the OGC API
    /// row still claims its advertised tile.
    #[test]
    fn a_saved_tileset_still_claims_its_advertised_tile() {
        let list = "https://user:pass@h/c/tiles?token=K&f=json";
        let offer = "https://user:pass@h/c/tiles/WebMercatorQuad/{z}/{y}/{x}";
        let saved_tile = "https://h/c/tiles/WebMercatorQuad/{z}/{y}/{x}.png";
        let map = tiles(&[saved_tile]);
        let listed = [json!({"offer": {"type": "tiles", "url": offer}, "tilesets": list})];
        assert_eq!(added(&map, &listed, &[0]), [Some(("map.removeTiles", json!({"index": 0})))]);
    }

    /// A coverage added as a real raster keeps its GetCoverage request, so the matcher knows it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_real_rasters_coverage_is_known_by_its_request() {
        use crate::{Project, Session};
        let mut s = Session::new(Project::default());
        let bytes = crate::tests::multiband_tiff(1, 1, &[vec![0.0]], (0.0, 0.0), 1.0, None);
        let r = crate::cmd::raster::add_named_bytes(&mut s, &json!({"name": "Soil carbon"}), "soil.tif".into(), bytes).unwrap();
        let id = r["layer"].as_u64().unwrap() as u32;
        let get = "https://h/wcs?SERVICE=WCS&VERSION=2.0.1&REQUEST=GetCoverage&COVERAGEID=soc&FORMAT=image%2Ftiff";
        crate::cmd::service::mark_service_source(&mut s, &r, get);
        let listed = [json!({
            "title": "Soil carbon",
            "offer": {
                "type": "coverage",
                "url": "https://h/wcs",
                "describe": "https://h/wcs?SERVICE=WCS&VERSION=2.0.1&REQUEST=DescribeCoverage&COVERAGEID=soc",
                "version": "2.0.1",
            },
        })];
        assert_eq!(added(s.map(), &listed, &[0]), [Some(("layer.remove", json!({"layer": id})))]);
    }
}
