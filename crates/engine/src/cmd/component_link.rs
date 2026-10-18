//! Linked designs: a component whose bodies come from another design file (Insert Design).
//!
//! The linked design is opened and evaluated on its own, and its solid bodies, as placed in it,
//! join a new component through an Import feature (exact B-rep, kept in this design so it opens
//! without the part file). The link remembers the file, relative to this design's folder once it
//! is saved; Update Links reads the file again and replaces the Import feature's bodies, and
//! Break Link keeps the bodies and forgets the file.

use std::path::{Component as PathPart, Path, PathBuf};

use serde_json::{Value, json};
use solidcraft_doc::{DesignLink, Document, FeatureKind};

use super::CommandSpec;
use crate::params::{bad, str_, vec3};
use crate::{EngineError, Result, Session};

pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec::new("component.insert_design", "Insert Design", insert_design).at("SOLID", "ASSEMBLE").icon("component").params(
        "path: a .solidcraft design → a new component linked to it, holding its solid bodies as placed in that design; \
         name? (default: the design's name); parent?: component id or name (default: the active one); translate?: [x,y,z]",
    ),
    CommandSpec::new("component.update_links", "Update Links", update_links).icon("update").params(
        "component?: id or name (default: every linked component) — reads the linked designs again and replaces their bodies where they changed",
    ),
    CommandSpec::new("component.break_link", "Break Link", break_link)
        .icon("break")
        .params("component: id or name of a linked component — keeps its bodies as they are and forgets the design file"),
    CommandSpec::new("component.links", "List Links", list_links)
        .icon("chain")
        .noundo()
        .params("→ linked components: component, name, path, file (resolved), found"),
];

/// The folder relative link paths are kept against: the design's own, once it has a file.
fn base_dir(path: Option<&str>) -> Option<PathBuf> {
    path.and_then(|p| absolute(Path::new(p)).parent().map(Path::to_path_buf))
}

/// A path as given, made absolute when the platform can, with `.` and `..` worked out.
fn absolute(p: &Path) -> PathBuf {
    let full = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut out = PathBuf::new();
    for c in full.components() {
        match c {
            PathPart::CurDir => {}
            PathPart::ParentDir if matches!(out.components().next_back(), Some(PathPart::Normal(_))) => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// `target` relative to the folder `base` (both absolute), when they share a root.
fn relative(base: &Path, target: &Path) -> Option<PathBuf> {
    let (b, t): (Vec<_>, Vec<_>) = (base.components().collect(), target.components().collect());
    let root = |c: &[PathPart]| c.iter().take_while(|x| matches!(x, PathPart::Prefix(_) | PathPart::RootDir)).count();
    if root(&b) == 0 || b.get(..root(&b)) != t.get(..root(&b)) {
        return None;
    }
    let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in t.iter().skip(common) {
        out.push(c.as_os_str());
    }
    Some(out)
}

/// How a link stores `file` for a design saved at `design` (`None`: not saved yet).
fn stored(design: Option<&str>, file: &Path) -> String {
    let file = absolute(file);
    match base_dir(design).and_then(|d| relative(&d, &file)) {
        // Relative paths are written with `/`, so a design moves between systems with its parts.
        Some(rel) => rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"),
        None => file.to_string_lossy().into_owned(),
    }
}

/// The file a stored link path names, for a design saved at `design`.
fn resolve(design: Option<&str>, link: &str) -> PathBuf {
    let p = Path::new(link);
    match base_dir(design) {
        Some(d) if p.is_relative() => d.join(p),
        _ => p.to_path_buf(),
    }
}

/// The links of `doc` stored for a design saved at `to` instead of at `from`.
pub(super) fn rebased(doc: &Document, from: Option<&str>, to: &str) -> std::collections::BTreeMap<u64, DesignLink> {
    doc.links.iter().map(|(c, l)| (*c, DesignLink { path: stored(Some(to), &resolve(from, &l.path)), feature: l.feature })).collect()
}

/// The solid bodies of the design at `file`, evaluated, as an Import feature.
fn linked_bodies(file: &Path, own: Option<&str>) -> Result<(String, solidcraft_io::StepFeature)> {
    let shown = file.display().to_string();
    if own.is_some_and(|o| absolute(Path::new(o)) == absolute(file)) {
        return Err(EngineError::Other(format!("{shown}: a design can't be linked into itself")));
    }
    let bytes = solidcraft_io::vfs::read(&shown, solidcraft_io::MAX_DESIGN_BYTES).map_err(|e| EngineError::Other(format!("{shown}: {e}")))?;
    let doc = solidcraft_io::read_design(&bytes).map_err(|e| EngineError::Other(format!("{shown}: {e}")))?;
    let name = doc.name.clone();
    let part = Session::new(doc);
    if let Some(e) = part.model.results.iter().find_map(|r| r.error.clone()) {
        return Err(EngineError::Other(format!("{shown}: the design has a feature that fails ({e}); fix it before linking it")));
    }
    let world = part.world_state();
    let solids: Vec<String> = world.bodies.iter().filter(|b| !b.body.is_mesh()).map(|b| b.name.clone()).collect();
    if solids.is_empty() {
        return Err(EngineError::Other(format!("{shown}: the design has no solid bodies to link")));
    }
    let step = solidcraft_io::export(&part.doc.painted(&world), &solids, solidcraft_io::Format::Step, &name)
        .map_err(|e| EngineError::Other(format!("{shown}: {e}")))?;
    let mut feature = solidcraft_io::step_import_feature(&step, &shown).map_err(|e| EngineError::Other(format!("{shown}: {e}")))?;
    if world.bodies.len() > solids.len() {
        feature.warnings.push(format!("{} mesh bodies of {shown} are not linked", world.bodies.len() - solids.len()));
    }
    Ok((name, feature))
}

fn component_key(s: &Session, p: &Value, key: &str, cmd: &str) -> Result<u64> {
    let k = match p.get(key) {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(x)) => x.clone(),
        _ => return Err(bad(cmd, format!("`{key}` must be a component id or name"))),
    };
    s.doc.find_component(&k).ok_or_else(|| bad(cmd, format!("no component `{k}`")))
}

fn insert_design(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "component.insert_design";
    let given = str_(p, "path").filter(|x| !x.trim().is_empty() && x.len() < 4096).ok_or_else(|| bad(cmd, "`path` is required"))?;
    let translate = match p.get("translate") {
        None => solidcraft_geom::Vec3::ZERO,
        Some(v) => vec3(v).filter(|t| t.is_finite() && t.len() < 1e9).ok_or_else(|| bad(cmd, "`translate` must be [x, y, z] in mm"))?,
    };
    let file = absolute(Path::new(given));
    let (design_name, sf) = linked_bodies(&file, s.path.as_deref())?;
    let name = str_(p, "name").filter(|n| !n.trim().is_empty()).map(str::to_string).unwrap_or(design_name);
    let mut create = json!({"name": name, "activate": false});
    if let Some(parent) = p.get("parent") {
        create["parent"] = parent.clone();
    }
    let made = s.execute("component.create", &create)?;
    let comp = made["component"].as_u64().ok_or_else(|| bad(cmd, "the component was not created"))?;
    let occ = made["occurrence"].as_u64();
    let id = s.doc_mut().add_feature(sf.kind, Some(&name))?;
    if let Some(f) = s.doc_mut().feature_mut(id) {
        f.component = comp;
        f.body_names = sf.body_names;
    }
    if let Some(o) = s.doc_mut().occurrences.iter_mut().find(|o| Some(o.id) == occ) {
        o.transform[3] = [translate.x, translate.y, translate.z, 1.0];
    }
    let path = stored(s.path.as_deref(), &file);
    s.doc_mut().links.insert(comp, DesignLink { path: path.clone(), feature: id });
    s.refresh();
    if let Some(e) = s.model.result(id).and_then(|r| r.error.clone()) {
        return Err(EngineError::Other(e));
    }
    let st = s.model.state();
    let bodies: Vec<&str> = st.bodies.iter().filter(|b| b.feature == id).map(|b| b.name.as_str()).collect();
    Ok(
        json!({"component": comp, "occurrence": occ, "name": name, "feature": id, "path": path, "bodies": bodies, "created_bodies": bodies, "warnings": sf.warnings}),
    )
}

fn update_links(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "component.update_links";
    let only = if p.get("component").is_some() { Some(component_key(s, p, "component", cmd)?) } else { None };
    if let Some(c) = only
        && !s.doc.links.contains_key(&c)
    {
        return Err(bad(cmd, "that component is not linked to a design"));
    }
    let links: Vec<(u64, DesignLink)> = s.doc.links.iter().filter(|(c, _)| only.is_none_or(|o| o == **c)).map(|(c, l)| (*c, l.clone())).collect();
    let (mut updated, mut unchanged, mut warnings) = (Vec::new(), Vec::new(), Vec::new());
    for (comp, link) in links {
        let cname = s.doc.components.iter().find(|c| c.id == comp).map(|c| c.name.clone());
        let Some(cname) = cname else {
            // The component was deleted: the link goes with it.
            s.doc_mut().links.remove(&comp);
            continue;
        };
        let Some(FeatureKind::Import { step: old, .. }) = s.doc.feature(link.feature).filter(|f| f.component == comp).map(|f| &f.kind) else {
            return Err(bad(
                cmd,
                format!("{cname}: its linked bodies (feature {}) are gone; break the link or insert the design again", link.feature),
            ));
        };
        let old = old.clone();
        let file = resolve(s.path.as_deref(), &link.path);
        let (_, sf) = linked_bodies(&file, s.path.as_deref()).map_err(|e| bad(cmd, format!("{cname}: {e}")))?;
        warnings.extend(sf.warnings.iter().cloned());
        if matches!(&sf.kind, FeatureKind::Import { step, .. } if *step == old) {
            unchanged.push(cname);
            continue;
        }
        if let Some(f) = s.doc_mut().feature_mut(link.feature) {
            // Bodies keep their names where the design still has as many.
            if f.body_names.len() != sf.body_names.len() {
                f.body_names = sf.body_names;
            }
            f.kind = sf.kind;
        }
        updated.push(cname);
    }
    s.refresh();
    if let Some(e) = s.doc.links.values().find_map(|l| s.model.result(l.feature).and_then(|r| r.error.clone())) {
        return Err(EngineError::Other(e));
    }
    Ok(json!({"updated": updated, "unchanged": unchanged, "warnings": warnings}))
}

fn break_link(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "component.break_link";
    let comp = component_key(s, p, "component", cmd)?;
    let link = s.doc_mut().links.remove(&comp).ok_or_else(|| bad(cmd, "that component is not linked to a design"))?;
    Ok(json!({"component": comp, "path": link.path}))
}

fn list_links(s: &mut Session, _p: &Value) -> Result<Value> {
    let links: Vec<Value> = s
        .doc
        .links
        .iter()
        .map(|(c, l)| {
            let file = resolve(s.path.as_deref(), &l.path);
            let name = s.doc.components.iter().find(|x| x.id == *c).map(|x| x.name.clone());
            json!({"component": c, "name": name, "path": l.path, "file": file.display().to_string(), "found": file.is_file()})
        })
        .collect();
    Ok(json!({"links": links}))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn link_paths_are_kept_relative_to_the_design_and_follow_save_as() {
        let root = absolute(Path::new("projects"));
        let part = root.join("parts").join("plate.solidcraft");
        let design = root.join("assembly").join("case.solidcraft");
        let design = design.to_string_lossy().into_owned();
        assert_eq!(stored(Some(&design), &part), "../parts/plate.solidcraft");
        assert_eq!(absolute(&resolve(Some(&design), "../parts/plate.solidcraft")), part);
        // A design path given relative to the working folder works the same.
        assert_eq!(stored(Some("case.solidcraft"), &absolute(Path::new("parts/plate.solidcraft"))), "parts/plate.solidcraft");
        // Before the design has a file the path is kept whole; saving it makes it relative.
        let unsaved = stored(None, &part);
        assert!(Path::new(&unsaved).is_absolute(), "{unsaved}");
        let mut doc = Document::default();
        doc.links.insert(7, DesignLink { path: unsaved, feature: 3 });
        let moved = rebased(&doc, None, &design);
        assert_eq!(moved.get(&7).map(|l| l.path.as_str()), Some("../parts/plate.solidcraft"));
        let beside = root.join("parts").join("asm.solidcraft").to_string_lossy().into_owned();
        assert_eq!(
            rebased(&Document { links: moved, ..Document::default() }, Some(&design), &beside).get(&7).map(|l| l.path.as_str()),
            Some("plate.solidcraft")
        );
    }

    fn run(s: &mut Session, id: &str, p: Value) -> Value {
        s.execute(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
    }

    fn save_box(path: &Path, height: f64) {
        let mut part = Session::default();
        run(&mut part, "solid.box", json!({"length": 10, "width": 20, "height": height, "body_name": "Plate"}));
        run(&mut part, "file.save", json!({"path": path}));
    }

    fn volume(s: &mut Session) -> f64 {
        run(s, "inspect.measure", json!({}))["total"]["volume_mm3"].as_f64().unwrap_or(f64::NAN)
    }

    #[test]
    fn a_saved_assembly_reopens_with_its_link_updates_it_and_breaks_it() {
        let dir = std::env::temp_dir().join(format!("solidcraft-link-reopen-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("parts")).unwrap();
        std::fs::create_dir_all(dir.join("asm")).unwrap();
        let part = dir.join("parts").join("plate.solidcraft");
        let case = dir.join("asm").join("case.solidcraft");
        save_box(&part, 30.0);
        // Given an assembly holding the part, saved in a folder beside the part's.
        let mut a = Session::default();
        let inserted = run(&mut a, "component.insert_design", json!({"path": part}));
        assert_eq!(inserted["name"], "plate");
        run(&mut a, "file.save", json!({"path": case}));
        assert!((volume(&mut a) - 6000.0).abs() < 1e-6);
        // When it is opened again, the link is kept relative to it and finds the part.
        let mut b = Session::default();
        run(&mut b, "doc.open", json!({"path": case}));
        let links = run(&mut b, "component.links", json!({}));
        assert_eq!(links["links"][0]["path"], "../parts/plate.solidcraft", "{links}");
        assert_eq!(links["links"][0]["found"], true, "{links}");
        assert!((volume(&mut b) - 6000.0).abs() < 1e-6);
        // An unchanged part leaves the assembly as it is; an edited one comes in on update.
        assert_eq!(run(&mut b, "component.update_links", json!({}))["unchanged"], json!(["plate"]));
        save_box(&part, 45.0);
        assert_eq!(run(&mut b, "component.update_links", json!({}))["updated"], json!(["plate"]));
        assert!((volume(&mut b) - 9000.0).abs() < 1e-6, "{}", volume(&mut b));
        // A part file that has gone is reported, and the assembly keeps the bodies it had.
        std::fs::remove_file(&part).unwrap();
        let err = b.execute("component.update_links", &json!({})).unwrap_err().to_string();
        assert!(err.contains("plate"), "{err}");
        assert!((volume(&mut b) - 9000.0).abs() < 1e-6);
        // Break Link keeps the bodies and forgets the file.
        run(&mut b, "component.break_link", json!({"component": "plate"}));
        assert_eq!(run(&mut b, "component.links", json!({}))["links"], json!([]));
        assert!((volume(&mut b) - 9000.0).abs() < 1e-6);
        assert!(b.execute("component.update_links", &json!({"component": "plate"})).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inserting_a_missing_file_or_bad_parameters_changes_nothing() {
        let mut s = Session::default();
        let missing = std::env::temp_dir().join(format!("solidcraft-no-such-part-{}.solidcraft", std::process::id()));
        assert!(s.execute("component.insert_design", &json!({"path": missing})).is_err());
        assert!(s.execute("component.insert_design", &json!({})).is_err());
        assert!(s.execute("component.insert_design", &json!({"path": "x.solidcraft", "translate": [1, "a", 2]})).is_err());
        assert!(s.doc.components.is_empty() && s.doc.links.is_empty() && s.doc.features.is_empty());
    }

    // Portable native-link regressions. A revolved source design is inserted as a linked
    // component, saved, reopened in a fresh session and explicitly refreshed. Every linked
    // body goes out to STEP and back in (`linked_bodies` above), so a revolution face the
    // writer emits as a cone the importer refuses is lost here. Expected topology, validity
    // and measures come from the analytic construction, never from what the run happened to
    // produce. Owner files and the external proof manifest are never touched by these.

    /// Save a design whose only body is one closed profile revolved 360° about the sketch y
    /// axis. Points are (radius, axial) in the XZ sketch.
    fn save_revolved(path: &Path, body: &str, points: &[[f64; 2]]) {
        let mut part = Session::default();
        run(&mut part, "sketch.create", json!({"plane": "XZ", "name": "Profile"}));
        run(&mut part, "sketch.line", json!({"points": points, "closed": true}));
        run(&mut part, "sketch.finish", json!({}));
        run(&mut part, "solid.revolve", json!({"sketch": "Profile", "axis": "y", "body_name": body}));
        run(&mut part, "file.save", json!({"path": path}));
    }

    /// One linked body after insert, save, fresh reopen and explicit refresh, keyed by its own
    /// name rather than by position.
    #[derive(Debug)]
    struct LinkedBody {
        faces: usize,
        shells: usize,
        validity: Vec<String>,
        volume: f64,
        area: f64,
    }

    fn link_round_trip(tag: &str, points: &[[f64; 2]]) -> LinkedBody {
        let dir = std::env::temp_dir().join(format!("solidcraft-native-link-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("parts")).unwrap();
        std::fs::create_dir_all(dir.join("asm")).unwrap();
        let part = dir.join("parts").join("rev.solidcraft");
        let host = dir.join("asm").join("host.solidcraft");
        save_revolved(&part, "Rev", points);
        let mut a = Session::default();
        let inserted = run(&mut a, "component.insert_design", json!({"path": part}));
        let name = inserted["bodies"][0].as_str().unwrap().to_string();
        run(&mut a, "file.save", json!({"path": host}));
        let mut b = Session::default();
        run(&mut b, "doc.open", json!({"path": host}));
        let links = run(&mut b, "component.links", json!({}));
        assert_eq!(links["links"][0]["found"], true, "{links}");
        // The refresh re-reads the source and re-imports it through the same link path.
        let refreshed = run(&mut b, "component.update_links", json!({}));
        assert_eq!(refreshed["updated"], json!([]), "an unchanged source: {refreshed}");
        let st = b.model.state();
        let named: Vec<_> = st.bodies.iter().filter(|x| x.name == name).collect();
        assert_eq!(named.len(), 1, "`{name}`: bodies {:?}", st.bodies.iter().map(|x| x.name.as_str()).collect::<Vec<_>>());
        let body = &named[0].body;
        let facts = LinkedBody {
            faces: body.face_count(),
            shells: body.shell_count(),
            validity: body.validity(),
            volume: solidcraft_kernel::measure(body).unwrap().volume,
            area: solidcraft_kernel::measure(body).unwrap().area,
        };
        let _ = std::fs::remove_dir_all(&dir);
        facts
    }

    /// A quadrilateral revolved about its axis: an outer cylinder of radius `r_out`, annuli at
    /// both ends and an inner side from radius `r0` at z 0 to `r1` at z `h`. Returns the exact
    /// volume and area of that solid of revolution. These are exact; `Body` measures sum a
    /// tessellation, so callers compare at ~1e-3 relative (the mesher's resolution), while the
    /// exact checks are topology, validity and the analytic shape of the faces.
    fn shell_of_revolution(r0: f64, r1: f64, r_out: f64, h: f64) -> (f64, f64) {
        let pi = std::f64::consts::PI;
        let outer = r_out * r_out * h;
        let inner = (r0 * r0 + r0 * r1 + r1 * r1) * h / 3.0;
        let volume = pi * (outer - inner);
        let area =
            pi * (2.0 * r_out * h + (r0 + r1) * (h * h + (r1 - r0) * (r1 - r0)).sqrt() + (r_out * r_out - r0 * r0) + (r_out * r_out - r1 * r1));
        (volume, area)
    }

    #[test]
    fn native_link_revolved_near_axis_cone_is_kept() {
        // The inner side moves 2.5e-9 mm outward over 50 mm: a genuine cone of half-angle
        // atan(2.5e-9 / 50) = 5e-11 rad. That is inside the writer's emission band
        // (1e-12, pi/2) but below the importer's 1e-9 floor, so the linked body loses the face.
        let delta = 2.5e-9_f64;
        let b = link_round_trip("near-axis", &[[10.0, 0.0], [10.0 + delta, 50.0], [20.0, 50.0], [20.0, 0.0]]);
        assert!(b.validity.is_empty(), "the linked near-axis body is not a sound solid: {b:?}");
        assert_eq!(b.shells, 1, "{b:?}");
        assert_eq!(b.faces, 4, "the near-cylindrical inner cone face was dropped: {b:?}");
        let (v, a) = shell_of_revolution(10.0, 10.0 + delta, 20.0, 50.0);
        assert!((b.volume - v).abs() <= 1e-3 * v, "volume {} vs {v}: {b:?}", b.volume);
        assert!((b.area - a).abs() <= 1e-3 * a, "area {} vs {a}: {b:?}", b.area);
    }

    #[test]
    fn native_link_revolved_accepted_band_cone_is_kept() {
        // Half-angle atan(7.5e-8 / 50) = 1.5e-9 rad: a real cone inside the importer's accept
        // band, so it survives the round trip (the control for the near-axis case).
        let delta = 50.0 * 1.5e-9;
        let b = link_round_trip("accepted", &[[10.0, 0.0], [10.0 + delta, 50.0], [20.0, 50.0], [20.0, 0.0]]);
        assert!(b.validity.is_empty(), "{b:?}");
        assert_eq!(b.faces, 4, "{b:?}");
        let (v, a) = shell_of_revolution(10.0, 10.0 + delta, 20.0, 50.0);
        assert!((b.volume - v).abs() <= 1e-3 * v, "volume {} vs {v}: {b:?}", b.volume);
        assert!((b.area - a).abs() <= 1e-3 * a, "area {} vs {a}: {b:?}", b.area);
    }

    #[test]
    fn native_link_revolved_genuine_trimmed_cone_is_kept() {
        // A countersink-like frustum: the inner side spans 5 mm of radius over h = 5/tan(0.3),
        // a genuine 0.3 rad trimmed cone between two annular faces.
        let h = 5.0 / 0.3_f64.tan();
        let b = link_round_trip("genuine", &[[10.0, 0.0], [15.0, h], [20.0, h], [20.0, 0.0]]);
        assert!(b.validity.is_empty(), "{b:?}");
        assert_eq!(b.faces, 4, "{b:?}");
        let (v, a) = shell_of_revolution(10.0, 15.0, 20.0, h);
        assert!((b.volume - v).abs() <= 1e-3 * v, "volume {} vs {v}: {b:?}", b.volume);
        assert!((b.area - a).abs() <= 1e-3 * a, "area {} vs {a}: {b:?}", b.area);
    }

    // Opt-in actual-part proof harness. It only ever runs when a manifest is named through
    // SOLIDCRAFT_NATIVE_LINK_PROOF_MANIFEST (the `--ignored` test at the end). Every owner
    // path is supplied by that manifest at runtime; no owner path is compiled in. Guard
    // refusals happen before the first write, and the harness only reads the disposable
    // copies the manifest points at.

    /// SHA-256 (FIPS 180-4), so candidate and copy identity can be checked with no dependency
    /// and no platform tool.
    fn sha256_hex(bytes: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
            0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
            0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
            0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
            0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
            0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
        let mut msg = bytes.to_vec();
        let bits = (bytes.len() as u64).wrapping_mul(8);
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bits.to_be_bytes());
        for chunk in msg.as_chunks::<64>().0 {
            let mut w = [0u32; 64];
            for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
                w[i] = u32::from_be_bytes(*word);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *slot = slot.wrapping_add(v);
            }
        }
        h.iter().map(|v| format!("{v:08x}")).collect()
    }

    /// A disposable source copy named in the manifest.
    struct ProofSource {
        name: String,
        path: PathBuf,
        sha256: String,
    }

    /// One part's expected linked body: its name and analytic topology/measure.
    struct ProofPart {
        name: String,
        expected_faces: usize,
        expected_volume_mm3: Option<f64>,
        tolerance: f64,
    }

    /// One exact native checkpoint a candidate CLI produced (full-lifecycle phase).
    struct ProofCheckpoint {
        path: PathBuf,
        parts: Vec<ProofPart>,
    }

    /// The runtime-supplied manifest that drives the opt-in harness.
    struct ProofManifest {
        scratch_root: PathBuf,
        output_root: PathBuf,
        candidate_cli: PathBuf,
        candidate_sha256: String,
        protected_paths: Vec<PathBuf>,
        sources: Vec<ProofSource>,
        parts: Vec<ProofPart>,
        checkpoints: Vec<ProofCheckpoint>,
        phase: String,
    }

    fn required_str(v: &Value, key: &str) -> std::result::Result<String, String> {
        v.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("manifest: `{key}` (non-empty string) is required"))
    }

    fn parse_part(v: &Value) -> std::result::Result<ProofPart, String> {
        let name = required_str(v, "name")?;
        let expected_faces = v
            .get("expected_faces")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("manifest: part `{name}`: `expected_faces` (integer) is required"))? as usize;
        Ok(ProofPart {
            name,
            expected_faces,
            expected_volume_mm3: v.get("expected_volume_mm3").and_then(Value::as_f64),
            tolerance: v.get("tolerance").and_then(Value::as_f64).unwrap_or(1e-6),
        })
    }

    impl ProofManifest {
        fn parse(value: &Value, manifest_path: &Path) -> std::result::Result<Self, String> {
            let scratch_root =
                manifest_path.parent().map(Path::to_path_buf).ok_or_else(|| format!("{}: the manifest has no folder", manifest_path.display()))?;
            let output_root = PathBuf::from(required_str(value, "output_root")?);
            let cli = value.get("candidate_cli").ok_or("manifest: `candidate_cli` (object) is required")?;
            let protected_paths = value
                .get("protected_paths")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(PathBuf::from).collect())
                .unwrap_or_default();
            let sources = value
                .get("sources")
                .and_then(Value::as_array)
                .ok_or("manifest: `sources` (array) is required")?
                .iter()
                .map(|s| {
                    Ok(ProofSource {
                        name: required_str(s, "name")?,
                        path: PathBuf::from(required_str(s, "path")?),
                        sha256: required_str(s, "sha256")?,
                    })
                })
                .collect::<std::result::Result<Vec<_>, String>>()?;
            let parts = value
                .get("parts")
                .and_then(Value::as_array)
                .ok_or("manifest: `parts` (array) is required")?
                .iter()
                .map(parse_part)
                .collect::<std::result::Result<Vec<_>, String>>()?;
            let checkpoints = value
                .get("checkpoints")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|c| {
                            let path = PathBuf::from(required_str(c, "path")?);
                            let parts = c
                                .get("parts")
                                .and_then(Value::as_array)
                                .ok_or_else(|| format!("manifest: checkpoint {}: `parts` (array) is required", path.display()))?
                                .iter()
                                .map(parse_part)
                                .collect::<std::result::Result<Vec<_>, String>>()?;
                            Ok(ProofCheckpoint { path, parts })
                        })
                        .collect::<std::result::Result<Vec<_>, String>>()
                })
                .transpose()?
                .unwrap_or_default();
            let phase = required_str(value, "phase")?;
            if phase != "insert-reopen" && phase != "full-lifecycle" {
                return Err(format!("manifest: `phase` must be insert-reopen or full-lifecycle, not `{phase}`"));
            }
            Ok(ProofManifest {
                scratch_root,
                output_root,
                candidate_cli: PathBuf::from(required_str(cli, "path")?),
                candidate_sha256: required_str(cli, "sha256")?,
                protected_paths,
                sources,
                parts,
                checkpoints,
                phase,
            })
        }
    }

    /// Why `output_root` must not be written to, when it may not: it is one of the protected
    /// paths the manifest names, or it lies outside the fresh scratch root the manifest is in.
    fn output_refusal(output_root: &Path, scratch_root: &Path, protected: &[PathBuf]) -> Option<String> {
        let out = absolute(output_root);
        if let Some(p) = protected.iter().map(|p| absolute(p)).find(|p| out == *p || out.starts_with(p)) {
            return Some(format!("refused: output root {} is protected path {}", out.display(), p.display()));
        }
        let scratch = absolute(scratch_root);
        if !out.starts_with(&scratch) {
            return Some(format!("refused: output root {} is not inside the fresh scratch root {}", out.display(), scratch.display()));
        }
        None
    }

    fn verify_digest(path: &Path, expected: &str) -> std::result::Result<(), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("{}: sha256 {actual} != expected {expected}", path.display()));
        }
        Ok(())
    }

    /// Every guard, all read-only: output containment/protection, disposable copies inside the
    /// scratch root with matching digests, candidate identity. Runs before any write.
    fn guard_manifest(m: &ProofManifest) -> std::result::Result<(), String> {
        if let Some(reason) = output_refusal(&m.output_root, &m.scratch_root, &m.protected_paths) {
            return Err(reason);
        }
        let scratch = absolute(&m.scratch_root);
        if m.sources.is_empty() {
            return Err("manifest: `sources` names no disposable copies".into());
        }
        for s in &m.sources {
            let path = absolute(&s.path);
            if !path.starts_with(&scratch) {
                return Err(format!("refused: source {} is not a disposable copy inside {}", path.display(), scratch.display()));
            }
            verify_digest(&s.path, &s.sha256)?;
        }
        for c in &m.checkpoints {
            let path = absolute(&c.path);
            if !path.starts_with(&scratch) {
                return Err(format!("refused: checkpoint {} is outside {}", path.display(), scratch.display()));
            }
        }
        verify_digest(&m.candidate_cli, &m.candidate_sha256).map_err(|e| format!("candidate CLI: {e}"))?;
        Ok(())
    }

    /// One expected body of the open design, checked by `Body::validity()`, face count and
    /// measure, keyed by name.
    fn part_facts(s: &Session, part: &ProofPart) -> std::result::Result<String, String> {
        let st = s.model.state();
        let named: Vec<_> = st.bodies.iter().filter(|b| b.name == part.name).collect();
        if named.len() != 1 {
            let names: Vec<&str> = st.bodies.iter().map(|b| b.name.as_str()).collect();
            return Err(format!("part `{}`: {} bodies named so; the design has {names:?}", part.name, named.len()));
        }
        let body = &named[0].body;
        let validity = body.validity();
        if !validity.is_empty() {
            return Err(format!("part `{}`: not a sound solid: {validity:?}", part.name));
        }
        if body.face_count() != part.expected_faces {
            return Err(format!("part `{}`: {} faces, expected {}", part.name, body.face_count(), part.expected_faces));
        }
        let m = solidcraft_kernel::measure(body).map_err(|e| format!("part `{}`: {e}", part.name))?;
        if let Some(want) = part.expected_volume_mm3
            && !(m.volume.is_finite() && (m.volume - want).abs() <= part.tolerance * want.abs().max(1.0))
        {
            return Err(format!("part `{}`: volume {} != {want} (tolerance {})", part.name, m.volume, part.tolerance));
        }
        Ok(format!("part `{}`: {} faces, volume {:.6} mm3, ok", part.name, body.face_count(), m.volume))
    }

    /// Read and guard a manifest, then run its phase and return the report text. Nothing is
    /// written before `guard_manifest` returns `Ok`.
    fn run_proof(manifest_path: &Path) -> std::result::Result<String, String> {
        let bytes = std::fs::read(manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: not a JSON manifest: {e}", manifest_path.display()))?;
        let m = ProofManifest::parse(&value, manifest_path)?;
        guard_manifest(&m)?;
        std::fs::create_dir_all(&m.output_root).map_err(|e| format!("{}: {e}", m.output_root.display()))?;
        let mut lines = vec![format!("phase {}", m.phase)];
        if m.phase == "insert-reopen" {
            // Build the linked host from the disposable copies through the shared link path,
            // save it into the output root, reopen it fresh and refresh the links explicitly.
            let host = m.output_root.join("linked-host.solidcraft");
            let mut s = Session::default();
            for src in &m.sources {
                let part =
                    m.parts.iter().find(|p| p.name == src.name).ok_or_else(|| format!("manifest: source `{}` has no matching part", src.name))?;
                s.execute("component.insert_design", &json!({"path": src.path, "name": part.name}))
                    .map_err(|e| format!("insert `{}`: {e}", part.name))?;
            }
            s.execute("file.save", &json!({"path": host})).map_err(|e| format!("save {}: {e}", host.display()))?;
            let mut r = Session::default();
            r.execute("doc.open", &json!({"path": host})).map_err(|e| format!("reopen {}: {e}", host.display()))?;
            r.execute("component.update_links", &json!({})).map_err(|e| format!("update links: {e}"))?;
            for part in &m.parts {
                lines.push(part_facts(&r, part)?);
            }
        } else {
            if m.checkpoints.is_empty() {
                return Err("full-lifecycle phase needs at least one `checkpoints` entry".to_string());
            }
            for c in &m.checkpoints {
                let mut s = Session::default();
                s.execute("doc.open", &json!({"path": c.path})).map_err(|e| format!("open {}: {e}", c.path.display()))?;
                for part in &c.parts {
                    lines.push(part_facts(&s, part)?);
                }
            }
        }
        let report = lines.join("\n");
        std::fs::write(m.output_root.join("proof-report.txt"), &report).map_err(|e| format!("report: {e}"))?;
        Ok(report)
    }

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn rejects_protected_output_paths() {
        let scratch = absolute(Path::new("/tmp/solidcraft-scratch-S"));
        let master = absolute(Path::new("/tmp/solidcraft-owner/lagerblock.solvecraft"));
        let protected = [master.clone()];
        // An owner file or folder can never be the output root.
        assert!(output_refusal(&master, &scratch, &protected).is_some());
        assert!(output_refusal(&master.join("checkpoints"), &scratch, &protected).is_some());
        // Neither can anything outside the fresh scratch root.
        assert!(output_refusal(Path::new("/tmp/solidcraft-elsewhere"), &scratch, &[]).is_some());
        // Inside the scratch root is allowed.
        assert!(output_refusal(&scratch.join("checkpoints"), &scratch, &protected).is_none());
        assert!(output_refusal(&scratch, &scratch, &protected).is_none());
    }

    #[test]
    fn native_link_proof_harness_refuses_before_writing() {
        let dir = std::env::temp_dir().join(format!("solidcraft-native-proof-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A manifest path that does not exist is refused.
        assert!(run_proof(&dir.join("absent.json")).is_err());
        let manifest = dir.join("proof-manifest.json");
        let outside = dir.parent().map(|p| p.join("solidcraft-native-proof-outside")).unwrap();
        let _ = std::fs::remove_dir_all(&outside);
        let write = |value: &Value| std::fs::write(&manifest, serde_json::to_vec(value).unwrap()).unwrap();
        // Output root outside the scratch root (the manifest's own folder).
        write(&json!({
            "candidate_cli": {"path": "x", "sha256": "0"},
            "output_root": outside.to_string_lossy(), "sources": [], "parts": [], "phase": "insert-reopen"
        }));
        let err = run_proof(&manifest).unwrap_err();
        assert!(err.contains("not inside the fresh scratch root"), "{err}");
        assert!(!outside.exists(), "a refused manifest wrote something");
        // Output root listed as a protected path.
        let guarded = dir.join("protected-master");
        write(&json!({
            "candidate_cli": {"path": "x", "sha256": "0"},
            "output_root": guarded.to_string_lossy(), "protected_paths": [guarded.to_string_lossy()],
            "sources": [], "parts": [], "phase": "insert-reopen"
        }));
        let err = run_proof(&manifest).unwrap_err();
        assert!(err.contains("protected path"), "{err}");
        assert!(!guarded.exists(), "a refused manifest wrote something");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Proves the harness machinery end to end on synthetic data inside a temp scratch: two
    /// disposable revolved copies, verified digests, an insert/reopen/refresh and the G checks.
    /// It never reads the environment or an owner file.
    #[test]
    fn native_link_proof_harness_synthetic_dry_run() {
        let dir = std::env::temp_dir().join(format!("solidcraft-native-proof-dry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let accepted = 50.0 * 1.5e-9;
        let h = 5.0 / 0.3_f64.tan();
        let cases: [(&str, [[f64; 2]; 4]); 2] =
            [("RevA", [[10.0, 0.0], [10.0 + accepted, 50.0], [20.0, 50.0], [20.0, 0.0]]), ("RevB", [[10.0, 0.0], [15.0, h], [20.0, h], [20.0, 0.0]])];
        let mut sources = Vec::new();
        for (name, points) in cases {
            let path = dir.join(format!("{name}.solidcraft"));
            save_revolved(&path, name, &points);
            sources.push(json!({"name": name, "path": path.to_string_lossy(), "sha256": sha256_hex(&std::fs::read(&path).unwrap())}));
        }
        let candidate = dir.join("candidate-cli");
        std::fs::write(&candidate, b"synthetic candidate").unwrap();
        let (va, _) = shell_of_revolution(10.0, 10.0 + accepted, 20.0, 50.0);
        let (vb, _) = shell_of_revolution(10.0, 15.0, 20.0, h);
        let manifest = json!({
            "candidate_cli": {"path": candidate.to_string_lossy(), "sha256": sha256_hex(b"synthetic candidate")},
            "output_root": dir.join("out").to_string_lossy(),
            "protected_paths": [dir.join("protected-master").to_string_lossy()],
            "sources": sources,
            "parts": [
                {"name": "RevA", "expected_faces": 4, "expected_volume_mm3": va, "tolerance": 1e-3},
                {"name": "RevB", "expected_faces": 4, "expected_volume_mm3": vb, "tolerance": 1e-3},
            ],
            "phase": "insert-reopen"
        });
        let manifest_path = dir.join("proof-manifest.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let report = run_proof(&manifest_path).unwrap_or_else(|e| panic!("synthetic dry run refused: {e}"));
        assert!(report.contains("part `RevA`: 4 faces"), "{report}");
        assert!(report.contains("part `RevB`: 4 faces"), "{report}");
        assert!(dir.join("out").join("proof-report.txt").is_file(), "{report}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Opt-in actual-part proof. It runs only with `--ignored` and an explicit manifest, and
    /// fails (writing nothing) without one. The real manifest and owner-part runs belong to
    /// tasks 5-6; this test is the runtime entry point for them.
    #[test]
    #[ignore = "opt-in: needs SOLIDCRAFT_NATIVE_LINK_PROOF_MANIFEST and disposable owner-part copies"]
    fn native_link_actual_parts_proof() {
        let path = match std::env::var("SOLIDCRAFT_NATIVE_LINK_PROOF_MANIFEST") {
            Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
            _ => panic!(
                "SOLIDCRAFT_NATIVE_LINK_PROOF_MANIFEST is not set: the actual-part harness runs only with an explicit manifest and wrote nothing"
            ),
        };
        match run_proof(&path) {
            Ok(report) => println!("{report}"),
            Err(e) => panic!("native-link actual-part proof refused: {e}"),
        }
    }
}
