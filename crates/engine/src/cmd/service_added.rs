//! Which layers of a service's listing (`service.layers`) are on the map already, and the command
//! that removes each (Add From Service's Remove). A listed layer is known by the address it came
//! from (`service::mark_service_source`, a tile layer's template), never by its name alone, so a
//! file's layer or another service's that shares the title is left alone.

use rustgis_map::Map;
use serde_json::{Value, json};

/// A command and its parameters (`layer.remove {"layer"}`, `map.removeTiles {"index"}`).
pub type Removal = (&'static str, Value);

/// For each layer of `listed` (a `service.layers` listing's `layers`) whose place is in `rows`,
/// the command that removes it from `map` when it is there; `None` for the others, and for every
/// row not asked about. Each map layer is given to one row at most.
pub fn added(map: &Map, listed: &[Value], rows: &[usize]) -> Vec<Option<Removal>> {
    // Each layer's source as it is and without its keys, once: a project saved and opened again
    // keeps its addresses without them (the listing's still carry them).
    let sources: Vec<Source<'_>> =
        map.layers.iter().filter_map(|l| l.dataset.source.as_deref().map(|s| (l.id, s, crate::sources::redact(s)))).collect();
    let mut added = vec![None; listed.len()];
    for &i in rows {
        if let (Some(l), Some(slot)) = (listed.get(i), added.get_mut(i)) {
            *slot = removal(map, &sources, l);
        }
    }
    claim_tilesets(map, listed, rows, &mut added);
    added
}

/// A map layer's id and source, as it is and without its keys.
type Source<'a> = (u32, &'a str, String);

/// The command that removes the listed layer `l` from `map`, when it is there: matched by its
/// address as it is first, then without its keys (as a saved project keeps it), so a copy of the
/// same service added under another key is only taken when nothing matches exactly.
fn removal(map: &Map, sources: &[Source<'_>], l: &Value) -> Option<Removal> {
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
        // Downloaded features keep the address they came from (an area only adds a query to it).
        Some("features") => remove_layer(&from_address),
        // A coverage keeps its GetCoverage request: the service's address, which its other
        // coverages share, with this coverage's id.
        Some("coverage") => {
            let id = offer.get("describe").and_then(Value::as_str).and_then(coverage_id)?;
            remove_layer(&|url, s| from_address(url, s) && coverage_id(s) == Some(id))
        }
        // By position: a service's tile layers have no catalog id to tell them apart. Matched by
        // the listed template here (a saved project keeps it without its keys); an OGC API
        // layer's advertised one, by `claim_tilesets`.
        _ => map
            .tiles
            .iter()
            .position(|t| template_matches(url, &t.url))
            .or_else(|| map.tiles.iter().position(|t| template_matches(&keyless, &t.url)))
            .map(|i| ("map.removeTiles", json!({"index": i}))),
    }
}

/// Gives the OGC API rows asked about (`rows`) that `added` doesn't match yet the tile layer their
/// tileset list advertised. Its template isn't in the listing, so it is found under that list: a
/// tile layer under one listed layer's list alone (counting every layer listed, shown or not),
/// that no row matched by its own template, is that row's. One under several lists is nobody's,
/// so Remove never takes another row's tiles.
fn claim_tilesets(map: &Map, listed: &[Value], rows: &[usize], added: &mut [Option<Removal>]) {
    let mut claimed: Vec<u64> = added
        .iter()
        .flatten()
        .filter(|(command, _)| *command == "map.removeTiles")
        .filter_map(|(_, p)| p.get("index").and_then(Value::as_u64))
        .collect();
    let lists: Vec<&str> = listed.iter().filter_map(tileset_list).collect();
    // How many listed layers' lists each tile layer is under, once.
    let under: Vec<usize> = map.tiles.iter().map(|t| lists.iter().filter(|list| under_list(list, &t.url)).count()).collect();
    for &i in rows {
        if added.get(i).is_none_or(Option::is_some) {
            continue;
        }
        let Some(list) = listed.get(i).and_then(tileset_list) else { continue };
        let found = map
            .tiles
            .iter()
            .zip(&under)
            .zip(0u64..)
            .find(|((t, under), n)| **under == 1 && !claimed.contains(n) && under_list(list, &t.url))
            .map(|(_, n)| n);
        if let (Some(n), Some(slot)) = (found, added.get_mut(i)) {
            *slot = Some(("map.removeTiles", json!({"index": n})));
            claimed.push(n);
        }
    }
}

/// An OGC API layer's tileset list address, without its query.
fn tileset_list(l: &Value) -> Option<&str> {
    l.get("tilesets").and_then(Value::as_str).and_then(|t| t.split(['?', '#']).next()).filter(|t| !t.is_empty())
}

/// Whether the tile template `url` is under the tileset list address `list` (its tilesets' paths).
fn under_list(list: &str, url: &str) -> bool {
    url.strip_prefix(list).is_some_and(|rest| rest.starts_with('/'))
}

/// Whether `source` is the address `url`, or `url` with a query or fragment after it (an area's
/// bounding box): never another address that only starts with the same letters (`…/items2` for
/// `…/items`), nor one further down its path (`…/items/2`, one feature added by hand).
fn from_address(url: &str, source: &str) -> bool {
    source.strip_prefix(url).is_some_and(|rest| rest.is_empty() || url.ends_with(['?', '&']) || rest.starts_with(['?', '&', '#']))
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
    let date = |time: &str| !time.is_empty() && !time.contains(['/', '?', '#']);
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
        assert!(!template_matches("http://h/{Time}", "http://h/other/{z}/{x}/{y}.png"));
        assert!(!template_matches("http://h/{Time}", "http://h/"));
        // A middle one: one piece of the path, not several.
        assert!(!template_matches(timed, "http://h/a/b/{z}/{x}/{y}.png"));
        assert!(!template_matches("http://h/{Time}/x/{Time}.png", "http://h/a/b/x/c.png"));
        assert!(template_matches("http://h/{Time}/x/{Time}.png", "http://h/2024/x/2025.png"));
    }

    #[test]
    fn an_address_matches_only_itself_and_what_follows_it() {
        let items = "https://h/items";
        assert!(from_address(items, items));
        assert!(from_address(items, "https://h/items?bbox=1,2,3,4"));
        assert!(!from_address(items, "https://h/items/2"), "one feature further down the path");
        assert!(from_address(items, "https://h/items#top"));
        assert!(!from_address(items, "https://h/items2"), "another service's address sharing the prefix");
        assert!(!from_address(items, "https://h/item"));
        assert!(!from_address(items, "wfs.geojson"));
        // An address ending in its query's start: what follows is that query.
        assert!(from_address("https://h/wcs?", "https://h/wcs?SERVICE=WCS"));
        // One ending in `/` is a path, not a prefix of every address under it.
        assert!(!from_address("https://h/", "https://h/other"));
        assert!(from_address("https://h/", "https://h/?bbox=1,2,3,4"));
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

    /// An OGC API row's advertised tiles: claimed under its tileset list alone, counting the rows
    /// a filter hides too, so a row never takes another's tiles.
    #[test]
    fn a_tileset_is_claimed_only_under_one_list() {
        let map = tiles(&["https://h/tiles/B/{z}/{x}/{y}.png", "https://h/c/tiles/WebMercatorQuad/{z}/{y}/{x}"]);
        let row = |id: &str, list: &str| json!({"offer": {"type": "tiles", "url": format!("https://h/offer/{id}")}, "tilesets": list});
        let listed = [row("a", "https://h/tiles"), row("b", "https://h/tiles"), row("c", "https://h/c/tiles?f=json")];
        // Rows a and b share a list: neither takes its tiles, even with b filtered out.
        assert_eq!(added(&map, &listed, &[0]), [None, None, None]);
        assert_eq!(added(&map, &listed, &[0, 1, 2]), [None, None, Some(("map.removeTiles", json!({"index": 1})))]);
    }
}
